/* SPDX-License-Identifier: GPL-2.0-or-later
 * xEtch NTFS-3G worker. The parent owns all disk and upload I/O.
 * No paths to host images, mounting privileges, or force/recovery flags.
 */
#include "config.h"
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <sys/stat.h>
#ifdef _WIN32
#include <io.h>
#endif
#include "types.h"
#include "device.h"
#include "volume.h"
#include "inode.h"
#include "attrib.h"
#include "dir.h"
#include "unistr.h"
#include "security.h"
#include "logging.h"

#define CHUNK (1024 * 1024)
_Static_assert(sizeof(off_t) >= 8, "64-bit file offsets are required");
_Static_assert(sizeof(((struct stat *)0)->st_size) >= 8, "64-bit stat sizes are required");
static uint64_t capacity, cursor;
static const char *phase = "initialization";
static void put64(unsigned char *p, uint64_t v) {
    int i;
    for (i = 0; i < 8; i++)
        p[i] = (unsigned char)(v >> (8 * i));
}
static uint32_t get32(const unsigned char *p) {
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}
static int exact_read(void *buf, size_t count) {
    return fread(buf, 1, count, stdin) == count ? 0 : -1;
}
static int header(char kind, uint64_t offset, uint64_t length) {
    unsigned char h[24] = {0};
    memcpy(h, "XNT1", 4);
    h[4] = (unsigned char)kind;
    put64(h + 8, offset);
    put64(h + 16, length);
    return fwrite(h, 1, sizeof(h), stdout) == sizeof(h) ? 0 : -1;
}
static int reply(void *data, uint32_t length) {
    unsigned char h[8];
    if (exact_read(h, 8) || get32(h) || get32(h + 4) != length) {
        errno = EIO;
        return -1;
    }
    if (length && exact_read(data, length)) {
        errno = EIO;
        return -1;
    }
    return 0;
}
static s64 transfer(char op, void *buf, s64 count, s64 offset) {
    s64 done = 0;
    if (count < 0 || offset < 0 || (uint64_t)offset > capacity ||
        (uint64_t)count > capacity - (uint64_t)offset) {
        errno = EINVAL;
        return -1;
    }
    while (done < count) {
        uint32_t n = (uint32_t)((count - done) > CHUNK ? CHUNK : count - done);
        if (header(op, (uint64_t)(offset + done), n)) {
            errno = EIO;
            return -1;
        }
        if (op == 'W' && fwrite((char *)buf + done, 1, n, stdout) != n) {
            errno = EIO;
            return -1;
        }
        if (fflush(stdout) || reply(op == 'R' ? (char *)buf + done : NULL, op == 'R' ? n : 0))
            return -1;
        done += n;
    }
    return count;
}
static int dev_open(struct ntfs_device *d, int flags) {
    if (NDevOpen(d)) {
        errno = EBUSY;
        return -1;
    }
    NDevSetOpen(d);
    if ((flags & O_ACCMODE) == O_RDONLY)
        NDevSetReadOnly(d);
    else
        NDevClearReadOnly(d);
    cursor = 0;
    return 0;
}
static int dev_close(struct ntfs_device *d) {
    NDevClearOpen(d);
    return 0;
}
static s64 dev_seek(struct ntfs_device *d, s64 off, int whence) {
    s64 base = whence == SEEK_SET   ? 0
               : whence == SEEK_CUR ? (s64)cursor
               : whence == SEEK_END ? (s64)capacity
                                    : -1;
    (void)d;
    if (base < 0 || (off < 0 && off < -base) ||
        (off >= 0 && (uint64_t)off > capacity - (uint64_t)base)) {
        errno = EINVAL;
        return -1;
    }
    cursor = (uint64_t)(base + off);
    return (s64)cursor;
}
static s64 dev_pread(struct ntfs_device *d, void *b, s64 n, s64 off) {
    (void)d;
    return transfer('R', b, n, off);
}
static s64 dev_pwrite(struct ntfs_device *d, const void *b, s64 n, s64 off) {
    if (NDevReadOnly(d)) {
        errno = EROFS;
        return -1;
    }
    NDevSetDirty(d);
    return transfer('W', (void *)b, n, off);
}
static s64 dev_read(struct ntfs_device *d, void *b, s64 n) {
    s64 r = dev_pread(d, b, n, (s64)cursor);
    if (r > 0)
        cursor += (uint64_t)r;
    return r;
}
static s64 dev_write(struct ntfs_device *d, const void *b, s64 n) {
    s64 r = dev_pwrite(d, b, n, (s64)cursor);
    if (r > 0)
        cursor += (uint64_t)r;
    return r;
}
static int dev_sync(struct ntfs_device *d) {
    if (header('F', 0, 0) || fflush(stdout) || reply(NULL, 0))
        return -1;
    NDevClearDirty(d);
    return 0;
}
static int dev_stat(struct ntfs_device *d, struct stat *b) {
    (void)d;
    memset(b, 0, sizeof(*b));
    b->st_mode = S_IFREG | 0600;
    b->st_size = (off_t)capacity;
    return 0;
}
static int dev_ioctl(struct ntfs_device *d, unsigned long request, void *arg) {
    (void)d;
    (void)request;
    (void)arg;
    errno = ENOTTY;
    return -1;
}
static struct ntfs_device_operations ops = {dev_open,  dev_close,  dev_seek, dev_read, dev_write,
                                            dev_pread, dev_pwrite, dev_sync, dev_stat, dev_ioctl};

