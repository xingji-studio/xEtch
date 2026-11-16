/* SPDX-License-Identifier: BSD-3-Clause
 * Copyright (c) 2026 xEtch contributors.
 * Explicit unsupported adapters. No upstream GPL xattr implementation is used.
 * The parent rejects mutations involving xattrs/default ACLs before dispatch.
 */
#include <ext4_errno.h>
#include <ext4_fs.h>
#include <ext4_xattr.h>
const char *ext4_extract_xattr_name(const char *full, size_t length, uint8_t *index, size_t *suffix,
                                    bool *found) {
    (void)full;
    (void)length;
    (void)index;
    *suffix = 0;
    *found = false;
    return NULL;
}
const char *ext4_get_xattr_name_prefix(uint8_t index, size_t *length) {
    (void)index;
    *length = 0;
    return NULL;
}
int ext4_xattr_list(struct ext4_inode_ref *ref, struct ext4_xattr_list_entry *list, size_t *length) {
    (void)ref;
    (void)list;
    (void)length;
    return ENOTSUP;
}
int ext4_xattr_get(struct ext4_inode_ref *ref, uint8_t index, const char *name, size_t length, void *buffer,
                   size_t size, size_t *read) {
    (void)ref;
    (void)index;
    (void)name;
    (void)length;
    (void)buffer;
    (void)size;
    (void)read;
    return ENOTSUP;
}
int ext4_xattr_remove(struct ext4_inode_ref *ref, uint8_t index, const char *name, size_t length) {
    (void)ref;
    (void)index;
    (void)name;
    (void)length;
    return ENOTSUP;
}
int ext4_xattr_set(struct ext4_inode_ref *ref, uint8_t index, const char *name, size_t length,
                   const void *value, size_t size) {
    (void)ref;
    (void)index;
    (void)name;
    (void)length;
    (void)value;
    (void)size;
    return ENOTSUP;
}
