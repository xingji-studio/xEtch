`jbd2-csum-v3.bin` contains the 1024-byte journal superblock from a disposable
128 MiB ext4 image, formatted by e2fsprogs 1.46.5 and mounted/written/unmounted
by Linux 6.18.33.2-microsoft-standard-WSL2 during the system roundtrip test.
It contains filesystem metadata only. Its feature flags are 64BIT | CSUM_V3
(`0x12`); the stored CRC32C is `b817de4f`.

The regression uses an independent bitwise CRC to check the original kernel
record, sequence/start updates, truncated reads and corruption detection.
