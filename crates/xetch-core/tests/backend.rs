//! Portable backend integration tests. Run explicitly with QEMU installed:
//! cargo test -p xetch-core --test backend -- --include-ignored
use serde_json::{json, Value};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::Path,
};
use xetch_core::execute;

fn call(method: &str, p: Value) -> Value {
    execute(method, p).unwrap_or_else(|e| panic!("{method}: {e}"))
}
fn create(path: &Path, table: &str) {
    call(
        "image.create",
        json!({"output":path,"size":67108864,"table":table}),
    );
}
fn revision(workspace: &Path) -> usize {
    call("workspace.plan", json!({"workspace":workspace}))["manifest"]["changes"]
        .as_array()
        .unwrap()
        .len()
}

#[test]
#[ignore = "requires qemu-img, qemu-storage-daemon and loopback sockets"]
fn fat_vmdk_workspace_roundtrip_and_failed_operation_rollback() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    let image = p.join("source.img");
    let workspace = p.join("session");
    create(&image, "none");
    let original = fs::read(&image).unwrap();
    call(
        "workspace.create",
        json!({"image":image,"workspace":workspace}),
    );
    call(
        "file.mkdir",
        json!({"workspace":workspace,"path":"/中文目录"}),
    );
    let payload = p.join("payload.txt");
    fs::write(&payload, "跨平台 xEtch\n").unwrap();
    call(
        "file.put",
        json!({"workspace":workspace,"path":"/中文目录/内容.txt","source":payload}),
    );
    assert_eq!(revision(&workspace), 2);
    assert!(execute(
        "file.put",
        json!({"workspace":workspace,"path":"/中文目录/内容.txt","source":payload})
    )
    .is_err());
    assert!(execute(
        "file.remove",
        json!({"workspace":workspace,"path":"/中文目录"})
    )
    .is_err());
    assert!(execute(
        "file.mkdir",
        json!({"workspace":workspace,"path":"/../escape"})
    )
    .is_err());
    assert_eq!(revision(&workspace), 2);
    // An orphan left by interruption is ignored; the committed head is authoritative.
    fs::write(workspace.join("edit-orphan.qcow2"), "incomplete").unwrap();
    call(
        "file.rename",
        json!({"workspace":workspace,"path":"/中文目录/内容.txt","destination":"/中文目录/renamed.txt"}),
    );
    fs::write(&payload, "short").unwrap();
    call(
        "file.put",
        json!({"workspace":workspace,"path":"/中文目录/renamed.txt","source":payload,"replace":true}),
    );
    let entries = call(
        "file.list",
        json!({"workspace":workspace,"path":"/中文目录"}),
    );
    assert_eq!(entries["entries"][0]["size"], 5);
    let output = p.join("edited.vmdk");
    call(
        "workspace.apply",
        json!({"workspace":workspace,"output":output}),
    );
    let exported = p.join("exported.txt");
    call(
        "file.get",
        json!({"image":output,"path":"/中文目录/renamed.txt","destination":exported}),
    );
    assert_eq!(fs::read(exported).unwrap(), b"short");
    assert_eq!(fs::read(&image).unwrap(), original);
    assert!(execute(
        "workspace.apply",
        json!({"workspace":workspace,"output":output})
    )
    .is_err());
    call(
        "file.remove",
        json!({"workspace":workspace,"path":"/中文目录/renamed.txt"}),
    );
    call(
        "file.remove",
        json!({"workspace":workspace,"path":"/中文目录"}),
    );
    assert_eq!(
        call("file.list", json!({"workspace":workspace}))["entries"],
        json!([])
    );
    call(
        "file.mkdir",
        json!({"workspace":workspace,"path":"/parent"}),
    );
    call(
        "file.mkdir",
        json!({"workspace":workspace,"path":"/straße"}),
    );
    let before = revision(&workspace);
    assert!(matches!(
        execute(
            "file.rename",
            json!({"workspace":workspace,"path":"/straße","destination":"/parent/moved"})
        ),
        Err(xetch_core::Error::Unsupported(_))
    ));
    assert!(execute(
        "file.rename",
        json!({"workspace":workspace,"path":"/straße","destination":"/STRASSE/child"})
    )
    .is_err());
    assert_eq!(revision(&workspace), before);
    call(
        "file.rename",
        json!({"workspace":workspace,"path":"/straße","destination":"/renamed"}),
    );
}

#[test]
#[ignore = "requires QEMU and loopback sockets"]
fn mbr_gpt_and_vmdk_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    for table in ["mbr", "gpt"] {
        let image = p.join(format!("{table}.vmdk"));
        create(&image, table);
        let info = call("image.inspect", json!({"image":image}));
        assert_eq!(info["image"]["format"], "vmdk");
        assert_eq!(info["layout"]["table"], table.to_uppercase());
        assert_eq!(info["layout"]["partitions"][0]["filesystem"], "FAT32");
        let workspace = p.join(table);
        call(
            "workspace.create",
            json!({"image":image,"workspace":workspace}),
        );
        call(
            "file.mkdir",
            json!({"workspace":workspace,"partition":1,"path":"/folder"}),
        );
        let output = p.join(format!("{table}-out.img"));
        call(
            "workspace.apply",
            json!({"workspace":workspace,"output":output}),
        );
        assert_eq!(
            call("file.list", json!({"image":output,"partition":1}))["entries"][0]["name"],
            "folder"
        );
        assert!(execute("file.list", json!({"image":image,"partition":99})).is_err());
    }
}

