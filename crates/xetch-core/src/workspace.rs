use crate::{
    nbd::Disk,
    tools::{self, ImageInfo},
    Error, Result,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Fingerprint {
    path: PathBuf,
    size: u64,
    modified_ns: String,
    sha256: String,
}
fn fingerprint(path: &Path) -> Result<Fingerprint> {
    let mut file = File::open(path)?;
    let before = file.metadata()?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    let after = file.metadata()?;
    if before.len() != after.len() || before.modified()? != after.modified()? {
        return Err(Error::Conflict(
            "source changed while fingerprinting".into(),
        ));
    }
    Ok(Fingerprint {
        path: path.into(),
        size: after.len(),
        modified_ns: after
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Invalid("source timestamp predates Unix epoch".into()))?
            .as_nanos()
            .to_string(),
        sha256: format!("{:x}", hash.finalize()),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub operation: String,
    pub partition: Option<u32>,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental_filesystem: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub source: ImageInfo,
    pub fingerprints: Vec<Fingerprint>,
    pub head: String,
    pub changes: Vec<Change>,
}
pub struct Workspace {
    pub root: PathBuf,
    pub manifest: Manifest,
    _lock: File,
}

pub fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
fn write_manifest(root: &Path, manifest: &Manifest) -> Result<()> {
    let mut temp = tempfile::NamedTempFile::new_in(root)?;
    serde_json::to_writer_pretty(temp.as_file_mut(), manifest)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(root.join("workspace.json"))
        .map_err(|e| e.error)?;
    sync_dir(root)
}

impl Workspace {
    pub fn create(source: &Path, root: &Path) -> Result<Self> {
        let image = tools::info(source, false)?;
        let fingerprints = image
            .dependencies
            .iter()
            .map(|p| fingerprint(p))
            .collect::<Result<Vec<_>>>()?;
        // Never use or clean an existing directory: it might contain user data.
        fs::create_dir(root)?;
        let root = fs::canonicalize(root)?;
        let result = (|| {
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(root.join(".lock"))?;
            lock.try_lock()
                .map_err(|e| Error::Conflict(format!("workspace is busy: {e}")))?;
            let head = "base.qcow2".to_string();
            tools::command(&[
                "create".into(),
                "-f".into(),
                "qcow2".into(),
                "-F".into(),
                image.format.clone().into(),
                "-b".into(),
                image.path.as_os_str().into(),
                root.join(&head).into_os_string(),
            ])?;
            OpenOptions::new()
                .write(true)
                .open(root.join(&head))?
                .sync_all()?;
            let manifest = Manifest {
                schema_version: 1,
                source: image,
                fingerprints,
                head,
                changes: vec![],
            };
            let workspace = Self {
                root: root.clone(),
                manifest,
                _lock: lock,
            };
            workspace.verify_source()?;
            write_manifest(&root, &workspace.manifest)?;
            Ok(workspace)
        })();
        // Only remove our own known files if initial creation failed.
        if result.is_err() {
            let _ = fs::remove_file(root.join("base.qcow2"));
            let _ = fs::remove_file(root.join(".lock"));
            let _ = fs::remove_dir(&root);
        }
        result
    }
    pub fn open(root: &Path) -> Result<Self> {
        let root = fs::canonicalize(root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join(".lock"))?;
        lock.try_lock()
            .map_err(|e| Error::Conflict(format!("workspace is busy: {e}")))?;
        let file = File::open(root.join("workspace.json"))?;
        if file.metadata()?.len() > 4 * 1024 * 1024 {
            return Err(Error::Invalid("workspace manifest exceeds 4 MiB".into()));
        }
        let manifest: Manifest = serde_json::from_reader(file)?;
        if manifest.schema_version != 1 {
            return Err(Error::Unsupported("workspace schema version".into()));
        }
        let head = Path::new(&manifest.head);
        if head.components().count() != 1
            || head.file_name() != Some(head.as_os_str())
            || head.extension().is_none_or(|e| e != "qcow2")
        {
            return Err(Error::Invalid("invalid workspace head filename".into()));
        }
        let workspace = Self {
            root,
            manifest,
            _lock: lock,
        };
        workspace.verify_source()?;
        let info = tools::info(&workspace.head(), true)?;
        if info.format != "qcow2" || info.virtual_size != workspace.manifest.source.virtual_size {
            return Err(Error::Invalid(
                "workspace overlay format or size mismatch".into(),
            ));
        }
        Ok(workspace)
    }
    pub fn head(&self) -> PathBuf {
        self.root.join(&self.manifest.head)
    }
    pub fn verify_source(&self) -> Result<()> {
        for expected in &self.manifest.fingerprints {
            if &fingerprint(&expected.path)? != expected {
                return Err(Error::Conflict(format!(
                    "source changed: {}",
                    expected.path.display()
                )));
            }
        }
        Ok(())
    }
    pub fn disk(&self) -> Result<Disk> {
        Disk::open(&self.head(), "qcow2", false)
    }

    /// Each operation writes a fresh child overlay. The manifest head is the
    /// commit point; failed operations and interrupted writes stay unreachable.
    pub fn mutate<F>(&mut self, change: Change, operation: F) -> Result<()>
    where
        F: FnOnce(&mut Disk) -> Result<()>,
    {
        if self.manifest.changes.len() >= 128 {
            return Err(Error::Unsupported(
                "128-operation checkpoint limit; export and open a new workspace".into(),
            ));
        }
        let temp = tempfile::Builder::new()
            .prefix("edit-")
            .suffix(".qcow2")
            .tempfile_in(&self.root)?;
        let path = temp.path().to_path_buf();
        tools::command(&[
            "create".into(),
            "-f".into(),
            "qcow2".into(),
            "-F".into(),
            "qcow2".into(),
            "-b".into(),
            self.head().into_os_string(),
            path.as_os_str().into(),
        ])?;
        {
            let mut disk = Disk::open(&path, "qcow2", true)?;
            operation(&mut disk)?;
            disk.flush()?;
        }
        OpenOptions::new().write(true).open(&path)?.sync_all()?;
        tools::command(&[
            "check".into(),
            "-f".into(),
            "qcow2".into(),
            path.as_os_str().into(),
        ])?;
        self.verify_source()?;
        let previous = self.manifest.head.clone();
        self.manifest.head = path.file_name().unwrap().to_string_lossy().into_owned();
        self.manifest.changes.push(change);
        // Keep the new image before committing its filename to the manifest.
        let (_, kept) = temp.keep().map_err(|e| e.error)?;
        if let Err(e) = write_manifest(&self.root, &self.manifest) {
            self.manifest.head = previous;
            self.manifest.changes.pop();
            // A directory fsync can fail after rename; retain the overlay in
            // case the new manifest is already visible on disk.
            return Err(e);
        }
        let _ = kept;
        Ok(())
    }
}
