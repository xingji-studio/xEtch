/* SPDX-License-Identifier: BSD-3-Clause
 * Copyright (c) 2026 xEtch contributors.
 * POSIX ACL v1 disk encoding and creation-mode filtering, independently authored.
 */
#include <ext4.h>
#include <ext4_errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
static uint16_t r16(const uint8_t *p) { return p[0] | (uint16_t)p[1] << 8; }
static uint32_t r32(const uint8_t *p) { return r16(p) | (uint32_t)r16(p + 2) << 16; }
static void w16(uint8_t *p, uint16_t n) {
    p[0] = (uint8_t)n;
    p[1] = (uint8_t)(n >> 8);
}
int xetch_acl_inherit(const char *path, const char *parent, bool directory, uint32_t parent_mode) {
    const char *key = "system.posix_acl_default";
    size_t size = 0;
    uint32_t mode = (directory ? 0755u : 0644u) | (directory ? (parent_mode & 02000u) : 0);
    int e = ext4_getxattr(parent, key, strlen(key), NULL, 0, &size);
    if (e == ENODATA)
        return ext4_mode_set(path, mode);
    if (e)
        return e;
    if (size < 16 || size > 4096)
        return EINVAL;
    uint8_t *access = malloc(size), *defaults = malloc(size);
    if (!access || !defaults) {
        free(access);
        free(defaults);
        return ENOMEM;
    }
    e = ext4_getxattr(parent, key, strlen(key), defaults, size, &size);
    if (e)
        goto finish;
    if (r32(defaults) != 1) {
        e = EINVAL;
        goto finish;
    }
    unsigned owner = 0, group = 0, other = 0, mask = 0, named = 0;
    uint16_t previous = 0;
    uint32_t previous_id = 0;
    for (size_t pos = 4; pos < size;) {
        if (size - pos < 4) {
            e = EINVAL;
            goto finish;
        }
        uint16_t tag = r16(defaults + pos), perm = r16(defaults + pos + 2);
        bool has_id = tag == 2 || tag == 8;
        size_t length = has_id ? 8 : 4;
        if ((tag != 1 && tag != 2 && tag != 4 && tag != 8 && tag != 16 && tag != 32) || perm > 7 ||
            tag < previous || length > size - pos) {
            e = EINVAL;
            goto finish;
        }
        uint32_t id = has_id ? r32(defaults + pos + 4) : 0;
        if (has_id && (id == UINT32_MAX || (tag == previous && id <= previous_id))) {
            e = EINVAL;
            goto finish;
        }
        if (tag == 1)
            owner++;
        if (tag == 4)
            group++;
        if (tag == 32)
            other++;
        if (tag == 16)
            mask++;
        if (has_id)
            named++;
        previous = tag;
        previous_id = id;
        pos += length;
    }
    if (owner != 1 || group != 1 || other != 1 || mask > 1 || (named && !mask) || r16(defaults + 4) != 1 ||
        previous != 32) {
        e = EINVAL;
        goto finish;
    }
    memcpy(access, defaults, size);
    uint32_t filtered = mode & ~0777u;
    for (size_t pos = 4; pos < size;) {
        uint16_t tag = r16(access + pos), perm = r16(access + pos + 2);
        int shift = -1;
        if (tag == 1)
            shift = 6;
        else if (tag == 16 || (tag == 4 && !mask))
            shift = 3;
        else if (tag == 32)
            shift = 0;
        if (shift >= 0) {
            perm &= (mode >> shift) & 7;
            w16(access + pos + 2, perm);
            filtered |= (uint32_t)perm << shift;
        }
        pos += tag == 2 || tag == 8 ? 8 : 4;
    }
    e = ext4_setxattr(path, "system.posix_acl_access", 23, access, size);
    if (!e && directory)
        e = ext4_setxattr(path, key, strlen(key), defaults, size);
    if (!e)
        e = ext4_mode_set(path, filtered);
finish:
    free(access);
    free(defaults);
    return e;
}
