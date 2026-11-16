//! Non-shipping D1 controller. Host physical paths are never accepted.
#[cfg(any(windows, target_os = "linux"))]
fn run() -> xetch_core::Result<()> {
    use serde_json::{json, Value};
    use std::{
        io::{self, BufRead, Read, Write},
        path::PathBuf,
    };
    use xetch_core::{device_validation, Error};
    let mut args = std::env::args_os().skip(1);
    let command = args
        .next()
        .and_then(|s| s.into_string().ok())
        .unwrap_or_default();
    let mut root = None;
    let mut device = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--root") => root = args.next().map(PathBuf::from),
            Some("--loop") => device = args.next().map(PathBuf::from),
            _ => {
                return Err(Error::Invalid(
                    "use create|claim|serve --root NEW_TEST_ROOT [--loop /dev/loopN]".into(),
                ))
            }
        }
    }
    let root = root.ok_or_else(|| Error::Invalid("--root is required".into()))?;
    if command == "create" {
        if device.is_some() {
            return Err(Error::Invalid("create never accepts a device path".into()));
        }
        println!("{}", device_validation::create(&root)?);
        return Ok(());
    }
    if !matches!(command.as_str(), "claim" | "serve") {
        return Err(Error::Invalid("expected create, claim or serve".into()));
    }
    let mut session = device_validation::Session::open(&root, device.as_deref())?;
    if command == "claim" {
        println!("{}", session.status(false)?);
        return Ok(());
    }
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let mut data = Vec::new();
        let n = (&mut input)
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut data)?;
        if n == 0 {
            break;
        }
        if data.len() > 1024 * 1024 {
            return Err(Error::Invalid("test request exceeds 1 MiB".into()));
        }
        let request: Value = serde_json::from_slice(&data)?;
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let result = if request["jsonrpc"] != "2.0" || !request["method"].is_string() {
            Err(Error::Invalid("JSON-RPC 2.0 method required".into()))
        } else {
            session.dispatch(
                request["method"].as_str().unwrap(),
                request.get("params").cloned().unwrap_or(json!({})),
            )
        };
        let response = match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(e) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":e.to_string(),"data":e.body()}})
            }
        };
        writeln!(output, "{response}")?;
        output.flush()?;
    }
    // Committing runs synchronously to a verified terminal state before stdout is written.
    // A vanished frontend therefore cannot cause this helper to abandon an in-flight write.
    Ok(())
}
#[cfg(any(windows, target_os = "linux"))]
fn main() {
    if let Err(e) = run() {
        eprintln!("{}", serde_json::json!({"error":e.body()}));
        std::process::exit(1);
    }
}
#[cfg(not(any(windows, target_os = "linux")))]
fn main() {
    eprintln!("D1 supports Windows and Linux only");
    std::process::exit(1);
}
