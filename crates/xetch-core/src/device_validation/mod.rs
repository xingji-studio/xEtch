//! D1 laboratory only. Compiled solely with validation-hooks, never dispatched by image RPC.
//! Targets must be newly created owned regular images mapped as loop/VHD devices.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use uuid::Uuid;
#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows.rs"]
mod platform;

const SIZE: u64 = 128 * 1024 * 1024;
const BLOCK: usize = 4096;
const MAX_CHANGED: usize = 2 * 1024 * 1024;
const PURPOSE: &str = "xetch-disposable-device-validation-v1";
const MARKER: &str = ".xetch-device-validation.json";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    purpose: String,
    version: u32,
    token: String,
    image_name: String,
    image_identity: String,
    size: u64,
    partition_offset: u64,
    partition_size: u64,
    prefix_sha256: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    offset: u64,
    old: Vec<u8>,
    new: Vec<u8>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalBody {
    version: u32,
    fixture_token: String,
    target_id: String,
    transaction_id: String,
    plan_id: String,
    plan_hash: String,
    partition_size: u64,
    patches: Vec<Patch>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    sha256: String,
    body: JournalBody,
}
struct Plan {
    id: String,
    hash: String,
    phrase: String,
    expires: Instant,
    patches: Vec<Patch>,
}
struct Authorization {
    token: String,
    plan_id: String,
}

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn conflict(message: &str) -> Error {
    Error::Conflict(message.into())
}
fn hash(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}
fn file_hash(file: &mut File, offset: u64, len: u64) -> Result<String> {
    file.seek(SeekFrom::Start(offset))?;
    let mut remaining = len;
    let mut sha = Sha256::new();
    let mut storage = vec![0; 1024 * 1024 + 4096];
    let start = storage.as_ptr().align_offset(4096);
    let block = &mut storage[start..start + 1024 * 1024];
    while remaining > 0 {
        let count = remaining.min(block.len() as u64) as usize;
        file.read_exact(&mut block[..count])?;
        sha.update(&block[..count]);
        remaining -= count as u64;
    }
    Ok(format!("{:x}", sha.finalize()))
}
fn plain_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || platform::reparse(&metadata) {
        return Err(invalid("a non-linked regular validation file is required"));
    }
    platform::file_identity(&file)?;
    Ok(file)
}

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn create_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    crate::workspace::sync_dir(path.parent().unwrap())
}
fn load_fixture(root: &Path) -> Result<Fixture> {
    let m = fs::symlink_metadata(root)?;
    if !m.is_dir() || platform::reparse(&m) {
        return Err(invalid("validation root must be a real directory"));
    }
    let mut marker = plain_file(&root.join(MARKER))?;
    if marker.metadata()?.len() > 16384 {
        return Err(invalid("fixture marker is too large"));
    }
    let mut data = Vec::new();
    marker.read_to_end(&mut data)?;
    let f: Fixture = serde_json::from_slice(&data)?;
    if f.purpose != PURPOSE
        || f.version != 1
        || Uuid::parse_str(&f.token).is_err()
        || f.size != SIZE
        || f.partition_offset != 1048576
        || f.partition_size != SIZE - 1048576
        || f.prefix_sha256.len() != 64
        || f.image_name != platform::IMAGE_NAME
    {
        return Err(invalid("fixture ownership, geometry or format is invalid"));
    }
    let image = plain_file(&root.join(&f.image_name))?;
    if platform::file_identity(&image)? != f.image_identity {
        return Err(conflict("fixture backing file was replaced or linked"));
    }
    Ok(f)
}
/// Creates a fresh bounded image. Never accepts an existing root or device path.
pub fn create(root: &Path) -> Result<Value> {
    private_dir(root)?;
    let source = root.join("original.img");
    let mut raw = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&source)?;
    raw.set_len(SIZE)?;
    let (offset, size) = crate::partition::write_table(&mut raw, SIZE, "mbr", "fat32")?;
    // Windows initializes a zero MBR disk signature on first attachment. Give
    // this fresh disposable disk its identity before sealing the protected prefix.
    let signature = u32::from_le_bytes(Uuid::new_v4().as_bytes()[..4].try_into().unwrap()) | 1;
    raw.seek(SeekFrom::Start(440))?;
    raw.write_all(&signature.to_le_bytes())?;

    {
        let mut window = crate::nbd::Window::new(&mut raw, offset, size);
        fatfs::format_volume(
            &mut window,
            fatfs::FormatVolumeOptions::new()
                .fat_type(fatfs::FatType::Fat32)
                .volume_label(*b"XETCH D1   "),
        )?;
        window.flush()?;
    }
    raw.sync_all()?;
    let prefix_sha256 = file_hash(&mut raw, 0, offset)?;
    platform::create_image(&source, &root.join(platform::IMAGE_NAME), SIZE)?;
    let image = plain_file(&root.join(platform::IMAGE_NAME))?;
    let f = Fixture {
        purpose: PURPOSE.into(),
        version: 1,
        token: Uuid::new_v4().to_string(),
        image_name: platform::IMAGE_NAME.into(),
        image_identity: platform::file_identity(&image)?,
        size: SIZE,
        partition_offset: offset,
        partition_size: size,
        prefix_sha256,
    };
    create_json(&root.join(MARKER), &f)?;
    private_dir(&root.join("recovery"))?;
    Ok(json!({"fixture":f,"root":root,"test_only":true}))
}

