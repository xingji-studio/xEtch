/* SPDX-License-Identifier: BSD-3-Clause
 * Copyright (c) 2026 xEtch contributors.
 * Non-shipping validation helper: operate on regular disposable raw images.
 */
#ifndef _WIN32
#define _POSIX_C_SOURCE 200809L
#endif
#include <ext4.h>
#include <ext4_blockdev.h>
#include <ext4_errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#ifdef _WIN32
#define seek64 _fseeki64
#define tell64 _ftelli64
#else
#define seek64 fseeko
#define tell64 ftello
#endif
static FILE *image;
static int open_device(struct ext4_blockdev *b) {
    (void)b;
    return EOK;
}
static int close_device(struct ext4_blockdev *b) {
    (void)b;
    return fflush(image) ? EIO : EOK;
}
static int read_device(struct ext4_blockdev *b, void *data, uint64_t block, uint32_t count) {
    (void)b;
    return seek64(image, (int64_t)(block * 512), SEEK_SET) || fread(data, 512, count, image) != count ? EIO
                                                                                                      : EOK;
}
static int write_device(struct ext4_blockdev *b, const void *data, uint64_t block, uint32_t count) {
    (void)b;
    return seek64(image, (int64_t)(block * 512), SEEK_SET) || fwrite(data, 512, count, image) != count ? EIO
                                                                                                       : EOK;
}
int main(int argc, char **argv) {
    if (argc < 5 || argc > 6)
        return 2;
    struct stat info;
    if (stat(argv[2], &info) || (info.st_mode & 0170000) != 0100000)
        return 2;
    bool writing = !strcmp(argv[1], "set") || !strcmp(argv[1], "remove") || !strcmp(argv[1], "sparse");
    image = fopen(argv[2], writing ? "r+b" : "rb");
    if (!image)
        return 2;
    seek64(image, 0, SEEK_END);
    int64_t size = tell64(image);
    if (size <= 0 || size % 512)
        return 2;
    unsigned char sector[512];
    struct ext4_blockdev_iface iface = {.open = open_device,
                                        .bread = read_device,
                                        .bwrite = write_device,
                                        .close = close_device,
                                        .ph_bsize = 512,
                                        .ph_bcnt = (uint64_t)size / 512,
                                        .ph_bbuf = sector};
    struct ext4_blockdev device = {.bdif = &iface, .part_size = (uint64_t)size};
    int e = ext4_device_register(&device, "probe");
    if (!e)
        e = ext4_mount("probe", "/", !writing);
    if (e) {
        fclose(image);
        fprintf(stderr, "mount: %d\n", e);
        return 1;
    }
    if (writing)
        e = ext4_journal_start("/");
    unsigned char value[4096];
    size_t length = 0;
    if (!e && !strcmp(argv[1], "set")) {
        if (argc != 6 || strlen(argv[5]) % 2 || strlen(argv[5]) / 2 > sizeof(value))
            e = EINVAL;
        else {
            length = strlen(argv[5]) / 2;
            for (size_t i = 0; i < length; i++) {
                unsigned n;
                if (sscanf(argv[5] + i * 2, "%2x", &n) != 1) {
                    e = EINVAL;
                    break;
                }
                value[i] = (unsigned char)n;
            }
            if (!e)
                e = ext4_setxattr(argv[3], argv[4], strlen(argv[4]), value, length);
        }
    } else if (!e && !strcmp(argv[1], "sparse")) {
        ext4_file file;
        e = ext4_fopen(&file, argv[3], "wb");
        if (!e) {
            /* Non-shipping low-level fixture: a single allocated tail beyond 4 GiB. */
            file.fpos = (4ULL << 30) + (2ULL << 20) + 136;
            size_t actual = 0;
            e = ext4_fwrite(&file, "Z", 1, &actual);
            if (!e && actual != 1)
                e = EIO;
            int close = ext4_fclose(&file);
            if (!e)
                e = close;
        }
    } else if (!e && !strcmp(argv[1], "get")) {
        e = ext4_getxattr(argv[3], argv[4], strlen(argv[4]), value, sizeof(value), &length);
        if (!e) {
            for (size_t i = 0; i < length; i++)
                printf("%02x", value[i]);
            puts("");
        }
    } else if (!e && !strcmp(argv[1], "list")) {
        char names[16384];
        e = ext4_listxattr(argv[3], names, sizeof(names), &length);
        if (!e)
            for (size_t i = 0; i < length; i += strlen(names + i) + 1)
                puts(names + i);
    } else if (!e && !strcmp(argv[1], "remove"))
        e = ext4_removexattr(argv[3], argv[4], strlen(argv[4]));
    else if (!e)
        e = EINVAL;
    if (writing) {
        int stop = ext4_journal_stop("/");
        if (!e)
            e = stop;
    }
    int stop = ext4_umount("/");
    if (!e)
        e = stop;
    if (fclose(image) && !e)
        e = EIO;
    if (e)
        fprintf(stderr, "operation: %d\n", e);
    return e ? 1 : 0;
}
