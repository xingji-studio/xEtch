use crate::{
    files,
    nbd::{Disk, Window},
    partition,
    staging::OutputStage,
    tools,
    workspace::{sync_dir, Change, Workspace},
    Error, Result,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageParams {
    image: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceParams {
    workspace: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceCreate {
    image: PathBuf,
    workspace: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateParams {
    output: PathBuf,
    size: u64,
    #[serde(default = "default_table")]
    table: String,
    #[serde(default)]
    format: Option<String>,
    #[serde(default = "default_filesystem")]
    filesystem: String,
    #[serde(default)]
    experimental_filesystems: bool,
}
fn default_filesystem() -> String {
    "fat32".into()
}
fn default_table() -> String {
    "gpt".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConvertParams {
    image: PathBuf,
    output: PathBuf,
    #[serde(default)]
    format: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyParams {
    workspace: PathBuf,
    output: PathBuf,
    #[serde(default)]
    format: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadParams {
    #[serde(default)]
    image: Option<PathBuf>,
    #[serde(default)]
    workspace: Option<PathBuf>,
    #[serde(default)]
    partition: Option<u32>,
    #[serde(default = "root_path")]
    path: String,
    #[serde(default)]
    destination: Option<PathBuf>,
}
fn root_path() -> String {
    "/".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MutateParams {
    workspace: PathBuf,
    #[serde(default)]
    partition: Option<u32>,
    path: String,
    #[serde(default)]
    source: Option<PathBuf>,
    #[serde(default)]
    destination: Option<String>,
    #[serde(default)]
    replace: bool,
    #[serde(default)]
    experimental_filesystems: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SectorParams {
    image: PathBuf,
    offset: u64,
    length: u32,
}

fn params<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| Error::Invalid(e.to_string()))
}

pub fn execute(method: &str, value: Value) -> Result<Value> {
    match method {
        "system.capabilities" => Ok(tools::doctor()),
        "image.inspect" => {
            let p: ImageParams = params(value)?;
            let info = tools::info(&p.image, false)?;
            let mut disk = Disk::open(&info.path, &info.format, false)?;
            let layout = partition::read(&mut disk, info.virtual_size)?;
            Ok(json!({"image":info,"layout":layout}))
        }
        "image.create" => {
            let p: CreateParams = params(value)?;
            if !matches!(p.filesystem.as_str(), "fat32" | "exfat" | "ext4") {
                return Err(Error::Unsupported(
                    "creation supports fat32 or experimental exfat/ext4".into(),
                ));
            }
            if p.filesystem != "fat32" && !p.experimental_filesystems {
                return Err(Error::Unsupported(format!(
                    "{} formatting requires --experimental-filesystems",
                    p.filesystem
                )));
            }
            if p.size < 64 * 1024 * 1024 || !p.size.is_multiple_of(512) {
                return Err(Error::Invalid(
                    "image size must be at least 64 MiB and a multiple of 512 bytes".into(),
                ));
            }
            output_format(&p.output, p.format.as_deref())?;
            let stage = OutputStage::new(&p.output)?;
            let mut raw = stage.file("raw.img")?;
            raw.as_file().set_len(p.size)?;
            crate::validation::checkpoint("create.allocated", raw.path())?;
            let (offset, size) =
                partition::write_table(raw.as_file_mut(), p.size, &p.table, &p.filesystem)?;
            {
                let mut volume = Window::new(raw.as_file_mut(), offset, size);
                match p.filesystem.as_str() {
                    "exfat" => crate::extended::format_exfat(&mut volume, offset)?,
                    "ext4" => crate::extended::format_ext4(&mut volume)?,
                    _ => fatfs::format_volume(
                        &mut volume,
                        fatfs::FormatVolumeOptions::new()
                            .fat_type(fatfs::FatType::Fat32)
                            .volume_label(*b"XETCH      "),
                    )?,
                }
                volume.flush()?;
            }
            raw.as_file().sync_all()?;
            crate::validation::checkpoint("create.formatted", raw.path())?;
            publish_staged(
                &stage,
                raw.path(),
                "raw",
                &p.output,
                p.format.as_deref(),
                p.size,
                || Ok(()),
            )?;
            drop(raw);
            stage
                .finish()
                .map_err(|e| Error::CommitUncertain(e.to_string()))?;
            Ok(
                json!({"output":p.output,"size":p.size,"table":p.table,"filesystem":match p.filesystem.as_str() {"exfat"=>"exFAT","ext4"=>"ext4",_=>"FAT32"}}),
            )
        }
        "image.convert" => {
            let p: ConvertParams = params(value)?;
            let info = tools::info(&p.image, false)?;
            publish(
                &info.path,
                &info.format,
                &p.output,
                p.format.as_deref(),
                info.virtual_size,
            )?;
            Ok(json!({"output":p.output}))
        }
        "workspace.create" => {
            let p: WorkspaceCreate = params(value)?;
            let workspace = Workspace::create(&p.image, &p.workspace)?;
            Ok(json!({"workspace":workspace.root,"manifest":workspace.manifest}))
        }
        "workspace.plan" => {
            let p: WorkspaceParams = params(value)?;
            let workspace = Workspace::open(&p.workspace)?;
            Ok(json!({"workspace":workspace.root,"manifest":workspace.manifest}))
        }
        "workspace.apply" => {
            let p: ApplyParams = params(value)?;
            let workspace = Workspace::open(&p.workspace)?;
            publish_checked(
                &workspace.head(),
                "qcow2",
                &p.output,
                p.format.as_deref(),
                workspace.manifest.source.virtual_size,
                || workspace.verify_source(),
            )?;
            Ok(json!({"output":p.output,"operations":workspace.manifest.changes.len()}))
        }
        "file.list" | "file.get" => {
            let p: ReadParams = params(value)?;
            with_input(&p, |disk| {
                if method == "file.list" {
                    return Ok(json!({"entries":files::list(disk,p.partition,&p.path)?}));
                }
                let output = p
                    .destination
                    .as_deref()
                    .ok_or_else(|| Error::Invalid("destination is required".into()))?;
                let stage = OutputStage::new(output)?;
                let mut temp = stage.file("converted")?;
                let bytes = files::get(disk, p.partition, &p.path, temp.as_file_mut())?;
                temp.as_file().sync_all()?;
                crate::validation::checkpoint("file_get.copied", temp.path())?;
                commit_output(temp, output)?;
                stage
                    .finish()
                    .map_err(|e| Error::CommitUncertain(e.to_string()))?;
                Ok(json!({"destination":output,"bytes":bytes}))
            })
        }
        "file.put" | "file.mkdir" | "file.remove" | "file.rename" => {
            let p: MutateParams = params(value)?;
            let mut workspace = Workspace::open(&p.workspace)?;
            let filesystem = files::check_write(
                &mut workspace.disk()?,
                p.partition,
                p.experimental_filesystems,
            )?;
            // Imported bytes are captured before the transaction, so a caller
            // cannot accidentally read from the overlay currently being edited.
            let mut upload = if method == "file.put" {
                let source = tools::regular(
                    p.source
                        .as_deref()
                        .ok_or_else(|| Error::Invalid("source is required".into()))?,
                )?;
                let mut source = File::open(source)?;
                if filesystem.starts_with("FAT") && source.metadata()?.len() > u64::from(u32::MAX) {
                    return Err(Error::Unsupported(
                        "FAT files cannot exceed 4 GiB minus one byte".into(),
                    ));
                }
                let mut capture = tempfile::tempfile()?;
                std::io::copy(&mut source, &mut capture)?;
                capture.seek(SeekFrom::Start(0))?;
                Some(capture)
            } else {
                None
            };
            let change = Change {
                operation: method.into(),
                partition: p.partition,
                path: p.path.clone(),
                destination: p.destination.clone(),
                experimental_filesystem: if filesystem.starts_with("FAT") || filesystem == "NTFS" {
                    None
                } else {
                    Some(filesystem)
                },
            };
            workspace.mutate(change, |disk| match method {
                "file.put" => files::put(
                    disk,
                    p.partition,
                    &p.path,
                    upload.as_mut().unwrap(),
                    p.replace,
                ),
                "file.mkdir" => files::mkdir(disk, p.partition, &p.path),
                "file.remove" => files::remove(disk, p.partition, &p.path),
                _ => files::rename(
                    disk,
                    p.partition,
                    &p.path,
                    p.destination
                        .as_deref()
                        .ok_or_else(|| Error::Invalid("destination is required".into()))?,
                ),
            })?;
            Ok(json!({"workspace":workspace.root,"revision":workspace.manifest.changes.len()}))
        }
        "sector.read" => {
            let p: SectorParams = params(value)?;
            if p.length == 0 || p.length > 65536 {
                return Err(Error::Invalid(
                    "sector read length must be 1..65536 bytes".into(),
                ));
            }
            let info = tools::info(&p.image, false)?;
            if p.offset
                .checked_add(u64::from(p.length))
                .is_none_or(|end| end > info.virtual_size)
            {
                return Err(Error::Invalid("read outside image".into()));
            }
            let mut disk = Disk::open(&info.path, &info.format, false)?;
            disk.seek(SeekFrom::Start(p.offset))?;
            let mut buf = vec![0; p.length as usize];
            disk.read_exact(&mut buf)?;
            let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
            Ok(json!({"offset":p.offset,"length":p.length,"hex":hex}))
        }
        _ => Err(Error::Unsupported(format!("unknown method {method}"))),
    }
}

fn with_input<T, F>(p: &ReadParams, operation: F) -> Result<T>
where
    F: FnOnce(&mut Disk) -> Result<T>,
{
    match (&p.image, &p.workspace) {
        (Some(image), None) => {
            let info = tools::info(image, false)?;
            operation(&mut Disk::open(&info.path, &info.format, false)?)
        }
        (None, Some(root)) => {
            let workspace = Workspace::open(root)?;
            operation(&mut workspace.disk()?)
        }
        _ => Err(Error::Invalid(
            "provide exactly one of image or workspace".into(),
        )),
    }
}
fn output_format(output: &Path, format: Option<&str>) -> Result<&'static str> {
    match format.unwrap_or_else(|| {
        if output
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("vmdk"))
        {
            "vmdk"
        } else {
            "raw"
        }
    }) {
        "raw" | "img" => Ok("raw"),
        "vmdk" => Ok("vmdk"),
        _ => Err(Error::Unsupported(
            "output format must be raw or vmdk (monolithicSparse)".into(),
        )),
    }
}
fn publish(
    source: &Path,
    source_format: &str,
    output: &Path,
    format: Option<&str>,
    size: u64,
) -> Result<()> {
    publish_checked(source, source_format, output, format, size, || Ok(()))
}
fn publish_checked<F: FnOnce() -> Result<()>>(
    source: &Path,
    source_format: &str,
    output: &Path,
    format: Option<&str>,
    size: u64,
    verify: F,
) -> Result<()> {
    let stage = OutputStage::new(output)?;
    publish_staged(&stage, source, source_format, output, format, size, verify)?;
    stage
        .finish()
        .map_err(|e| Error::CommitUncertain(e.to_string()))
}
#[allow(clippy::too_many_arguments)]
fn publish_staged<F: FnOnce() -> Result<()>>(
    stage: &OutputStage,
    source: &Path,
    source_format: &str,
    output: &Path,
    format: Option<&str>,
    size: u64,
    verify: F,
) -> Result<()> {
    let format = output_format(output, format)?;
    let temp = stage.file("converted")?;
    let mut args = vec![
        "convert".into(),
        "-f".into(),
        source_format.into(),
        "-O".into(),
        format.into(),
    ];
    if format == "vmdk" {
        args.extend(["-o".into(), "subformat=monolithicSparse".into()]);
    }
    args.extend([source.as_os_str().into(), temp.path().as_os_str().into()]);
    tools::command(&args)?;
    crate::validation::checkpoint("publish.converted", temp.path())?;
    let info = tools::info(temp.path(), false)?;
    if info.virtual_size != size || info.format != format {
        return Err(Error::Invalid("export validation failed".into()));
    }
    temp.as_file().sync_all()?;
    verify()?;
    commit_output(temp, output)
}
fn commit_output(temp: tempfile::NamedTempFile, output: &Path) -> Result<()> {
    let parent = fs::canonicalize(
        output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    crate::validation::checkpoint("publish.before_publish", temp.path())?;
    temp.persist_noclobber(output).map_err(|e| e.error)?;
    (|| -> Result<()> {
        crate::validation::checkpoint("publish.published", output)?;
        sync_dir(&parent)?;
        crate::validation::checkpoint("publish.directory_synced", output)
    })()
    .map_err(|e| Error::CommitUncertain(e.to_string()))?;
    Ok(())
}
