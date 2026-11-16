# BSD lwext4 P0 worker

This optional executable handles ordinary ext4 file operations over xEtch's
bounded, duplex stdio block-I/O protocol. The parent owns QEMU, partition bounds,
host uploads/downloads, source fingerprints and the atomic workspace commit.
The worker receives no host image path and needs no mount privileges.

The C sources in this directory are BSD-3-Clause licensed; see [LICENSE](LICENSE).
The selected upstream files are BSD-2-Clause/BSD-3-Clause, with their original
notices retained. [The pinned inventory](../../vendor/lwext4-origin.json) identifies
every imported header/source. `ext4_extent.c` and `ext4_xattr.c`, the two upstream
GPL implementation files, are physically excluded from the vendored source and
the explicit compiler input list.

`extents.c` is an independent implementation based on the published ext4 disk
format. It validates and flattens a bounded tree, modifies/coalesces records,
reuses or allocates metadata nodes and rebuilds the tree with CRC32C tails.
It handles 48-bit physical addresses, depths up to five, sparse holes, initialized
and unwritten extents, allocation, splitting and truncation. Admission is limited
to 1/2/4 KiB blocks, 16,384 records and 4,096 external metadata nodes per inode.
These bounds deliberately fail rather than silently degrading the tree.

The user-provided XJ380/OpenXJ380 reference was consulted for mapping invariants,
checksum boundaries and validation approaches. Its native driver identifies its
own implementation as Apache-2.0. No proprietary XJ380 source, Apache driver
implementation or upstream GPL implementation is copied into this worker.
This is a format-based implementation, not a claim of a formal clean-room process.

`xattr_disabled.c` supplies explicit unsupported adapters. The parent rejects
mutations involving xattrs/default ACLs along the affected paths before invoking
the worker; it does not silently discard attributes. The existing fstool engine
retains its previous functionality and remains the default.

Build on Linux/WSL with `python3 scripts/build_ext4_worker.py`; add
`--target windows` for the statically linked MinGW Windows executable. The build
checks the source inventory and hashes, rejects the excluded implementation
files, records actual compile inputs and copies all license notices. Windows
execution is native; its parent backend is built with native MSVC Rust.

The internal `XEW1` protocol uses a 24-byte little-endian header: four-byte magic,
one-byte operation, three zero reserved bytes, u64 offset and u64 length. The
maximum payload is 1 MiB. R/W/F/U requests receive a u32 status, u32 payload
length and optional bytes. H identifies the worker PID, D streams downloads,
J returns final JSON only after unmount/flush, and E returns a typed error.
The parent rejects unexpected, oversized, out-of-range or read-only writes.
Each successful W acknowledgment includes an NBD FLUSH; C cache writeback alone
is not treated as storage durability. The public CLI and JSON-RPC stay unchanged.

See [the P0 validation report](../../docs/LWEXT4-P0.md) for scope and evidence.