#[test]
#[ignore = "requires QEMU and loopback sockets"]
fn changed_source_and_corrupt_gpt_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    let image = p.join("source.img");
    let w = p.join("workspace");
    create(&image, "gpt");
    call("workspace.create", json!({"image":image,"workspace":w}));
    let mut file = fs::OpenOptions::new().write(true).open(&image).unwrap();
    file.seek(SeekFrom::Start(512 + 16)).unwrap();
    file.write_all(&[0xff; 4]).unwrap();
    drop(file);
    assert!(execute("image.inspect", json!({"image":image})).is_err());
    assert!(execute("workspace.plan", json!({"workspace":w})).is_err());
    assert!(execute(
        "workspace.apply",
        json!({"workspace":w,"output":p.join("out.img")})
    )
    .is_err());
    assert!(!p.join("out.img").exists());
}

#[test]
#[ignore = "requires QEMU and loopback sockets"]
fn output_and_workspace_never_overwrite_existing_data() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    let image = p.join("source.img");
    create(&image, "none");
    let before = fs::read(&image).unwrap();
    assert!(execute(
        "image.create",
        json!({"output":image,"size":67108864,"table":"none"})
    )
    .is_err());
    assert!(execute("image.convert", json!({"image":image,"output":image})).is_err());
    assert_eq!(before, fs::read(&image).unwrap());
    let existing = p.join("existing");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("keep"), "user data").unwrap();
    assert!(execute(
        "workspace.create",
        json!({"image":image,"workspace":existing})
    )
    .is_err());
    assert_eq!(fs::read(existing.join("keep")).unwrap(), b"user data");
    assert!(execute(
        "sector.read",
        json!({"image":image,"offset":67108863u64,"length":512})
    )
    .is_err());
}

#[test]
#[ignore = "requires QEMU and loopback sockets"]
fn vmdk_extent_fingerprints_and_workspace_lock() {
    use std::{fs::OpenOptions, process::Command};
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    let image = p.join("source.img");
    create(&image, "none");
    let vmdk = p.join("split.vmdk");
    assert!(
        Command::new(std::env::var_os("XETCH_QEMU_IMG").unwrap_or_else(|| "qemu-img".into()))
            .args([
                "convert",
                "-f",
                "raw",
                "-O",
                "vmdk",
                "-o",
                "subformat=twoGbMaxExtentFlat"
            ])
            .arg(&image)
            .arg(&vmdk)
            .status()
            .unwrap()
            .success()
    );
    let workspace = p.join("session");
    let result = call(
        "workspace.create",
        json!({"image":vmdk,"workspace":workspace}),
    );
    let fingerprints = result["manifest"]["fingerprints"].as_array().unwrap();
    assert_eq!(
        fingerprints.len(),
        2,
        "descriptor and extent must both be fingerprinted"
    );
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(workspace.join(".lock"))
        .unwrap();
    lock.try_lock().unwrap();
    assert!(matches!(
        execute("workspace.plan", json!({"workspace":workspace})),
        Err(xetch_core::Error::Conflict(_))
    ));
    drop(lock);
    let extent = Path::new(fingerprints[1]["path"].as_str().unwrap());
    let mut file = OpenOptions::new().write(true).open(extent).unwrap();
    file.write_all(b"changed").unwrap();
    drop(file);
    assert!(matches!(
        execute("workspace.plan", json!({"workspace":workspace})),
        Err(xetch_core::Error::Conflict(_))
    ));
}

#[test]
#[ignore = "requires QEMU and loopback sockets"]
fn exfat_native_roundtrip_opt_in_and_dirty_write_rollback() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    let image = p.join("exfat.img");
    let args = json!({"output":image,"size":67108864,"table":"none","filesystem":"exfat"});
    assert!(matches!(
        execute("image.create", args.clone()),
        Err(xetch_core::Error::Unsupported(_))
    ));
    assert!(!image.exists());
    let mut args = args;
    args["experimental_filesystems"] = json!(true);
    call("image.create", args);
    assert_eq!(
        call("image.inspect", json!({"image":image}))["layout"]["partitions"][0]["filesystem"],
        "exFAT"
    );
    let workspace = p.join("edit");
    call(
        "workspace.create",
        json!({"image":image,"workspace":workspace}),
    );
    assert!(execute("file.mkdir", json!({"workspace":workspace,"path":"/new"})).is_err());
    assert_eq!(revision(&workspace), 0);
    call(
        "file.mkdir",
        json!({"workspace":workspace,"path":"/new","experimental_filesystems":true}),
    );
    let payload = p.join("data.bin");
    fs::write(&payload, b"native exFAT data").unwrap();
    call(
        "file.put",
        json!({"workspace":workspace,"path":"/new/data.bin","source":payload,"experimental_filesystems":true}),
    );
    let output = p.join("saved.vmdk");
    call(
        "workspace.apply",
        json!({"workspace":workspace,"output":output}),
    );
    let readback = p.join("read.bin");
    call(
        "file.get",
        json!({"image":output,"path":"/new/data.bin","destination":readback}),
    );
    assert_eq!(fs::read(&readback).unwrap(), b"native exFAT data");
    assert_eq!(
        call("workspace.plan", json!({"workspace":workspace}))["manifest"]["changes"][0]
            ["experimental_filesystem"],
        "exFAT"
    );
    let mut file = fs::OpenOptions::new().write(true).open(&image).unwrap();
    file.seek(SeekFrom::Start(106)).unwrap();
    file.write_all(&[2]).unwrap();
    drop(file);
    let dirty = p.join("dirty");
    call("workspace.create", json!({"image":image,"workspace":dirty}));
    assert!(matches!(
        execute(
            "file.mkdir",
            json!({"workspace":dirty,"path":"/unsafe","experimental_filesystems":true})
        ),
        Err(xetch_core::Error::Conflict(_))
    ));
    assert_eq!(revision(&dirty), 0);
}
