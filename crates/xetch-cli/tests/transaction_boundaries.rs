#![cfg(feature = "validation-hooks")]
//! Real child-process interruption tests. Run with QEMU and --include-ignored.
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xetch"));
    command.args(["--json", "--experimental-filesystems"]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
fn run(args: &[&str]) -> Output {
    command().args(args).output().unwrap()
}
fn success(args: &[&str]) -> Value {
    let output = run(args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn wait_point(child: &mut Child, record: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(bytes) = fs::read(record) {
            if serde_json::from_slice::<Value>(&bytes).is_ok() {
                return;
            }
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "backend exited before interruption point"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("interruption point timed out");
}

#[test]
#[ignore = "requires QEMU and a validation-hooks build"]
fn interrupted_output_is_reclaimed_without_windows_lock_conflict() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("created.img");
    let record = root.path().join("point.json");
    let args = [
        "create",
        "--output",
        output.to_str().unwrap(),
        "--size",
        "64MiB",
        "--table",
        "none",
        "--filesystem",
        "exfat",
    ];
    let mut child = command()
        .args(args)
        .env("XETCH_VALIDATION_POINT", "create.formatted")
        .env("XETCH_VALIDATION_MODE", "pause")
        .env("XETCH_VALIDATION_RECORD", &record)
        .spawn()
        .unwrap();
    wait_point(&mut child, &record);
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!output.exists());
    success(&args);
    assert_eq!(
        success(&["inspect", output.to_str().unwrap()])["layout"]["partitions"][0]["filesystem"],
        "exFAT"
    );
    assert!(!fs::read_dir(root.path()).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".xetch-stage-")));
}

#[test]
#[ignore = "requires QEMU and a validation-hooks build"]
fn post_publication_failure_reports_uncertain_and_preserves_output() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("created.img");
    let record = root.path().join("point.json");
    let args = [
        "create",
        "--output",
        output.to_str().unwrap(),
        "--size",
        "64MiB",
        "--table",
        "none",
        "--filesystem",
        "exfat",
    ];
    let result = command()
        .args(args)
        .env("XETCH_VALIDATION_POINT", "publish.published")
        .env("XETCH_VALIDATION_MODE", "error")
        .env("XETCH_VALIDATION_RECORD", record)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let error: Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(error["error"]["code"], "commit_uncertain");
    let before = fs::read(&output).unwrap();
    assert!(!run(&args).status.success());
    assert_eq!(fs::read(&output).unwrap(), before);
    success(&["inspect", output.to_str().unwrap()]);
}

#[test]
#[ignore = "requires QEMU and a validation-hooks build"]
fn interrupted_manifest_recovers_old_data_and_removes_owned_temporaries() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.img");
    let ws = root.path().join("session");
    success(&[
        "create",
        "--output",
        source.to_str().unwrap(),
        "--size",
        "64MiB",
        "--table",
        "none",
        "--filesystem",
        "exfat",
    ]);
    success(&[
        "workspace",
        "create",
        source.to_str().unwrap(),
        "--workspace",
        ws.to_str().unwrap(),
    ]);
    let payload = root.path().join("payload.bin");
    fs::write(&payload, b"old data").unwrap();
    let args = [
        "file",
        "put",
        "--workspace",
        ws.to_str().unwrap(),
        "--source",
        payload.to_str().unwrap(),
        "--destination",
        "/data.bin",
        "--replace",
    ];
    success(&args);
    let before = fs::read(ws.join("workspace.json")).unwrap();
    fs::write(&payload, b"new data").unwrap();
    let record = root.path().join("point.json");
    let mut child = command()
        .args(args)
        .env("XETCH_VALIDATION_POINT", "manifest.temporary_synced")
        .env("XETCH_VALIDATION_MODE", "pause")
        .env("XETCH_VALIDATION_RECORD", &record)
        .spawn()
        .unwrap();
    wait_point(&mut child, &record);
    child.kill().unwrap();
    child.wait().unwrap();
    success(&["workspace", "info", "--workspace", ws.to_str().unwrap()]);
    assert_eq!(fs::read(ws.join("workspace.json")).unwrap(), before);
    assert!(!ws.join(".pending-edit.json").exists());
    assert!(!fs::read_dir(&ws).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".manifest-")));
    let readback = root.path().join("readback.bin");
    success(&[
        "file",
        "get",
        "--workspace",
        ws.to_str().unwrap(),
        "--source",
        "/data.bin",
        "--destination",
        readback.to_str().unwrap(),
    ]);
    assert_eq!(fs::read(readback).unwrap(), b"old data");
}
