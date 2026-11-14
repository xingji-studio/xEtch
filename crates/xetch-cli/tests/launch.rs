#[cfg(unix)]
use std::path::Path;
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn backend() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xetch"));
    command.env("XETCH_GUI", "/does-not-exist/xetch-test-frontend");
    command
}

#[test]
fn explicit_cli_arguments_never_start_the_gui() {
    for (args, expected) in [
        (vec!["--help"], 0),
        (vec!["--version"], 0),
        (vec!["help", "inspect"], 0),
        (vec!["--json", "doctor"], 0),
        (vec!["doctor", "--json"], 0),
        (vec!["--json"], 2),
        (vec!["--experimental-filesystems"], 2),
        (vec!["serve"], 2),
        (vec!["--unknown"], 2),
        (vec!["--"], 2),
        (vec![""], 2),
        (vec!["-psn_0_123junk"], 2),
    ] {
        let output = backend().args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(expected), "{args:?}: {output:?}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Cannot open xEtch GUI"));
        if args.contains(&"doctor") {
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
                    ["protocol_version"],
                1
            );
        }
    }
}

#[test]
fn stdio_backend_keeps_its_protocol_and_exits_on_eof() {
    let mut process = backend()
        .args(["serve", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    process.stdin.take().unwrap().write_all(
        b"{\"jsonrpc\":\"2.0\",\"id\":71,\"method\":\"missing\"}\n{\"jsonrpc\":\"2.0\",\"method\":\"missing\"}\n{bad\n",
    ).unwrap();
    let output = process.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let responses: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["id"], 71);
    assert_eq!(responses[0]["error"]["code"], -32601);
    assert_eq!(responses[1]["error"]["code"], -32700);
}

#[test]
fn missing_frontend_and_recursive_override_fail_without_starting_backend_mode() {
    let missing = tempfile::tempdir().unwrap();
    let output = backend()
        .env("XETCH_GUI", missing.path().join("missing frontend"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("GUI"));
    let output = backend()
        .env("XETCH_GUI", env!("CARGO_BIN_EXE_xetch"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&output.stderr).contains("backend executable itself"));
}

#[cfg(unix)]
fn stub_frontend(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        b"#!/bin/sh\nprintf '%s\\n' \"$XETCH_BACKEND\" \"$PWD\" \"$#\" > \"$XETCH_LAUNCH_TEST_RECORD\"\n",
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
fn wait_for_record(path: &Path) -> Vec<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            let lines: Vec<_> = text.lines().map(String::from).collect();
            if lines.len() == 3 {
                return lines;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "GUI was never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn no_arguments_launch_frontend_without_waiting_or_inheriting_stdio() {
    let root = tempfile::tempdir().unwrap();
    let gui = root.path().join("GUI folder 中文/custom frontend");
    stub_frontend(&gui);
    for (name, args, override_backend) in [
        ("default", vec![], None),
        ("override", vec![], Some("/explicit/backend")),
        (
            "system",
            if cfg!(target_os = "macos") {
                vec!["-psn_0_123", "-NSDocumentRevisionsDebugMode", "YES"]
            } else {
                vec![]
            },
            None,
        ),
    ] {
        let record = root.path().join(name);
        let mut command = backend();
        command
            .args(args)
            .env("XETCH_GUI", &gui)
            .env("XETCH_LAUNCH_TEST_RECORD", &record)
            .env_remove("XETCH_BACKEND")
            .env("PATH", "")
            .current_dir(root.path());
        if let Some(value) = override_backend {
            command.env("XETCH_BACKEND", value);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "{output:?}"
        );
        let lines = wait_for_record(&record);
        let expected = override_backend.map(String::from).unwrap_or_else(|| {
            Path::new(env!("CARGO_BIN_EXE_xetch"))
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        });
        assert_eq!(lines[0], expected);
        assert_eq!(
            Path::new(&lines[1]),
            gui.parent().unwrap().canonicalize().unwrap()
        );
        assert_eq!(lines[2], "0");
    }
}

#[cfg(unix)]
#[test]
fn renamed_portable_backend_finds_only_its_own_gui_from_unrelated_directory() {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("portable folder 中文");
    std::fs::create_dir(&package).unwrap();
    let binary = package.join("renamed backend");
    std::fs::copy(env!("CARGO_BIN_EXE_xetch"), &binary).unwrap();
    std::fs::write(package.join("xetch-package.json"), "{}").unwrap();
    let name = if cfg!(target_os = "macos") {
        "xEtch.app/Contents/MacOS/xEtch"
    } else {
        "xetch-gui"
    };
    let gui = package.join("gui").join(name);
    stub_frontend(&gui);
    let record = root.path().join("record");
    let mut command = Command::new(&binary);
    command
        .current_dir(root.path())
        .env_remove("XETCH_GUI")
        .env_remove("XETCH_BACKEND")
        .env("PATH", "")
        .env("XETCH_LAUNCH_TEST_RECORD", &record);
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        Path::new(&wait_for_record(&record)[0]),
        binary.canonicalize().unwrap()
    );
    std::fs::remove_file(&gui).unwrap();
    // A similarly named executable in the working directory must not run.
    stub_frontend(&root.path().join("gui").join(name));
    std::fs::remove_file(&record).unwrap();
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert!(!record.exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("serve --stdio"));
}
