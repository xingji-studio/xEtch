/* SPDX-License-Identifier: BSD-3-Clause
 * Copyright (c) 2026 xEtch contributors.
 * Unprivileged lwext4 worker. All volume/upload/output I/O belongs to the parent.
 * Binary framing follows xEtch's 24-byte bounded worker protocol, magic XEW1.
 */
#include <ext4.h>
#include <ext4_blockdev.h>
#include <ext4_errno.h>
#include <ext4_inode.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#ifdef _WIN32
#include <fcntl.h>
#include <io.h>
#include <process.h>
#define process_id _getpid
#else
#include <unistd.h>
#define process_id getpid
#endif
#define LIMIT (1024u * 1024u)
static uint64_t volume_size, upload_size;
static bool writing;
static uint32_t u32(const unsigned char *b) {
    return b[0] | (uint32_t)b[1] << 8 | (uint32_t)b[2] << 16 | (uint32_t)b[3] << 24;
}
static void little64(unsigned char *b, uint64_t value) {
    for (unsigned i = 0; i < 8; i++)
        b[i] = (unsigned char)(value >> (i * 8));
}
static int emit(unsigned char operation, uint64_t offset, const void *data, uint32_t length) {
    unsigned char prefix[24] = {'X', 'E', 'W', '1', 0};
    prefix[4] = operation;
    little64(prefix + 8, offset);
    little64(prefix + 16, length);
    if (fwrite(prefix, 24, 1, stdout) != 1 || (data && length && fwrite(data, length, 1, stdout) != 1) ||
        fflush(stdout))
        return EIO;
    return EOK;
}
static int response(void *buffer, uint32_t length) {
    unsigned char prefix[8];
    if (fread(prefix, 8, 1, stdin) != 1 || u32(prefix) || u32(prefix + 4) != length)
        return EIO;
    return length && fread(buffer, length, 1, stdin) != 1 ? EIO : EOK;
}
static int transfer(unsigned char operation, uint64_t offset, void *buffer, uint64_t length) {
    uint64_t bound = operation == 'U' ? upload_size : volume_size;
    if (offset > bound || length > bound - offset || (operation == 'W' && !writing))
        return EINVAL;
    for (uint64_t done = 0; done < length;) {
        uint32_t amount = (uint32_t)((length - done) > LIMIT ? LIMIT : length - done);
        void *data = (unsigned char *)buffer + done;
        int error = emit(operation, offset + done, operation == 'W' ? data : NULL, amount);
        if (!error)
            error = response(operation == 'W' ? NULL : data, operation == 'W' ? 0 : amount);
        if (error)
            return error;
        done += amount;
    }
    return EOK;
}
static int device_open(struct ext4_blockdev *device) {
    (void)device;
    return EOK;
}
static int device_close(struct ext4_blockdev *device) {
    (void)device;
    return EOK;
}
static int device_read(struct ext4_blockdev *device, void *buffer, uint64_t block, uint32_t count) {
    (void)device;
    if (block > UINT64_MAX / 512 || count > UINT32_MAX / 512)
        return EINVAL;
    return transfer('R', block * 512, buffer, (uint64_t)count * 512);
}
static int device_write(struct ext4_blockdev *device, const void *buffer, uint64_t block, uint32_t count) {
    (void)device;
    if (block > UINT64_MAX / 512 || count > UINT32_MAX / 512)
        return EINVAL;
    /* Parent W acknowledgments include NBD FLUSH, preserving callback order. */
    return transfer('W', block * 512, (void *)buffer, (uint64_t)count * 512);
}
static int device_flush(void) {
    int error = emit('F', 0, NULL, 0);
    return error ? error : response(NULL, 0);
}
static char *decode(const char *hex) {
    size_t n = strlen(hex);
    if (n % 2 || n > 8192)
        return NULL;
    char *text = calloc(n / 2 + 1, 1);
    if (!text)
        return NULL;
    for (size_t i = 0; i < n; i += 2) {
        unsigned value = 0;
        for (size_t j = 0; j < 2; j++) {
            unsigned char c = (unsigned char)hex[i + j];
            if (c >= '0' && c <= '9')
                value = value * 16 + c - '0';
            else if (c >= 'a' && c <= 'f')
                value = value * 16 + c - 'a' + 10;
            else {
                free(text);
                return NULL;
            }
        }
        if (!value) {
            free(text);
            return NULL;
        }
        text[i / 2] = (char)value;
    }
    return text;
}
static int number(const char *text, uint64_t *result) {
    if (!*text)
        return EINVAL;
    uint64_t value = 0;
    for (const char *p = text; *p; p++) {
        if (*p < '0' || *p > '9' || value > (UINT64_MAX - (*p - '0')) / 10)
            return EINVAL;
        value = value * 10 + (unsigned)(*p - '0');
    }
    *result = value;
    return EOK;
}
static int fail(int error, const char *phase) {
    unsigned code = error == ENOENT || error == EINVAL                          ? 1
                    : error == ENOTSUP || error == EOPNOTSUPP || error == E2BIG ? 2
                    : error == EEXIST || error == ENOTEMPTY                     ? 3
                                                                                : 4;
    char message[256];
    int n = snprintf(message, sizeof(message), "lwext4 %s failed (%d)%s", phase, error,
                     error == ENOSPC ? ": no free space" : "");
    if (n > 0)
        emit('E', code, message, (uint32_t)n);
    return 1;
}
static void parent_name(const char *path, char *parent) {
    strcpy(parent, path);
    char *last = strrchr(parent, '/');
    if (last == parent)
        parent[1] = 0;
    else if (last)
        *last = 0;
    else
        strcpy(parent, "/");
}
static int stamp(const char *path, bool modified) {
    uint32_t now = (uint32_t)time(NULL);
    int error = ext4_ctime_set(path, now);
    return error || !modified ? error : ext4_mtime_set(path, now);
}
static int initialize_metadata(const char *path, bool directory) {
    char parent[4097];
    parent_name(path, parent);
    uint32_t mode, uid, gid;
    int error = ext4_mode_get(parent, &mode);
    if (!error)
        error = ext4_owner_get(parent, &uid, &gid);
    if (!error)
        error = ext4_owner_set(path, 0, (mode & 02000) ? gid : 0);
    if (!error)
        error = ext4_mode_set(path, (directory ? 0755 : 0644) | (directory ? (mode & 02000) : 0));
    return error ? error : stamp(path, true);
}
static int kind(const char *path, uint32_t *mode) { return ext4_mode_get(path, mode); }
static int regular(const char *path) {
    uint32_t mode;
    int error = kind(path, &mode);
    return error ? error : (mode & 0170000) == 0100000 ? EOK : ENOTSUP;
}
static int copy_file(const char *path, bool upload, bool replace, uint64_t *total) {
    uint32_t mode;
    int exists = kind(path, &mode);
    int error = EOK;
    if (upload) {
        if (!exists && !replace)
            return EEXIST;
        if (exists && exists != ENOENT)
            return exists;
        if (!exists && (mode & 0170000) != 0100000)
            return ENOTSUP;
    } else if ((error = regular(path)))
        return error;
    ext4_file file;
    error = ext4_fopen(&file, path, upload ? "wb" : "rb");
    if (error)
        return error;
    unsigned char *buffer = malloc(LIMIT);
    if (!buffer) {
        ext4_fclose(&file);
        return ENOMEM;
    }
    uint64_t length = upload ? upload_size : file.fsize;
    *total = 0;
    while (*total < length && !error) {
        size_t amount = (size_t)((length - *total) > LIMIT ? LIMIT : length - *total), actual = 0;
        if (upload) {
            error = transfer('U', *total, buffer, amount);
            if (!error)
                error = ext4_fwrite(&file, buffer, amount, &actual);
        } else {
            error = ext4_fread(&file, buffer, amount, &actual);
            if (!error && actual != amount)
                error = EIO;
            if (!error)
                error = emit('D', *total, buffer, (uint32_t)amount);
            if (!error)
                error = response(NULL, 0);
        }
        if (!error && actual != amount)
            error = EIO;
        *total += actual;
    }
    free(buffer);
    int close = ext4_fclose(&file);
    if (!error)
        error = close;
    if (!error && upload)
        error = exists == ENOENT ? initialize_metadata(path, false) : stamp(path, true);
    return error;
}
static int append(char *json, size_t *used, const char *bytes, size_t n) {
    if (n > LIMIT - *used - 1)
        return E2BIG;
    memcpy(json + *used, bytes, n);
    *used += n;
    json[*used] = 0;
    return EOK;
}
static int quoted(char *json, size_t *used, const unsigned char *name, size_t n) {
    int error = append(json, used, "\"", 1);
    for (size_t i = 0; i < n && !error; i++) {
        char escaped[7];
        unsigned char c = name[i];
        if (c == '"' || c == '\\') {
            escaped[0] = '\\';
            escaped[1] = (char)c;
            error = append(json, used, escaped, 2);
        } else if (c < 32) {
            snprintf(escaped, sizeof(escaped), "\\u%04x", c);
            error = append(json, used, escaped, 6);
        } else
            error = append(json, used, (char *)&c, 1);
    }
    return error ? error : append(json, used, "\"", 1);
}
static int list(const char *path, char **result, uint32_t *length) {
    ext4_dir directory;
    int error = ext4_dir_open(&directory, path);
    if (error)
        return error;
    char *json = calloc(LIMIT, 1);
    size_t used = 0, count = 0;
    if (!json) {
        ext4_dir_close(&directory);
        return ENOMEM;
    }
    error = append(json, &used, "{\"entries\":[", 12);
    const ext4_direntry *entry;
    while (!error && (entry = ext4_dir_entry_next(&directory))) {
        if ((entry->name_length == 1 && entry->name[0] == '.') ||
            (entry->name_length == 2 && entry->name[0] == '.' && entry->name[1] == '.'))
            continue;
        if (count++)
            error = append(json, &used, ",", 1);
        if (!error)
            error = append(json, &used, "{\"name\":", 8);
        if (!error)
            error = quoted(json, &used, entry->name, entry->name_length);
        char name[4097], suffix[128];
        size_t length = strlen(path);
        if (length + entry->name_length + 2 > sizeof(name)) {
            error = EINVAL;
            break;
        }
        memcpy(name, path, length);
        if (length && name[length - 1] != '/')
            name[length++] = '/';
        memcpy(name + length, entry->name, entry->name_length);
        name[length + entry->name_length] = 0;
        struct ext4_inode inode;
        error = error ? error : ext4_raw_inode_fill(name, NULL, &inode);
        uint64_t size = (uint64_t)u32((unsigned char *)&inode.size_lo) |
                        ((uint64_t)u32((unsigned char *)&inode.size_hi) << 32);
        int n = snprintf(suffix, sizeof(suffix), ",\"is_directory\":%s,\"size\":%llu}",
                         entry->inode_type == EXT4_DE_DIR ? "true" : "false", (unsigned long long)size);
        if (!error)
            error = append(json, &used, suffix, (size_t)n);
    }
    ext4_dir_close(&directory);
    if (!error)
        error = append(json, &used, "]}", 2);
    if (!error) {
        *result = json;
        *length = (uint32_t)used;
    } else
        free(json);
    return error;
}
int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "--version")) {
        puts("{\"engine\":\"lwext4\",\"protocol\":1,\"profile\":\"bsd-extents-p0\",\"extents\":true,"
             "\"xattr\":false,\"license\":\"BSD-3-Clause\"}");
        return 0;
    }
