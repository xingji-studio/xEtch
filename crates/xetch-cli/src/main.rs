use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, Read, Write},
    path::PathBuf,
};

mod launcher;

#[derive(Parser)]
#[command(
    name = "xetch",
    version,
    about = "Offline IMG/VMDK editor. Edits are saved in a workspace; source images stay unchanged.",
    after_help = "Run without arguments to open the GUI. Use serve --stdio for the backend protocol."
)]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    /// Opt in to experimental ext4/exFAT writes. NTFS writes require the optional NTFS-3G worker.
    #[arg(long, global = true)]
    experimental_filesystems: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Check QEMU dependencies and supported operations.
    Doctor,
    /// Discover host disks/partitions and mandatory protection reasons (read-only).
    Device {
        #[command(subcommand)]
        command: DeviceCommand,
    },
    /// Inspect image format, partitions and filesystems.
    Inspect { image: PathBuf },
    /// Create a FAT32 or experimental exFAT/ext4 image with an optional partition table.
    Create {
        #[arg(long)]
        output: PathBuf,
        #[arg(long,value_parser=parse_size)]
        size: u64,
        #[arg(long,default_value="gpt",value_parser=["none","mbr","gpt"])]
        table: String,
        #[arg(long,value_parser=["raw","vmdk"])]
        format: Option<String>,
        #[arg(long, default_value="fat32", value_parser=["fat32","exfat","ext4"])]
        filesystem: String,
    },
    /// Convert to a NEW raw or single-file sparse VMDK image.
    Convert {
        image: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long,value_parser=["raw","vmdk"])]
        format: Option<String>,
    },
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    /// List successful pending edits.
    Plan {
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Export edits to a NEW image; source is never overwritten.
    Apply {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long,value_parser=["raw","vmdk"])]
        format: Option<String>,
    },
    /// Browse FAT/exFAT/ext4/NTFS; ext4/exFAT writes require --experimental-filesystems.
    File {
        #[command(subcommand)]
        command: FileCommand,
    },
    Sector {
        #[command(subcommand)]
        command: SectorCommand,
    },
    /// Serve newline-delimited JSON-RPC 2.0 on stdin/stdout.
    Serve {
        #[arg(long, required = true)]
        stdio: bool,
    },
}
#[derive(Subcommand)]
enum DeviceCommand {
    List,
    Inspect { id: String },
}
#[derive(Subcommand)]
enum WorkspaceCommand {
    Create {
        image: PathBuf,
        #[arg(long)]
        workspace: PathBuf,
    },
    Info {
        #[arg(long)]
        workspace: PathBuf,
    },
}
#[derive(Args)]
struct Input {
    #[arg(required_unless_present = "workspace", conflicts_with = "workspace")]
    image: Option<PathBuf>,
    #[arg(long)]
    workspace: Option<PathBuf>,
    /// 1-based MBR/GPT index; 0 for a standalone volume.
    #[arg(long)]
    partition: Option<u32>,
}
#[derive(Args)]
struct Edit {
    #[arg(long)]
    workspace: PathBuf,
    #[arg(long)]
    partition: Option<u32>,
}
#[derive(Subcommand)]
enum FileCommand {
    List {
        #[command(flatten)]
        input: Input,
        #[arg(long, default_value = "/")]
        path: String,
    },
    Get {
        #[command(flatten)]
        input: Input,
        #[arg(long)]
        source: String,
        #[arg(long)]
        destination: PathBuf,
    },
    Put {
        #[command(flatten)]
        edit: Edit,
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        destination: String,
        #[arg(long)]
        replace: bool,
    },
    Mkdir {
        #[command(flatten)]
        edit: Edit,
        #[arg(long)]
        path: String,
    },
    Remove {
        #[command(flatten)]
        edit: Edit,
        #[arg(long)]
        path: String,
    },
    Rename {
        #[command(flatten)]
        edit: Edit,
        #[arg(long)]
        source: String,
        #[arg(long)]
        destination: String,
    },
}
#[derive(Subcommand)]
enum SectorCommand {
    Read {
        image: PathBuf,
        #[arg(long)]
        offset: u64,
        #[arg(long, default_value_t = 512)]
        length: u32,
    },
}

fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim().to_ascii_uppercase();
    let units = [
        ("GIB", 1u64 << 30),
        ("MIB", 1 << 20),
        ("KIB", 1 << 10),
        ("G", 1 << 30),
        ("M", 1 << 20),
        ("K", 1 << 10),
        ("B", 1),
    ];
    let (number, multiplier) = units
        .iter()
        .find_map(|(suffix, mul)| s.strip_suffix(suffix).map(|n| (n, *mul)))
        .unwrap_or((&s, 1));
    number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .ok_or_else(|| "invalid size; use bytes or K/M/G, KiB/MiB/GiB".into())
}
fn input_params(i: Input, path: String) -> Value {
    json!({"image":i.image,"workspace":i.workspace,"partition":i.partition,"path":path})
}
fn edit_params(e: Edit, path: String) -> Value {
    json!({"workspace":e.workspace,"partition":e.partition,"path":path})
}
fn request(command: Command) -> (&'static str, Value) {
    match command {
        Command::Doctor => ("system.capabilities", json!({})),
        Command::Device {
            command: DeviceCommand::List,
        } => ("device.list", json!({})),
        Command::Device {
            command: DeviceCommand::Inspect { id },
        } => ("device.inspect", json!({"id":id})),
        Command::Inspect { image } => ("image.inspect", json!({"image":image})),
        Command::Create {
            output,
            size,
            table,
            format,
            filesystem,
        } => (
            "image.create",
            json!({"output":output,"size":size,"table":table,"format":format,"filesystem":filesystem}),
        ),
        Command::Convert {
            image,
            output,
            format,
        } => (
            "image.convert",
            json!({"image":image,"output":output,"format":format}),
        ),
        Command::Workspace {
            command: WorkspaceCommand::Create { image, workspace },
        } => (
            "workspace.create",
            json!({"image":image,"workspace":workspace}),
        ),
        Command::Workspace {
            command: WorkspaceCommand::Info { workspace },
        }
        | Command::Plan { workspace } => ("workspace.plan", json!({"workspace":workspace})),
        Command::Apply {
            workspace,
            output,
            format,
        } => (
            "workspace.apply",
            json!({"workspace":workspace,"output":output,"format":format}),
        ),
        Command::Sector {
            command:
                SectorCommand::Read {
                    image,
                    offset,
                    length,
                },
        } => (
            "sector.read",
            json!({"image":image,"offset":offset,"length":length}),
        ),
        Command::File { command } => match command {
            FileCommand::List { input, path } => ("file.list", input_params(input, path)),
            FileCommand::Get {
                input,
                source,
                destination,
            } => {
                let mut p = input_params(input, source);
                p["destination"] = json!(destination);
                ("file.get", p)
            }
            FileCommand::Put {
                edit,
                source,
                destination,
                replace,
            } => {
                let mut p = edit_params(edit, destination);
                p["source"] = json!(source);
                p["replace"] = json!(replace);
                ("file.put", p)
            }
            FileCommand::Mkdir { edit, path } => ("file.mkdir", edit_params(edit, path)),
            FileCommand::Remove { edit, path } => ("file.remove", edit_params(edit, path)),
            FileCommand::Rename {
                edit,
                source,
                destination,
            } => {
                let mut p = edit_params(edit, source);
                p["destination"] = json!(destination);
                ("file.rename", p)
            }
        },
        Command::Serve { .. } => unreachable!(),
    }
}
fn rpc_error(id: Value, code: i32, message: &str, data: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message,"data":data}})
}
fn rpc(line: &str) -> Option<Value> {
    let request: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Some(rpc_error(Value::Null, -32700, "Parse error", Value::Null)),
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let valid_id = id.is_null() || id.is_string() || id.is_i64() || id.is_u64();
    if !request.is_object()
        || request["jsonrpc"] != "2.0"
        || !request["method"].is_string()
        || !valid_id
    {
        return Some(rpc_error(
            Value::Null,
            -32600,
            "Invalid Request",
            Value::Null,
        ));
    }
    let result = xetch_core::execute(
        request["method"].as_str().unwrap(),
        request.get("params").cloned().unwrap_or(json!({})),
    );
    request.get("id")?;
    Some(match result {
        Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
        Err(error) => {
            let code = match &error {
                xetch_core::Error::Invalid(_) => -32602,
                xetch_core::Error::Unsupported(s) if s.starts_with("unknown method ") => -32601,
                _ => -32000,
            };
            rpc_error(id, code, &error.to_string(), json!(error.body()))
        }
    })
}
fn serve() -> io::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let mut line = Vec::new();
        let n = Read::by_ref(&mut input)
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        if n > 1024 * 1024 {
            writeln!(
                output,
                "{}",
                rpc_error(
                    Value::Null,
                    -32600,
                    "Request exceeds 1 MiB limit",
                    Value::Null
                )
            )?;
            output.flush()?;
            break;
        }
        let response = match std::str::from_utf8(&line) {
            Ok(s) => rpc(s),
            Err(_) => Some(rpc_error(Value::Null, -32700, "Invalid UTF-8", Value::Null)),
        };
        if let Some(response) = response {
            writeln!(output, "{response}")?;
            output.flush()?;
        }
    }
    Ok(())
}
fn main() {
    let args = launcher::arguments(std::env::args_os(), cfg!(target_os = "macos"));
    if args.len() == 1 {
        if let Err(error) = launcher::launch() {
            eprintln!("Cannot open xEtch GUI: {error}");
            std::process::exit(4);
        }
        return;
    }
    let cli = Cli::parse_from(args);
    if matches!(cli.command, Command::Serve { .. }) {
        if let Err(e) = serve() {
            eprintln!("{e}");
            std::process::exit(4);
        }
        return;
    }
    let (method, mut p) = request(cli.command);
    if matches!(
        method,
        "image.create" | "file.put" | "file.mkdir" | "file.remove" | "file.rename"
    ) {
        p["experimental_filesystems"] = json!(cli.experimental_filesystems);
    }
    match xetch_core::execute(method, p) {
        Ok(value) => {
            let text = if cli.json {
                value.to_string()
            } else {
                serde_json::to_string_pretty(&value).unwrap()
            };
            println!("{text}");
        }
        Err(e) => {
            eprintln!("{}", json!({"error":e.body()}));
            std::process::exit(match e {
                xetch_core::Error::Invalid(_) | xetch_core::Error::Json(_) => 2,
                xetch_core::Error::Unsupported(_) => 3,
                xetch_core::Error::Conflict(_) => 5,
                _ => 4,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn size_overflow_and_units() {
        assert_eq!(parse_size("64MiB").unwrap(), 64 * 1024 * 1024);
        assert_eq!(parse_size("512").unwrap(), 512);
        assert!(parse_size("18446744073709551615G").is_err());
        assert!(parse_size("-1").is_err());
    }
    #[test]
    fn rpc_protocol_errors_and_notifications() {
        assert_eq!(rpc("{").unwrap()["error"]["code"], -32700);
        assert_eq!(rpc("[]").unwrap()["error"]["code"], -32600);
        assert_eq!(
            rpc(r#"{"jsonrpc":"2.0","id":1,"method":"missing"}"#).unwrap()["error"]["code"],
            -32601
        );
        assert_eq!(
            rpc(r#"{"jsonrpc":"2.0","id":2,"method":"image.inspect","params":{}}"#).unwrap()
                ["error"]["code"],
            -32602
        );
        assert!(rpc(r#"{"jsonrpc":"2.0","method":"missing"}"#).is_none());
    }
}
