# xEtch NTFS worker

This optional executable uses NTFS-3G 2026.9.28 (GPL-2.0-or-later).
`worker.c` and `windows-compat/` are GPL-2.0-or-later, independently of the
Apache-2.0 Rust application. NTFS-3G is statically linked only into this worker.
The parent communicates with it through the documented binary protocol below.

Build: `python3 scripts/build_ntfs_worker.py`; add `--target windows` on a Linux
host with `x86_64-w64-mingw32-gcc`. Place the worker alongside xetch, or set
`XETCH_NTFS_WORKER`. The script downloads a SHA-256-pinned upstream release,
builds only libntfs-3g, and writes a dependency profile and license notice.
Distributors must also provide the corresponding source (upstream archive,
this worker, compatibility files and build script) under its GPL terms.

Source: https://github.com/tuxera/ntfs-3g and the pinned release URL/hash in the
build script. No filesystem algorithms, log checks, or security descriptor
algorithms are replaced. MinGW adaptations retain GCC attributes in upstream
compat.h, provide missing POSIX mode constants and account lookup declarations,
and report that POSIX host accounts are unavailable. New files inherit NTFS
security descriptors from their NTFS parent; existing descriptors are retained.
Windows host ACLs are not copied into a guest filesystem.

The worker is an offline filesystem engine, not a mount driver. It receives no
host image path and cannot request I/O outside the selected partition. It is
not an OS security sandbox; use only trusted offline images.

## Protocol 1

Arguments: operation, partition bytes, hex UTF-8 source path, hex UTF-8 destination
path, upload bytes, replace (0/1), protocol (1). Paths are absolute guest paths.
`--version` returns a single JSON object with engine/version/protocol.

Worker stdout uses a 24-byte frame: magic `XNT1`, one-byte operation, three zero
reserved bytes, little-endian u64 offset, little-endian u64 length (max 1 MiB).
`R` reads partition bytes, `W` carries bytes to write, `F` flushes, and `U` reads
the parent's immutable upload snapshot. Parent replies with little-endian u32
status (0 success) and u32 payload length, followed by requested read bytes.
`W`/`F` replies have no payload. The parent bounds all arithmetic and requests.
`J` carries final JSON only after a successful clean unmount; `E` carries UTF-8
error text and an error class in offset. Stderr is diagnostic output.

Each operation preflights read-only, rejects a dirty/hibernated volume, opens
read-write without recovery/force flags, flushes and cleanly unmounts. The parent
publishes the new QCOW2 checkpoint only on complete success. Failed writes or
worker termination leave the previous checkpoint active.

Supported: ordinary unnamed file data creation/replacement, directory creation,
file/empty-directory deletion, and file rename/move. Directory rename, writes
to compressed/encrypted/reparse files, and NTFS metadata paths are rejected.
Existing file replacement retains its MFT identity, security and other streams.
This is not NTFS repair, journal replay, arbitrary attribute/ADS editing, or a
claim of Windows chkdsk certification.
