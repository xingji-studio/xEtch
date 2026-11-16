//! Validation-only malformed worker frames; never selected by normal builds.
use std::io::{self, Write};

fn frame(kind: u8, offset: u64, length: u64, payload: &[u8], bad_magic: bool) {
    let mut header = [0u8; 24];
    header[..4].copy_from_slice(if bad_magic { b"BAD!" } else { b"XEW1" });
    header[4] = kind;
    header[8..16].copy_from_slice(&offset.to_le_bytes());
    header[16..24].copy_from_slice(&length.to_le_bytes());
    let mut stdout = io::stdout().lock();
    stdout.write_all(&header).unwrap();
    stdout.write_all(payload).unwrap();
    stdout.flush().unwrap();
}
fn main() {
    let capacity: u64 = std::env::args().nth(2).unwrap().parse().unwrap();
    let mode = std::env::var("XETCH_EXT4_PROBE").unwrap();
    let hello = if mode == "old_profile" {
        format!("{{\"pid\":{}}}", std::process::id())
    } else {
        serde_json::json!({"pid":std::process::id(),"protocol":1,"profile":"bsd-xattr-acl-v1","xattr":true,"acl":true}).to_string()
    };
    frame(b'H', 0, hello.len() as u64, hello.as_bytes(), false);
    match mode.as_str() {
        "oversized" => frame(b'R', 0, 1024 * 1024 + 1, &[], false),
        "out_of_bounds" => frame(b'R', capacity, 512, &[], false),
        "write_read_only" => frame(b'W', 0, 512, &[0; 512], false),
        "wrong_magic" => frame(b'R', 0, 0, &[], true),
        "exit" => std::process::exit(7),
        "old_profile" => {}
        _ => panic!("unknown probe"),
    }
}
