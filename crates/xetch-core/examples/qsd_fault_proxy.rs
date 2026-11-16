//! Validation-only QSD wrapper: inject real NBD errors using QEMU blkdebug.
//! Normal backend builds neither install nor select this example.
use serde_json::{json, Value};
use std::{ffi::OsString, fs, io::Write, net::TcpListener, process::Stdio};

fn parent_id() -> u32 {
    #[cfg(target_os = "linux")]
    {
        unsafe { libc::getppid() as u32 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
            System::Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
        };
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            assert!(snapshot != INVALID_HANDLE_VALUE);
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut found = Process32FirstW(snapshot, &mut entry);
            let mut parent = 0;
            while found != 0 {
                if entry.th32ProcessID == std::process::id() {
                    parent = entry.th32ParentProcessID;
                    break;
                }
                found = Process32NextW(snapshot, &mut entry);
            }
            CloseHandle(snapshot);
            parent
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        0
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let real = std::env::var_os("XETCH_TEST_QSD_REAL").ok_or("XETCH_TEST_QSD_REAL is required")?;
    let mode = std::env::var("XETCH_TEST_QSD_FAULT").unwrap_or_default();
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let port = args
        .windows(2)
        .find(|pair| pair[0] == "--nbd-server")
        .and_then(|pair| {
            pair[1]
                .to_string_lossy()
                .split(',')
                .find_map(|field| field.strip_prefix("addr.port=").map(str::to_owned))
        })
        .ok_or("NBD port is required")?
        .parse::<u16>()?;
    if let Some(trace) = std::env::var_os("XETCH_TEST_QSD_STARTUP_TRACE") {
        let mut trace = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(trace)?;
        writeln!(
            trace,
            "{}",
            json!({"pid":std::process::id(),"port":port,"mode":mode})
        )?;
    }
    let collide = match mode.as_str() {
        "bind-always" => true,
        "bind-once" => {
            let marker = std::env::var_os("XETCH_TEST_QSD_COLLISION_MARKER")
                .ok_or("collision marker is required")?;
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(marker)
            {
                Ok(_) => true,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
                Err(error) => return Err(error.into()),
            }
        }
        _ => false,
    };
    // Keep the reservation until the real QSD exits. This exercises its actual
    // startup diagnostic instead of synthesizing an error string in the proxy.
    let _reservation = if collide {
        Some(TcpListener::bind(("127.0.0.1", port))?)
    } else {
        None
    };
    let mut writable = false;
    let mut image = Value::Null;
    for index in 0..args.len().saturating_sub(1) {
        if args[index] != "--blockdev" {
            continue;
        }
        let mut node: Value = serde_json::from_str(&args[index + 1].to_string_lossy())?;
        writable = node["read-only"] == false;
        image = node["file"]["filename"].clone();
        if mode == "startup-error" {
            node["driver"] = "xetch-validation-invalid-driver".into();
            args[index + 1] = serde_json::to_string(&node)?.into();
        }
        if writable && matches!(mode.as_str(), "eio" | "enospc" | "flush") {
            let event = if mode == "flush" {
                "flush_to_disk"
            } else {
                "write_aio"
            };
            let errno = if mode == "enospc" { 28 } else { 5 };
            let state = if mode == "flush" { 2 } else { 4 };
            let states: Vec<Value> = (1..state)
                .map(|from| json!({"event":"write_aio","state":from,"new_state":from+1}))
                .collect();
            // qcow2 emits debug events on its file child, not on a filter
            // above the qcow2 node. Inject at actual host-image I/O.
            let file = node["file"].take();
            node["file"] = json!({"driver":"blkdebug","image":file,
                "set-state":states,"inject-error":[{"event":event,"state":state,"errno":errno,"once":true}]});
            args[index + 1] = serde_json::to_string(&node)?.into();
        }
    }
    let mut command = std::process::Command::new(real);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let mut child = xetch_core::child::spawn(&mut command)?;
    if writable {
        if let Some(record) = std::env::var_os("XETCH_TEST_QSD_RECORD") {
            fs::write(
                record,
                serde_json::to_vec(
                    &json!({"parent_pid":parent_id(),"proxy_pid":std::process::id(),"qsd_pid":child.id(),"image":image,"mode":mode}),
                )?,
            )?;
        }
    }
    let status = child.wait()?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(4));
    }
    Ok(())
}
