#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

/// Keep argv[0] separate from user arguments. Only consume known macOS
/// launch metadata at the front, never command operands or values after `--`.
pub fn arguments(args: impl IntoIterator<Item = OsString>, macos: bool) -> Vec<OsString> {
    let mut args = args.into_iter();
    let program = args.next().unwrap_or_else(|| "xetch".into());
    let rest: Vec<_> = args.collect();
    let mut start = 0;
    while macos && start < rest.len() {
        if is_process_serial_number(&rest[start]) {
            start += 1;
        } else if rest[start] == "-NSDocumentRevisionsDebugMode"
            && rest
                .get(start + 1)
                .is_some_and(|value| value == "YES" || value == "NO")
        {
            start += 2;
        } else {
            break;
        }
    }
    std::iter::once(program)
        .chain(rest.into_iter().skip(start))
        .collect()
}

fn is_process_serial_number(arg: &OsStr) -> bool {
    let Some(value) = arg.to_str().and_then(|s| s.strip_prefix("-psn_")) else {
        return false;
    };
    let Some((high, low)) = value.split_once('_') else {
        return false;
    };
    [high, low].iter().all(|part| {
        !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && part.parse::<u32>().is_ok()
    })
}

fn gui_name() -> &'static str {
    if cfg!(windows) {
        "xEtch.Windows.exe"
    } else if cfg!(target_os = "macos") {
        "xEtch.app/Contents/MacOS/xEtch"
    } else {
        "xetch-gui"
    }
}

fn find_gui(exe: &Path, configured: Option<OsString>) -> io::Result<PathBuf> {
    let missing = || {
        io::Error::new(
            io::ErrorKind::NotFound,
            "GUI was not found. Keep the complete application folder together, build/package the frontend, or set XETCH_GUI to its executable. Linux/macOS frontends are not available yet. For the CLI use xetch --help; for the backend use xetch serve --stdio.",
        )
    };
    let candidates = if let Some(path) = configured {
        // An explicit override must fail visibly rather than silently select
        // a different frontend. Resolve before changing the child's directory.
        vec![std::env::current_dir()?.join(path)]
    } else {
        let parent = exe.parent().ok_or_else(missing)?;
        let mut paths = vec![parent.join("gui").join(gui_name())];
        if !parent.join("xetch-package.json").is_file() {
            paths.push(parent.join(gui_name()));
            paths.push(parent.join("../gui").join(gui_name()));
            // Support cargo build users alongside the unified build outputs.
            if cfg!(windows) {
                if let Some(project) = parent
                    .ancestors()
                    .find(|root| root.join("apps/windows/xEtch.Windows.csproj").is_file())
                {
                    let profile = if parent.file_name() == Some(OsStr::new("debug")) {
                        "debug"
                    } else {
                        "release"
                    };
                    paths.push(project.join(format!(
                        "target/xetch/windows-x64/{profile}/gui/{}",
                        gui_name()
                    )));
                    paths.push(project.join(format!(
                        "target/gui/windows/bin/x64/Release/net8.0-windows10.0.19041.0/win-x64/{}",
                        gui_name()
                    )));
                }
            }
        }
        paths
    };
    for path in candidates {
        if path.is_file() {
            if path.canonicalize()? == exe.canonicalize()? {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "XETCH_GUI must refer to a frontend, not the backend executable itself.",
                ));
            }
            return Ok(path);
        }
    }
    Err(missing())
}

pub fn launch() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let gui = find_gui(&exe, std::env::var_os("XETCH_GUI"))?;
    let backend = match std::env::var_os("XETCH_BACKEND") {
        Some(path) => std::env::current_dir()?.join(path),
        None => exe,
    };
    #[cfg(windows)]
    let result = spawn_gui(&gui, &backend);
    #[cfg(not(windows))]
    let result = {
        let mut command = Command::new(&gui);
        command
            .current_dir(gui.parent().expect("absolute executable has a parent"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env("XETCH_BACKEND", backend);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        command.spawn().map(|_| ())
    };
    result.map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("Cannot start GUI {}: {error}", gui.display()),
        )
    })
}

#[cfg(windows)]
fn spawn_gui(gui: &Path, backend: &Path) -> io::Result<()> {
    use std::{mem, os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            CreateProcessW, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS, PROCESS_INFORMATION,
            STARTUPINFOW,
        },
    };
    let wide = |value: &OsStr| value.encode_wide().chain(Some(0)).collect::<Vec<_>>();
    let application = wide(gui.as_os_str());
    let directory = wide(
        gui.parent()
            .expect("absolute executable has a parent")
            .as_os_str(),
    );
    let mut quoted = OsString::from("\"");
    quoted.push(gui);
    quoted.push("\"");
    let mut command_line = wide(&quoted);
    // This branch only runs immediately before the launcher exits. Windows
    // environment mutation is safe even with threads; explicit CLI/RPC calls
    // never enter this branch.
    std::env::set_var("XETCH_BACKEND", backend);
    // SAFETY: These Win32 structs accept zero initialization; every string is
    // NUL terminated and lives through CreateProcessW. Crucially inheritHandles
    // is FALSE: Stdio::null alone still inherits unrelated Windows pipe handles,
    // which keeps a script waiting for EOF until the GUI closes.
    unsafe {
        let mut startup: STARTUPINFOW = mem::zeroed();
        startup.cb = mem::size_of::<STARTUPINFOW>() as u32;
        let mut process: PROCESS_INFORMATION = mem::zeroed();
        if CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            DETACHED_PROCESS | CREATE_UNICODE_ENVIRONMENT,
            ptr::null(),
            directory.as_ptr(),
            &startup,
            &mut process,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str], macos: bool) -> Vec<OsString> {
        arguments(values.iter().map(OsString::from), macos)
    }

    #[test]
    fn executable_name_is_never_a_user_argument() {
        for name in [
            "xetch",
            "",
            "C:\\portable folder 中文\\renamed.exe",
            "--json",
        ] {
            assert_eq!(args(&[name], false), vec![OsString::from(name)]);
        }
        assert_eq!(arguments([], false), vec![OsString::from("xetch")]);
    }

    #[test]
    fn system_metadata_only_at_the_start_on_macos() {
        for suffix in [vec![], vec!["serve", "--stdio"], vec!["--json", "doctor"]] {
            let mut input = vec![
                "xetch",
                "-psn_0_123456",
                "-NSDocumentRevisionsDebugMode",
                "YES",
            ];
            input.extend(&suffix);
            let mut expected = vec!["xetch"];
            expected.extend(&suffix);
            assert_eq!(args(&input, true), args(&expected, false));
            assert_eq!(
                args(&input, false),
                input.iter().map(OsString::from).collect::<Vec<_>>()
            );
        }
        for input in [
            vec!["xetch", "inspect", "--", "-psn_0_123"],
            vec!["xetch", "--json", "-psn_0_123"],
            vec!["xetch", "-psn_"],
            vec!["xetch", "-psn_0_123junk"],
            vec!["xetch", "-psn_0_4294967296"],
            vec!["xetch", "-psn_0_1_2"],
            vec!["xetch", "-NSDocumentRevisionsDebugMode"],
            vec!["xetch", "-NSDocumentRevisionsDebugMode", "doctor"],
            vec!["xetch", ""],
        ] {
            assert_eq!(args(&input, true), args(&input, false));
        }
    }
}