#ifdef _WIN32
    _setmode(_fileno(stdin), _O_BINARY);
    _setmode(_fileno(stdout), _O_BINARY);
#endif
    if (argc != 7 || number(argv[2], &volume_size) || number(argv[5], &upload_size) || !volume_size ||
        volume_size % 512)
        return fail(EINVAL, "arguments");
    const char *operation = argv[1];
    writing = strcmp(operation, "list") && strcmp(operation, "get");
    if (writing && strcmp(operation, "put") && strcmp(operation, "mkdir") && strcmp(operation, "remove") &&
        strcmp(operation, "rename"))
        return fail(EINVAL, "operation");
    char *path = decode(argv[3]), *destination = decode(argv[4]);
    if (!path || !destination || path[0] != '/' || destination[0] != '/') {
        free(path);
        free(destination);
        return fail(EINVAL, "paths");
    }
    char hello[64];
    int size = snprintf(hello, sizeof(hello), "{\"pid\":%u}", (unsigned)process_id());
    if (emit('H', 0, hello, (uint32_t)size))
        return 1;
    unsigned char sector[512];
    struct ext4_blockdev_iface iface = {.open = device_open,
                                        .bread = device_read,
                                        .bwrite = device_write,
                                        .close = device_close,
                                        .ph_bsize = 512,
                                        .ph_bcnt = volume_size / 512,
                                        .ph_bbuf = sector};
    struct ext4_blockdev device = {.bdif = &iface, .part_size = volume_size};
    int error = ext4_device_register(&device, "volume");
    if (!error)
        error = ext4_mount("volume", "/", !writing);
    if (error) {
        free(path);
        free(destination);
        return fail(error, "mount");
    }
    if (writing)
        error = ext4_journal_start("/");
    uint64_t bytes = 0;
    char *listing = NULL;
    uint32_t listing_size = 0;
    if (!error && !strcmp(operation, "list"))
        error = list(path, &listing, &listing_size);
    else if (!error && !strcmp(operation, "get"))
        error = copy_file(path, false, false, &bytes);
    else if (!error && !strcmp(operation, "put"))
        error = copy_file(path, true, !strcmp(argv[6], "1"), &bytes);
    else if (!error && !strcmp(operation, "mkdir")) {
        uint32_t mode;
        int exists = kind(path, &mode);
        error = !exists ? EEXIST : exists == ENOENT ? ext4_dir_mk(path) : exists;
        if (!error)
            error = initialize_metadata(path, true);
    } else if (!error && !strcmp(operation, "remove")) {
        uint32_t mode;
        error = kind(path, &mode);
        if (!error && (mode & 0170000) == 0040000) {
            ext4_dir dir;
            error = ext4_dir_open(&dir, path);
            if (!error) {
                const ext4_direntry *entry;
                while ((entry = ext4_dir_entry_next(&dir)))
                    if (!((entry->name_length == 1 && entry->name[0] == '.') ||
                          (entry->name_length == 2 && entry->name[0] == '.' && entry->name[1] == '.'))) {
                        error = ENOTEMPTY;
                        break;
                    }
                ext4_dir_close(&dir);
            }
            if (!error)
                error = ext4_dir_rm(path);
        } else if (!error)
            error = (mode & 0170000) == 0100000 ? ext4_fremove(path) : ENOTSUP;
    } else if (!error && !strcmp(operation, "rename")) {
        uint32_t mode, other;
        error = kind(path, &mode);
        int exists = kind(destination, &other);
        if (!error && !exists)
            error = EEXIST;
        if (!error && exists != ENOENT)
            error = exists;
        if (!error && (mode & 0170000) == 0040000 && !strncmp(path, destination, strlen(path)) &&
            destination[strlen(path)] == '/')
            error = EINVAL;
        if (!error)
            error = (mode & 0170000) == 0040000 ? ext4_dir_mv(path, destination)
                                                : ext4_frename(path, destination);
        if (!error)
            error = stamp(destination, false);
    }
    if (!error && writing) {
        char parent[4097];
        parent_name(path, parent);
        error = stamp(parent, true);
        if (!error && !strcmp(operation, "rename")) {
            parent_name(destination, parent);
            error = stamp(parent, true);
        }
    }
    if (writing) {
        int stop = ext4_journal_stop("/");
        if (!error)
            error = stop;
    }
    int close = ext4_umount("/");
    if (!error)
        error = close;
    if (!error)
        error = device_flush();
    free(path);
    free(destination);
    if (error) {
        free(listing);
        return fail(error, operation);
    }
    if (listing) {
        error = emit('J', 0, listing, listing_size);
        free(listing);
        if (error)
            return 1;
    } else {
        char result[80];
        int n = snprintf(result, sizeof(result), "{\"bytes\":%llu}", (unsigned long long)bytes);
        if (emit('J', 0, result, (uint32_t)n))
            return 1;
    }
    return 0;
}
