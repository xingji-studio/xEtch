//! Explicit optional-worker regressions. Requires QEMU and build_ext4_worker.py.
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

fn command(engine: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xetch"));
    command.args(["--json", "--experimental-filesystems"]);
    command.env("XETCH_EXT4_ENGINE", engine);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
fn worker() -> PathBuf {
    if let Some(path) = std::env::var_os("XETCH_EXT4_WORKER") {
        return path.into();
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let name = if cfg!(windows) {
        "xetch-ext4-worker.exe"
    } else {
        "xetch-ext4-worker"
    };
    for directory in ["target/release", "target/windows/release"] {
        let path = root.join(directory).join(name);
        if path.is_file() {
            return path;
        }
    }
    panic!("build the optional BSD worker before running --include-ignored");
}
fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
#[ignore = "requires QEMU and the separately built BSD lwext4 worker"]
fn bsd_worker_roundtrip_and_existing_engine_readback() {
    let folder = tempfile::tempdir().unwrap();
    let source = folder.path().join("source.img");
    success(
        command("fstool")
            .args(["create", "--output"])
            .arg(&source)
            .args(["--size", "64MiB", "--table", "none", "--filesystem", "ext4"])
            .output()
            .unwrap(),
    );
    let before = fs::read(&source).unwrap();
    let workspace = folder.path().join("session");
    success(
        command("lwext4")
            .args(["workspace", "create"])
            .arg(&source)
            .arg("--workspace")
            .arg(&workspace)
            .output()
            .unwrap(),
    );
    let payload = folder.path().join("payload.bin");
    let expected: Vec<_> = (0..(1024 * 1024 + 137)).map(|n| (n % 251) as u8).collect();
    fs::write(&payload, &expected).unwrap();
    let worker = worker();
    success(
        command("lwext4")
            .env("XETCH_EXT4_WORKER", &worker)
            .args(["file", "put", "--workspace"])
            .arg(&workspace)
            .arg("--source")
            .arg(&payload)
            .args(["--destination", "/内容.bin"])
            .output()
            .unwrap(),
    );
    for engine in ["fstool", "lwext4"] {
        let output = folder.path().join(format!("{engine}.bin"));
        success(
            command(engine)
                .env("XETCH_EXT4_WORKER", &worker)
                .args(["file", "get", "--workspace"])
                .arg(&workspace)
                .args(["--source", "/内容.bin", "--destination"])
                .arg(&output)
                .output()
                .unwrap(),
        );
        assert_eq!(fs::read(output).unwrap(), expected);
    }
    assert_eq!(fs::read(source).unwrap(), before);
}

#[test]
#[ignore = "requires QEMU"]
fn engine_selection_errors_never_silently_fall_back() {
    let folder = tempfile::tempdir().unwrap();
    let source = folder.path().join("source.img");
    success(
        command("fstool")
            .args(["create", "--output"])
            .arg(&source)
            .args(["--size", "64MiB", "--table", "none", "--filesystem", "ext4"])
            .output()
            .unwrap(),
    );
    for (engine, expected) in [
        ("unknown", "invalid_argument"),
        ("lwext4", "dependency_failure"),
    ] {
        let output = command(engine)
            .env("XETCH_EXT4_WORKER", folder.path().join("missing-worker"))
            .args(["file", "list"])
            .arg(&source)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let result: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(result["error"]["code"], expected);
    }
    success(
        command("fstool")
            .env("XETCH_EXT4_WORKER", folder.path().join("missing-worker"))
            .args(["file", "list"])
            .arg(&source)
            .output()
            .unwrap(),
    );
}