static char *unhex(const char *text) {
    size_t n = strlen(text), i;
    char *result;
    if (n > 16384 || n % 2)
        return NULL;
    result = (char *)malloc(n / 2 + 1);
    if (!result)
        return NULL;
    for (i = 0; i < n; i += 2) {
        int a = text[i] >= '0' && text[i] <= '9'   ? text[i] - '0'
                : text[i] >= 'a' && text[i] <= 'f' ? text[i] - 'a' + 10
                                                   : -1;
        int b = text[i + 1] >= '0' && text[i + 1] <= '9'   ? text[i + 1] - '0'
                : text[i + 1] >= 'a' && text[i + 1] <= 'f' ? text[i + 1] - 'a' + 10
                                                           : -1;
        if (a < 0 || b < 0 || (a == 0 && b == 0)) {
            free(result);
            return NULL;
        }
        result[i / 2] = (char)(a * 16 + b);
    }
    result[n / 2] = 0;
    return result;
}
static int error_frame(int code, const char *context) {
    char message[1024];
    snprintf(message, sizeof(message), "%s (%s): %s (errno=%d)", context, phase, strerror(errno),
             errno);
    header('E', (uint64_t)code, strlen(message));
    fwrite(message, 1, strlen(message), stdout);
    fflush(stdout);
    return 1;
}
static ntfs_volume *mount_volume(int readonly) {
    struct ntfs_device *d = ntfs_device_alloc("xetch-parent-io", 0, &ops, NULL);
    ntfs_volume *v;
    if (!d)
        return NULL;
    v = ntfs_device_mount(d, readonly ? NTFS_MNT_RDONLY | NTFS_MNT_FORENSIC : NTFS_MNT_EXCLUSIVE);
    if (!v)
        ntfs_device_free(d);
    return v;
}
static ntfs_inode *parent_inode(ntfs_volume *v, const char *path, ntfschar **name, int *length) {
    const char *slash = strrchr(path, '/');
    char *parent;
    ntfs_inode *inode;
    if (!slash || !slash[1]) {
        errno = EINVAL;
        return NULL;
    }
    *length = ntfs_mbstoucs(slash + 1, name);
    if (*length < 1 || *length > 255) {
        errno = EINVAL;
        return NULL;
    }
    parent = (char *)malloc((size_t)(slash - path) + 2);
    if (!parent)
        return NULL;
    if (slash == path)
        strcpy(parent, "/");
    else {
        memcpy(parent, path, (size_t)(slash - path));
        parent[slash - path] = 0;
    }
    inode = ntfs_pathname_to_inode(v, NULL, parent);
    free(parent);
    return inode;
}
static int mutate(ntfs_volume *v, const char *op, const char *path, const char *dest,
                  uint64_t upload, int replace) {
    ntfs_inode *inode = NULL, *parent = NULL, *target_parent = NULL;
    ntfs_attr *data = NULL;
    ntfschar *name = NULL, *target_name = NULL;
    int length = 0, target_length = 0, result = -1, saved;
    unsigned char *buffer = NULL;
    struct PERMISSIONS_CACHE *cache = NULL;
    struct SECURITY_CONTEXT security;
    phase = "resolve path";
    inode = ntfs_pathname_to_inode(v, NULL, path);
    if (inode && inode->mft_no < 16) {
        errno = EOPNOTSUPP;
        goto done;
    }
    if (inode &&
        (inode->flags & (FILE_ATTR_ENCRYPTED | FILE_ATTR_COMPRESSED | FILE_ATTR_REPARSE_POINT))) {
        errno = EOPNOTSUPP;
        goto done;
    }
    if (!strcmp(op, "put")) {
        if (inode && !replace) {
            errno = EEXIST;
            goto done;
        }
        if (!inode) {
            le32 id;
            if (errno != ENOENT)
                goto done;
            phase = "resolve parent";
            parent = parent_inode(v, path, &name, &length);
            if (!parent)
                goto done;
            memset(&security, 0, sizeof(security));
            security.vol = v;
            security.uid = 1;
            security.gid = 1;
            security.pseccache = &cache;
            phase = "inherit file security";
            id = ntfs_inherited_id(&security, parent, FALSE);
            if (!id)
                goto done;
            phase = "create file";
            inode = ntfs_create(parent, id, name, (u8)length, S_IFREG);
            if (!inode)
                goto done;
            /* inode_sync reopens the parent to update FILE_NAME. Publish its
             * new index entry first; distinct unsynced parent handles would
             * otherwise make that lookup see the old directory contents. */
            phase = "sync parent index";
            if (ntfs_inode_close(parent)) {
                parent = NULL;
                goto done;
            }
            parent = NULL;
        }
        if ((inode->mrec->flags & MFT_RECORD_IS_DIRECTORY) ||
            (inode->flags &
             (FILE_ATTR_ENCRYPTED | FILE_ATTR_COMPRESSED | FILE_ATTR_REPARSE_POINT))) {
            errno = EOPNOTSUPP;
            goto done;
        }
        phase = "open data";
        data = ntfs_attr_open(inode, AT_DATA, AT_UNNAMED, 0);
        if (!data)
            goto done;
        phase = "resize data";
        if (ntfs_attr_truncate(data, 0) || ntfs_attr_truncate(data, (s64)upload))
            goto done;
        phase = "write data";
        buffer = (unsigned char *)malloc(CHUNK);
        if (!buffer)
            goto done;
        for (uint64_t pos = 0; pos < upload;) {
            uint32_t n = (uint32_t)((upload - pos) > CHUNK ? CHUNK : upload - pos);
            s64 written = 0;
            if (header('U', pos, n) || fflush(stdout) || reply(buffer, n))
                goto done;
            while (written < n) {
                s64 r = ntfs_attr_pwrite(data, (s64)pos + written, n - written, buffer + written);
                if (r <= 0)
                    goto done;
                written += r;
            }
            pos += n;
        }
        ntfs_attr_close(data);
        data = NULL;
        ntfs_inode_update_times(inode, NTFS_UPDATE_MCTIME);
        if (ntfs_inode_sync(inode))
            goto done;
        result = 0;
    } else if (!strcmp(op, "mkdir")) {
        le32 id;
        if (inode) {
            errno = EEXIST;
            goto done;
        }
        if (errno != ENOENT)
            goto done;
        parent = parent_inode(v, path, &name, &length);
        if (!parent)
            goto done;
        memset(&security, 0, sizeof(security));
        security.vol = v;
        security.uid = 1;
        security.gid = 1;
        security.pseccache = &cache;
        id = ntfs_inherited_id(&security, parent, TRUE);
        if (!id)
            goto done;
        inode = ntfs_create(parent, id, name, (u8)length, S_IFDIR);
        if (!inode)
            goto done;
        if (ntfs_inode_close(parent)) {
            parent = NULL;
            goto done;
        }
        parent = NULL;
        result = ntfs_inode_sync(inode);
    } else if (!strcmp(op, "remove")) {
        if (!inode)
            goto done;
        parent = parent_inode(v, path, &name, &length);
        if (!parent)
            goto done;
        /* ntfs_delete closes both inode handles on success and failure. */
        result = ntfs_delete(v, path, inode, parent, name, (u8)length);
        inode = NULL;
        parent = NULL;
    } else if (!strcmp(op, "rename")) {
        ntfs_inode *existing;
        if (!inode)
            goto done;
        if (inode->mrec->flags & MFT_RECORD_IS_DIRECTORY) {
            errno = EOPNOTSUPP;
            goto done;
        }
        existing = ntfs_pathname_to_inode(v, NULL, dest);
        if (existing) {
            ntfs_inode_close(existing);
            errno = EEXIST;
            goto done;
        }
        if (errno != ENOENT)
            goto done;
        target_parent = parent_inode(v, dest, &target_name, &target_length);
        if (!target_parent)
            goto done;
        if (ntfs_link(inode, target_parent, target_name, (u8)target_length))
            goto done;
        if (ntfs_inode_close(target_parent)) {
            target_parent = NULL;
            goto done;
        }
        target_parent = NULL;
        parent = parent_inode(v, path, &name, &length);
        if (!parent)
            goto done;
        result = ntfs_delete(v, path, inode, parent, name, (u8)length);
        inode = NULL;
        parent = NULL;
    } else
        errno = EINVAL;
done:
    saved = errno;
    if (data)
        ntfs_attr_close(data);
    if (inode && ntfs_inode_close(inode) && !result) {
        result = -1;
        saved = errno;
    }
    if (parent && ntfs_inode_close(parent) && !result) {
        result = -1;
        saved = errno;
    }
    if (target_parent && ntfs_inode_close(target_parent) && !result) {
        result = -1;
        saved = errno;
    }
    free(name);
    free(target_name);
    free(buffer);
    errno = saved;
    return result;
}
int main(int argc, char **argv) {
    ntfs_volume *volume;
    char *path, *destination;
    uint64_t upload;
    int status, saved;
    if (argc == 2 && !strcmp(argv[1], "--version")) {
        puts("{\"engine\":\"ntfs-3g\",\"version\":\"" VERSION "\",\"protocol\":1}");
        return 0;
    }
    if (argc != 8 || strcmp(argv[7], "1"))
        return 2;
#ifdef _WIN32
    _setmode(_fileno(stdin), _O_BINARY);
    _setmode(_fileno(stdout), _O_BINARY);
#endif
    ntfs_log_set_handler(ntfs_log_handler_stderr);
    ntfs_set_char_encoding("UTF-8");
    capacity = strtoull(argv[2], NULL, 10);
    upload = strtoull(argv[5], NULL, 10);
    if (capacity < 512 || capacity > INT64_MAX || upload > INT64_MAX) {
        errno = EINVAL;
        return error_frame(1, "invalid capacity");
    }
    path = unhex(argv[3]);
    destination = unhex(argv[4]);
    if (!path || !destination) {
        free(path);
        free(destination);
        errno = EINVAL;
        return error_frame(1, "invalid path");
    }
    volume = mount_volume(1);
    if (!volume) {
        free(path);
        free(destination);
        return error_frame(3, "NTFS-3G read-only preflight failed");
    }
    status = (volume->flags & VOLUME_IS_DIRTY) ? -1 : ntfs_volume_check_hiberfile(volume, 0);
    if (status) {
        ntfs_umount(volume, FALSE);
        free(path);
        free(destination);
        errno = EPERM;
        return error_frame(3, "dirty or hibernated NTFS volume");
    }
    if (ntfs_umount(volume, FALSE)) {
        free(path);
        free(destination);
        return error_frame(4, "NTFS preflight close failed");
    }
    volume = mount_volume(0);
    if (!volume) {
        free(path);
        free(destination);
        return error_frame(3, "NTFS-3G refused read-write access; recovery is disabled");
    }
    status = mutate(volume, argv[1], path, destination, upload, atoi(argv[6]));
    saved = errno;
    if (ntfs_umount(volume, FALSE) && !status) {
        status = -1;
        saved = errno;
    }
    free(path);
    free(destination);
    errno = saved;
    if (status)
        return error_frame(saved == EOPNOTSUPP ? 2
                           : saved == EEXIST   ? 3
                                               : 4,
                           "NTFS operation failed");
    header('J', 0, 2);
    fwrite("{}", 1, 2, stdout);
    fflush(stdout);
    return 0;
}
