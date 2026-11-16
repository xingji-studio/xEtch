//! Locked, identifiable staging for one output. Recovery never sweeps filenames.
use crate::{workspace::sync_dir, Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize, PartialEq)]
struct Intent {
    schema_version: u32,
    purpose: String,
    output: String,
    nonce: String,
}
fn read_intent(path: &Path) -> Result<Intent> {
    let file = File::open(path)?;
    if file.metadata()?.len() > 4096 {
        return Err(Error::Invalid("output intent exceeds 4 KiB".into()));
    }
    Ok(serde_json::from_reader(file)?)
}

pub(crate) struct OutputStage {
    root: PathBuf,
    lock: Option<File>,
}

impl OutputStage {
    pub fn new(output: &Path) -> Result<Self> {
        let name = output
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::Invalid("output needs a UTF-8 filename".into()))?;
        let parent = fs::canonicalize(
            output
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        let root = parent.join(format!(
            ".xetch-stage-{}",
            &format!("{:x}", Sha256::digest(name.as_bytes()))[..32]
        ));
        let exists = output.try_exists()? || fs::symlink_metadata(output).is_ok();
        let fresh = match fs::create_dir(&root) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e.into()),
        };
        if fresh && exists {
            fs::remove_dir(&root)?;
            return Err(Error::Conflict(
                "output already exists; choose a new output path".into(),
            ));
        }
        if fs::symlink_metadata(&root)?.file_type().is_symlink() || !root.is_dir() {
            return Err(Error::Conflict(
                "output staging path is not a regular directory".into(),
            ));
        }
        let owner = root.join("intent.json");
        // Read the nonce before opening the lock, then recheck after locking.
        // Windows byte-range locks prohibit reading through another handle to
        // the locked file, so metadata and locking use separate files.
        let observed = if fresh {
            None
        } else {
            Some(read_intent(&owner)?)
        };
        let lock_path = root.join("lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        if fresh {
            options.create_new(true);
        }
        let lock = options
            .open(&lock_path)
            .map_err(|e| Error::Conflict(format!("cannot open output staging intent: {e}")))?;
        lock.try_lock()
            .map_err(|e| Error::Conflict(format!("output is busy: {e}")))?;
        let mut stage = Self { root, lock: None };
        if fresh {
            let intent = Intent {
                schema_version: 2,
                purpose: "xetch-output-staging".into(),
                output: name.into(),
                nonce: uuid::Uuid::new_v4().to_string(),
            };
            let write = (|| -> Result<()> {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&owner)?;
                serde_json::to_writer(&mut file, &intent)?;
                file.write_all(b"\n")?;
                file.sync_all()?;
                lock.sync_all()?;
                sync_dir(&stage.root)?;
                sync_dir(&parent)
            })();
            if let Err(error) = write {
                let _ = fs::remove_file(&owner);
                let _ = fs::remove_file(&lock_path);
                let _ = fs::remove_dir(&stage.root);
                return Err(error);
            }
        } else {
            let intent = observed.unwrap();
            if intent.schema_version != 2
                || intent.purpose != "xetch-output-staging"
                || intent.output != name
            {
                return Err(Error::Conflict(
                    "unrecognized output staging intent; no files removed".into(),
                ));
            }
            // A concurrently deleted/recreated directory must not be mistaken
            // for the directory belonging to the handle we just locked.
            let current = read_intent(&owner)?;
            if current != intent {
                return Err(Error::Conflict(
                    "output staging changed while acquiring its lock".into(),
                ));
            }
        }
        stage.validate_contents()?;
        stage.lock = Some(lock);
        stage.clear_data()?;
        if exists {
            return Err(Error::Conflict(
                "output already published; inspect it before retrying".into(),
            ));
        }
        crate::validation::checkpoint("output.ready", &stage.root)?;
        Ok(stage)
    }

    fn validate_contents(&self) -> Result<()> {
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !matches!(
                entry.file_name().to_str(),
                Some("intent.json" | "lock" | "raw.img" | "converted")
            ) || !entry.file_type()?.is_file()
            {
                return Err(Error::Conflict(
                    "unknown file in output staging; recovery preserved it".into(),
                ));
            }
        }
        Ok(())
    }

    fn clear_data(&self) -> Result<()> {
        self.validate_contents()?;
        for name in ["raw.img", "converted"] {
            match fs::remove_file(self.root.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        sync_dir(&self.root)
    }

    pub fn file(&self, name: &str) -> Result<tempfile::NamedTempFile> {
        if !matches!(name, "raw.img" | "converted") {
            return Err(Error::Invalid("invalid staging filename".into()));
        }
        Ok(tempfile::Builder::new()
            .prefix(name)
            .rand_bytes(0)
            .tempfile_in(&self.root)?)
    }

    pub fn finish(mut self) -> Result<()> {
        self.cleanup()
    }

    fn cleanup(&mut self) -> Result<()> {
        if self.lock.is_none() {
            return Ok(());
        }
        self.clear_data()?;
        // Keep the lock until the directory is gone. A stale opened handle
        // checks its nonce against the current intent before touching data.
        fs::remove_file(self.root.join("intent.json"))?;
        fs::remove_file(self.root.join("lock"))?;
        fs::remove_dir(&self.root)?;
        self.lock.take();
        sync_dir(self.root.parent().unwrap())
    }
}

impl Drop for OutputStage {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
