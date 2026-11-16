# Filesystem dependency patches

## lwext4

`lwext4/` contains the 47 selected BSD source/header files from upstream revision
`58bcf89a121b72d4fb66334f1693d3b30e4cb9c5`, with their original notices retained.
`src/ext4_extent.c` and `src/ext4_xattr.c` are excluded GPL implementations.
The opt-in worker uses the independent BSD extent module in
`native/ext4-worker/`, including independent xattr and POSIX ACL adapters. It does not build the
excluded implementations. The existing fstool path remains the default.

[lwext4-origin.json](lwext4-origin.json) lists original and reviewed modified
hashes; [lwext4-xetch.patch](lwext4-xetch.patch) records the BSD upstream delta.
The build checks exact source inventory and hashes before compiling and bundles
all selected notices, including the BSD-2-Clause `include/misc/tree.h` notice.
See [the current report](../docs/LWEXT4.md) for provenance, fixes and verification.

## fstool

`fstool/` is the MIT-licensed crates.io source for 0.5.0. Its original LICENSE,
Cargo metadata and source remain present; provenance is in `fstool-origin.json`.
The reviewable source delta is [fstool-xetch.patch](fstool-xetch.patch).
The workspace uses a Cargo path patch, never edits registry caches, and keeps
the existing Rust 1.89 minimum. Upstream 0.5.1+ raises that minimum to 1.95.

Local changes cover the exFAT hosted adapter's rename implementation, contiguous
directory support, up-case table decoding/checksum validation, and typed missing-path errors in exFAT/ext
path lookup. A Windows-only dead-code allowance on the otherwise unused
`BodyRef::HardLink` variant avoids an upstream warning in dependency builds.
Rename moves entry sets
without recreating file data; existing timestamps, attributes, cluster identities,
allocation flags and non-name secondary entries are retained. Destination
collisions use the volume's up-case table; directory ancestry uses cluster
identity. xEtch still commits changes only inside a disposable QCOW2 child.

The up-case decoder treats the terminal U+FFFF mapping as a literal, preventing
the recommended 5836-byte table from failing decoding and falling back to ASCII.
Missing, truncated and checksum-invalid tables now fail closed instead of
silently changing the volume's filename equivalence rules.

Contiguous directories use their stream's NoFatChain flag and data length during
path traversal, reads and writes. Before appending entries the adapter materializes
the existing run as a FAT chain and updates the parent entry's flags/checksum;
the clusters and child contents stay in place.

The application uses fstool's existing sector-based exFAT formatter for the
complete recommended Unicode up-case table. It does not raise the dependency's
minimum compiler version or backport unrelated filesystem changes.

The ext formatter now stamps the filesystem UUID into its internal journal.
The ext xattr decoder uses the four-byte zero terminator, allowing ACL entries
whose namespace index is nonzero and whose name suffix is empty. Reads of
external xattr blocks validate CRC32C when metadata_csum is enabled. Default
ACL and setgid inheritance live in the application adapter, using the ext4
on-disk version 1 representation.
The format layout planner also avoids intermediate u32 overflow when computing
the final group's end near the 2^32-block limit (covered by an arithmetic-only
boundary test, not a full-capacity image claim).

The JBD2 writer preserves existing classic/64-bit/checksum-v2/v3 tag layouts,
escapes magic-prefixed payloads, ends each descriptor with LAST_TAG, and emits
the required tag, descriptor, commit and superblock CRC32C values. Superblock
decode checks all 1024 bytes when checksums are enabled; sequence/start changes
refresh its CRC. Legacy checksum-v1 writes are explicitly rejected, including
the oversized-transaction fallback. Linux kernel roundtrips and actual kernel
replay of the backend's saved journal cover checksum-v2/v3, including 2 KiB
blocks and 32/64-bit v2 tags. Current kernels replay the prepared v2 fixtures
and upgrade subsequent journal writes to v3; older-kernel-generated,
continuously-v2 samples remain untested. See the
[advanced validation matrix](../docs/ADVANCED-FS-VALIDATION.md).

See [the exFAT/ext4 notes](../docs/EXFAT-EXT4.md) for tested boundaries and
interoperability results.

## fatfs

`fatfs/` preserves the MIT-licensed 0.3.6 source and license. Provenance is in
`fatfs-origin.json`; [fatfs-xetch.patch](fatfs-xetch.patch) contains the small
directory fix. Dot entries have no LFN prefix, and a root-level directory's `..`
cluster is zero, including FAT32. Independent fsck.fat/mtools tests cover
FAT12/16/32 and the actual WinUI-created FAT32 output. The original API and
existing cross-parent directory-move restriction remain unchanged. Upstream
also records the dot-entry issue in its [changelog](https://github.com/rafalh/rust-fatfs/blob/master/CHANGELOG.md).