/// A process-local lease and one-use consent. No arbitrary physical device is admitted.
pub struct Session {
    root: PathBuf,
    fixture: Fixture,
    lease: platform::Lease,
    target_id: String,
    lease_id: String,
    plan: Option<Plan>,
    authorization: Option<Authorization>,
    target_write_calls: u64,
    fault_consumed: bool,
}
impl Session {
    pub fn open(root: &Path, loop_device: Option<&Path>) -> Result<Self> {
        let fixture = load_fixture(root)?;
        let root = fs::canonicalize(root)?;
        let lease = platform::Lease::open(&root, &fixture, loop_device)?;
        let target_id = hash(&serde_json::to_vec(&fixture)?);
        Ok(Self {
            root,
            fixture,
            lease,
            target_id,
            lease_id: Uuid::new_v4().to_string(),
            plan: None,
            authorization: None,
            target_write_calls: 0,
            fault_consumed: false,
        })
    }
    fn verify(&mut self) -> Result<()> {
        if load_fixture(&self.root)? != self.fixture {
            return Err(conflict(
                "fixture identity or geometry changed; consent is invalid",
            ));
        }
        self.lease.verify(&self.root, &self.fixture)
    }
    fn pending(&self) -> Result<Vec<(PathBuf, Journal)>> {
        let mut result = Vec::new();
        let directory = self.root.join("recovery");
        if platform::reparse(&fs::symlink_metadata(&directory)?) {
            return Err(conflict("recovery directory is linked"));
        }
        let mut count = 0;
        for entry in fs::read_dir(&directory)? {
            count += 1;
            if count > 128 {
                return Err(conflict("too many recovery records; review the fixture"));
            }
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_dir() || platform::reparse(&entry.metadata()?) {
                return Err(conflict("unexpected recovery directory entry"));
            }
            let journal = read_journal(&path, &self.fixture, &self.target_id)?;
            let committed = terminal(&path, "committed", &journal.sha256)?;
            let rolled_back = terminal(&path, "rolled-back", &journal.sha256)?;
            if committed && rolled_back {
                return Err(conflict("conflicting recovery terminal records"));
            }
            if !committed && !rolled_back {
                result.push((path, journal));
            }
        }
        Ok(result)
    }
    pub fn status(&mut self, full_hash: bool) -> Result<Value> {
        self.verify()?;
        let (records, recovery_error) = match self.pending() {
            Ok(records) => (records, None),
            Err(e) => (Vec::new(), Some(e.to_string())),
        };
        let pending = records
            .iter()
            .map(|(_, j)| {
                json!({"transaction_id":j.body.transaction_id,"plan_id":j.body.plan_id,
            "phrase":self.recovery_phrase(&j.body.transaction_id)})
            })
            .collect::<Vec<_>>();
        let editing_blocked = recovery_error.is_some() || !pending.is_empty();
        let digest = if full_hash {
            Some(self.lease.partition_hash(self.fixture.partition_size)?)
        } else {
            None
        };
        Ok(
            json!({"test_only":true,"target_id":self.target_id,"lease_id":self.lease_id,"exclusive":true,"fixture_token":self.fixture.token,
            "partition_offset":self.fixture.partition_offset,"partition_size":self.fixture.partition_size,"target_write_calls":self.target_write_calls,
            "pending_recovery":pending,"recovery_error":recovery_error,"editing_blocked":editing_blocked,"partition_sha256":digest}),
        )
    }
    pub fn snapshot(&mut self) -> Result<Value> {
        self.verify()?;
        let path = self.root.join("baseline.img");
        let mut seed = plain_file(&self.root.join("original.img"))?;
        if seed.metadata()?.len() != self.fixture.size
            || file_hash(&mut seed, 0, self.fixture.partition_offset)? != self.fixture.prefix_sha256
        {
            return Err(conflict("factory seed header changed"));
        }
        seed.seek(SeekFrom::Start(0))?;
        let mut output = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        std::io::copy(&mut seed, &mut output)?;
        let mut buffer = vec![0; 1024 * 1024];
        for offset in (0..self.fixture.partition_size).step_by(buffer.len()) {
            let len = (self.fixture.partition_size - offset).min(buffer.len() as u64) as usize;
            self.lease.read_at(offset, &mut buffer[..len])?;
            output.seek(SeekFrom::Start(self.fixture.partition_offset + offset))?;
            output.write_all(&buffer[..len])?;
        }
        output.sync_all()?;
        Ok(
            json!({"snapshot":path,"target_write_calls":self.target_write_calls,"partition_sha256":self.lease.partition_hash(self.fixture.partition_size)?}),
        )
    }
    pub fn plan(&mut self, name: &str) -> Result<Value> {
        self.plan = None;
        self.authorization = None;
        self.verify()?;
        if !self.pending()?.is_empty() {
            return Err(conflict(
                "unfinished transaction must be recovered before any new edit",
            ));
        }
        if Path::new(name).file_name().and_then(|n| n.to_str()) != Some(name)
            || !name.ends_with(".img")
            || name == "original.img"
        {
            return Err(invalid(
                "plan must name a separate raw .img directly inside this fixture directory",
            ));
        }
        let mut image = plain_file(&self.root.join(name))?;
        if image.metadata()?.len() != self.fixture.size
            || file_hash(&mut image, 0, self.fixture.partition_offset)?
                != self.fixture.prefix_sha256
        {
            return Err(invalid(
                "plan changes the partition table, protected prefix or device geometry",
            ));
        }
        let mut patches = Vec::new();
        let mut changed = 0;
        let mut old = vec![0; BLOCK];
        let mut new = vec![0; BLOCK];
        for offset in (0..self.fixture.partition_size).step_by(BLOCK) {
            let len = (self.fixture.partition_size - offset).min(BLOCK as u64) as usize;
            self.lease.read_at(offset, &mut old[..len])?;
            image.seek(SeekFrom::Start(self.fixture.partition_offset + offset))?;
            image.read_exact(&mut new[..len])?;
            if old[..len] != new[..len] {
                changed += len;
                if changed > MAX_CHANGED {
                    return Err(invalid("D1 plan exceeds the 2 MiB changed-sector bound"));
                }
                patches.push(Patch {
                    offset,
                    old: old[..len].to_vec(),
                    new: new[..len].to_vec(),
                });
            }
        }
        if patches.is_empty() {
            return Err(invalid("plan has no changes"));
        }
        let id = Uuid::new_v4().to_string();
        let hash = hash(&serde_json::to_vec(&(
            &self.target_id,
            &self.lease_id,
            &id,
            &patches,
        ))?);
        let phrase = format!("WRITE TEST {} {}", &self.fixture.token[..8], &id[..8]);
        let ttl = std::env::var("XETCH_DEVICE_TEST_CONFIRM_TTL_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(120000)
            .clamp(1, 120000);
        let result = json!({"test_only":true,"target_id":self.target_id,"target_label":self.root.join(&self.fixture.image_name),"lease_id":self.lease_id,"plan_id":id,"plan_hash":hash,"phrase":phrase,
            "expires_in_ms":ttl,"changed_bytes":changed,"changed_blocks":patches.len(),"partition_offset":self.fixture.partition_offset,"partition_size":self.fixture.partition_size,
            "recovery_directory":self.root.join("recovery"),"warning":"Disposable test device only. Physical writeback is not atomic; interruption may require recovery."});
        self.plan = Some(Plan {
            id,
            hash,
            phrase,
            expires: Instant::now() + Duration::from_millis(ttl),
            patches,
        });
        Ok(result)
    }
    pub fn confirm(&mut self, id: &str, phrase: &str) -> Result<Value> {
        self.authorization = None;
        self.verify()?;
        let p = self
            .plan
            .as_ref()
            .ok_or_else(|| conflict("no current plan"))?;
        if p.id != id || p.phrase != phrase || Instant::now() >= p.expires {
            return Err(conflict("confirmation is incorrect, stale or expired"));
        }
        let token = Uuid::new_v4().to_string();
        self.authorization = Some(Authorization {
            token: token.clone(),
            plan_id: id.into(),
        });
        Ok(json!({"authorization":token,"plan_id":id,"test_only":true}))
    }
    pub fn cancel(&mut self) -> Value {
        self.plan = None;
        self.authorization = None;
        json!({"cancelled":true,"target_write_calls":self.target_write_calls})
    }
    pub fn commit(&mut self, id: &str, token: &str) -> Result<Value> {
        let auth = self
            .authorization
            .take()
            .ok_or_else(|| conflict("explicit, one-use confirmation is required"))?;
        if auth.plan_id != id || auth.token != token {
            return Err(conflict("authorization does not match this plan"));
        }
        self.verify()?;
        if !self.pending()?.is_empty() {
            return Err(conflict("recovery is required"));
        }
        let p = self
            .plan
            .take()
            .ok_or_else(|| conflict("no current plan"))?;
        if p.id != id || Instant::now() >= p.expires {
            return Err(conflict("plan is stale or expired"));
        }
        self.check_old(&p.patches)?;
        let transaction = Uuid::new_v4().to_string();
        let directory = self.root.join("recovery").join(&transaction);
        let body = JournalBody {
            version: 1,
            fixture_token: self.fixture.token.clone(),
            target_id: self.target_id.clone(),
            transaction_id: transaction.clone(),
            plan_id: p.id,
            plan_hash: p.hash,
            partition_size: self.fixture.partition_size,
            patches: p.patches,
        };
        let journal = Journal {
            sha256: hash(&serde_json::to_vec(&body)?),
            body,
        };
        // No target writes before the complete undo and redo records are synced.
        private_dir(&directory)?;
        if std::env::var("XETCH_DEVICE_TEST_FAULT").ok().as_deref() == Some("journal_enospc") {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.join("journal.json"))?;
            file.write_all(b"{\"partial\":")?;
            file.sync_all()?;
            return Err(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "injected test fault: journal_enospc",
            )
            .into());
        }
        create_json(&directory.join("journal.json"), &journal)?;
        crate::workspace::sync_dir(directory.parent().unwrap())?;
        let applied = (|| -> Result<()> {
            crate::validation::checkpoint("device.journal_durable", &directory)?;
            self.verify()?;
            self.check_old(&journal.body.patches)?;
            for (index, patch) in journal.body.patches.iter().enumerate() {
                let before = if index == 0 {
                    "device.before_write".into()
                } else {
                    format!("device.before_write.{}", index + 1)
                };
                let after = if index == 0 {
                    "device.wrote_block".into()
                } else {
                    format!("device.wrote_block.{}", index + 1)
                };
                crate::validation::checkpoint(&before, &directory)?;
                self.write_patch(patch, false)?;
                crate::validation::checkpoint(&after, &directory)?;
            }
            if !self.fault_consumed
                && std::env::var("XETCH_DEVICE_TEST_FAULT").ok().as_deref() == Some("sync_eio")
            {
                self.fault_consumed = true;
                return Err(std::io::Error::other("injected test fault: sync_eio").into());
            }
            self.lease.sync()?;
            crate::validation::checkpoint("device.target_synced", &directory)?;
            self.check_values(&journal.body.patches, false)?;
            create_json(&directory.join("committed"), &journal.sha256)?;
            crate::validation::checkpoint("device.commit_recorded", &directory)?;
            Ok(())
        })();
        if let Err(error) = applied {
            if terminal(&directory, "committed", &journal.sha256)? {
                return Err(Error::CommitUncertain(error.to_string()));
            }
            if let Err(recovery) = self.rollback(&directory, &journal) {
                return Err(Error::CommitUncertain(format!(
                    "{error}; recovery required for {transaction}: {recovery}"
                )));
            }
            return Err(error);
        }
        Ok(
            json!({"committed":true,"transaction_id":transaction,"target_write_calls":self.target_write_calls}),
        )
    }
    fn write_patch(&mut self, patch: &Patch, recovery: bool) -> Result<()> {
        self.target_write_calls += 1;
        let mode = std::env::var("XETCH_DEVICE_TEST_FAULT").unwrap_or_default();
        let selected = if recovery {
            mode == "recovery_write_eio"
        } else {
            matches!(mode.as_str(), "write_eio" | "write_enospc" | "torn_write")
        };
        let data = if recovery { &patch.old } else { &patch.new };
        if !self.fault_consumed && selected {
            self.fault_consumed = true;
            if mode == "torn_write" {
                self.lease.write_at(patch.offset, &data[..data.len() / 2])?;
            }
            return Err(std::io::Error::new(
                if mode == "write_enospc" {
                    std::io::ErrorKind::StorageFull
                } else {
                    std::io::ErrorKind::Other
                },
                format!("injected test fault: {mode}"),
            )
            .into());
        }
        self.lease.write_at(patch.offset, data)
    }
    fn check_old(&mut self, patches: &[Patch]) -> Result<()> {
        self.check_values(patches, true)
    }
    fn check_values(&mut self, patches: &[Patch], old: bool) -> Result<()> {
        for patch in patches {
            let expected = if old { &patch.old } else { &patch.new };
            let mut actual = vec![0; expected.len()];
            self.lease.read_at(patch.offset, &mut actual)?;
            if actual != *expected {
                return Err(conflict("target sector does not match the recorded value; no further editing is allowed"));
            }
        }
        Ok(())
    }
    fn recovery_phrase(&self, id: &str) -> String {
        format!("RECOVER TEST {} {}", &self.fixture.token[..8], &id[..8])
    }
    pub fn recover(&mut self, id: &str, phrase: &str) -> Result<Value> {
        self.plan = None;
        self.authorization = None;
        self.verify()?;
        if Uuid::parse_str(id).is_err() || phrase != self.recovery_phrase(id) {
            return Err(conflict(
                "recovery confirmation does not match this fixture transaction",
            ));
        }
        let pending = self.pending()?;
        let (path, journal) = pending
            .iter()
            .find(|(_, j)| j.body.transaction_id == id)
            .ok_or_else(|| conflict("no matching unfinished transaction"))?;
        self.rollback(path, journal)?;
        Ok(
            json!({"recovered":true,"transaction_id":id,"target_write_calls":self.target_write_calls}),
        )
    }
    fn rollback(&mut self, directory: &Path, journal: &Journal) -> Result<()> {
        self.verify()?;
        // A torn write may contain old/new bytes. Other changes must not be overwritten.
        for patch in &journal.body.patches {
            let mut actual = vec![0; patch.old.len()];
            self.lease.read_at(patch.offset, &mut actual)?;
            if actual
                .iter()
                .zip(patch.old.iter().zip(&patch.new))
                .any(|(v, (old, new))| v != old && v != new)
            {
                return Err(conflict(
                    "foreign or corrupted target bytes require manual read-only inspection",
                ));
            }
        }
        let mut writes = 0;
        for patch in journal.body.patches.iter().rev() {
            let mut actual = vec![0; patch.old.len()];
            self.lease.read_at(patch.offset, &mut actual)?;
            if actual != patch.old {
                self.write_patch(patch, true)?;
                writes += 1;
                let point = if writes == 1 {
                    "device.recovery_wrote_block".into()
                } else {
                    format!("device.recovery_wrote_block.{writes}")
                };
                crate::validation::checkpoint(&point, directory)?;
            }
        }
        self.lease.sync()?;
        self.check_values(&journal.body.patches, true)?;
        crate::validation::checkpoint("device.recovery_synced", directory)?;
        create_json(&directory.join("rolled-back"), &journal.sha256)
    }
    pub fn dispatch(&mut self, method: &str, value: Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Status {
            #[serde(default)]
            hash: bool,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Image {
            image: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Confirm {
            plan_id: String,
            phrase: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Commit {
            plan_id: String,
            authorization: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Recover {
            transaction_id: String,
            phrase: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Empty {}
        fn params<T: serde::de::DeserializeOwned>(v: Value) -> Result<T> {
            serde_json::from_value(v).map_err(|e| Error::Invalid(e.to_string()))
        }
        match method {
            "validation.device.snapshot" => {
                let _: Empty = params(value)?;
                self.snapshot()
            }
            "validation.device.status" => {
                let p: Status = params(value)?;
                self.status(p.hash)
            }
            "validation.device.plan" => {
                let p: Image = params(value)?;
                self.plan(&p.image)
            }
            "validation.device.confirm" => {
                let p: Confirm = params(value)?;
                self.confirm(&p.plan_id, &p.phrase)
            }
            "validation.device.commit" => {
                let p: Commit = params(value)?;
                self.commit(&p.plan_id, &p.authorization)
            }
            "validation.device.cancel" => {
                let _: Empty = params(value)?;
                Ok(self.cancel())
            }
            "validation.device.recover" => {
                let p: Recover = params(value)?;
                self.recover(&p.transaction_id, &p.phrase)
            }
            _ => Err(Error::Unsupported(format!("unknown test method {method}"))),
        }
    }
}
fn terminal(path: &Path, name: &str, expected: &str) -> Result<bool> {
    let p = path.join(name);
    if !p.try_exists()? {
        return Ok(false);
    }
    let mut f = plain_file(&p)?;
    if f.metadata()?.len() > 128 {
        return Err(conflict("terminal record is invalid"));
    }
    let mut data = String::new();
    f.read_to_string(&mut data)?;
    if serde_json::from_str::<String>(&data)? != expected {
        return Err(conflict("terminal record checksum mismatch"));
    }
    Ok(true)
}
fn read_journal(path: &Path, fixture: &Fixture, target: &str) -> Result<Journal> {
    let mut file = plain_file(&path.join("journal.json"))?;
    if file.metadata()?.len() > 32 * 1024 * 1024 {
        return Err(conflict("recovery journal is too large"));
    }
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    let j: Journal = serde_json::from_slice(&data).map_err(|_| {
        conflict("recovery journal is incomplete or corrupt; editing remains blocked")
    })?;
    let b = &j.body;
    if Uuid::parse_str(&b.transaction_id).is_err() || Uuid::parse_str(&b.plan_id).is_err() {
        return Err(conflict("invalid transaction identity"));
    }
    if j.sha256 != hash(&serde_json::to_vec(b)?)
        || b.version != 1
        || b.fixture_token != fixture.token
        || b.target_id != target
        || b.partition_size != fixture.partition_size
        || path.file_name().and_then(|s| s.to_str()) != Some(&b.transaction_id)
        || b.patches.is_empty()
        || b.patches.len() > MAX_CHANGED / BLOCK
    {
        return Err(conflict(
            "recovery journal identity, checksum or bounds are invalid",
        ));
    }
    let mut end = 0;
    let mut total = 0;
    for p in &b.patches {
        if p.offset < end
            || !p.offset.is_multiple_of(BLOCK as u64)
            || p.old.len() != BLOCK
            || p.old.len() != p.new.len()
            || p.old == p.new
        {
            return Err(conflict("recovery patch is invalid"));
        }
        end = p
            .offset
            .checked_add(p.old.len() as u64)
            .ok_or_else(|| conflict("recovery patch overflow"))?;
        if end > fixture.partition_size {
            return Err(conflict("recovery patch escapes the partition"));
        }
        total += p.old.len();
        if total > MAX_CHANGED {
            return Err(conflict("recovery changes exceed their bound"));
        }
    }
    Ok(j)
}
