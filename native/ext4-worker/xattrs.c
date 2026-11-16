/* SPDX-License-Identifier: BSD-3-Clause
 * Copyright (c) 2026 xEtch contributors.
 * Independent implementation of ext4's published inline/block xattr format.
 * No code from the excluded GPL ext4_xattr.c is incorporated.
 */
#include <ext4_balloc.h>
#include <ext4_blockdev.h>
#include <ext4_crc32.h>
#include <ext4_errno.h>
#include <ext4_fs.h>
#include <ext4_inode.h>
#include <ext4_super.h>
#include <ext4_trans.h>
#include <ext4_xattr.h>
#include <stdlib.h>
#include <string.h>
#define MAGIC 0xea020000u
#define MAX_ATTRS 256
struct attribute {
    uint8_t index, length;
    char *name;
    uint8_t *value;
    size_t size;
};
struct attributes {
    struct ext4_inode_ref *ref;
    struct attribute items[MAX_ATTRS];
    size_t count, inline_offset, inline_size;
    uint32_t bs, references;
    uint64_t block;
    uint8_t *disk;
};
static uint16_t r16(const void *p) {
    const uint8_t *b = p;
    return b[0] | (uint16_t)b[1] << 8;
}
static uint32_t r32(const void *p) {
    const uint8_t *b = p;
    return r16(b) | (uint32_t)r16(b + 2) << 16;
}
static void w16(void *p, uint16_t n) {
    uint8_t *b = p;
    b[0] = (uint8_t)n;
    b[1] = (uint8_t)(n >> 8);
}
static void w32(void *p, uint32_t n) {
    w16(p, (uint16_t)n);
    w16((uint8_t *)p + 2, (uint16_t)(n >> 16));
}
static size_t aligned(size_t n) { return (n + 3) & ~(size_t)3; }
static uint32_t entry_hash(const struct attribute *a, bool legacy_signed) {
    uint32_t h = 0;
    for (size_t i = 0; i < a->length; i++) {
        uint32_t byte = (uint8_t)a->name[i];
        if (legacy_signed && byte >= 128)
            byte |= 0xffffff00u;
        h = (h << 5) ^ (h >> 27) ^ byte;
    }
    for (size_t i = 0; i < a->size; i += 4) {
        uint32_t word = 0;
        for (size_t j = 0; j < 4 && i + j < a->size; j++)
            word |= (uint32_t)a->value[i + j] << (j * 8);
        h = (h << 16) ^ (h >> 16) ^ word;
    }
    return h;
}
static uint32_t checksum(struct attributes *a, uint64_t physical, uint8_t *data) {
    uint8_t number[8];
    w32(number, (uint32_t)physical);
    w32(number + 4, (uint32_t)(physical >> 32));
    uint32_t seed = ext4_crc32c(0xffffffff, a->ref->fs->sb.uuid, 16);
    seed = ext4_crc32c(seed, number, 8);
    uint32_t saved = r32(data + 16);
    w32(data + 16, 0);
    uint32_t result = ext4_crc32c(seed, data, a->bs);
    w32(data + 16, saved);
    return result;
}
static bool checksums(struct attributes *a) {
    return ext4_sb_feature_ro_com(&a->ref->fs->sb, EXT4_FRO_COM_METADATA_CSUM);
}
static void dispose(struct attributes *a) {
    for (size_t i = 0; i < a->count; i++) {
        free(a->items[i].name);
        free(a->items[i].value);
    }
    free(a->disk);
}
static int parse(struct attributes *a, const uint8_t *data, size_t size, size_t start, size_t base,
                 bool external) {
    size_t pos = start, min_value = size, begin = a->count;
    while (pos + 4 <= size && r32(data + pos)) {
        if (pos + 16 > size || a->count == MAX_ATTRS)
            return EIO;
        uint8_t length = data[pos], index = data[pos + 1];
        size_t end = pos + aligned(16 + length), value = base + r16(data + pos + 2),
               bytes = r32(data + pos + 8);
        if (end + 4 > size || r32(data + pos + 4) || (!length && index != 2 && index != 3 && index != 8) ||
            value > size || bytes > size - value || (bytes && (value % 4 || value < end + 4)))
            return EIO;
        if (bytes && value < min_value)
            min_value = value;
        for (size_t i = 0; i < a->count; i++)
            if (a->items[i].index == index && a->items[i].length == length &&
                !memcmp(a->items[i].name, data + pos + 16, length))
                return EIO;
        struct attribute *v = &a->items[a->count];
        v->index = index;
        v->length = length;
        v->size = bytes;
        v->name = malloc((size_t)length + 1);
        v->value = malloc(bytes ? bytes : 1);
        if (!v->name || !v->value) {
            free(v->name);
            free(v->value);
            return ENOMEM;
        }
        memcpy(v->name, data + pos + 16, length);
        v->name[length] = 0;
        if (bytes)
            memcpy(v->value, data + value, bytes);
        a->count++;
        if (memchr(v->name, 0, length) ||
            (r32(data + pos + 12) &&
             (r32(data + pos + 12) != entry_hash(v, false) && r32(data + pos + 12) != entry_hash(v, true))))
            return EIO;
        if (external && a->count > begin + 1) {
            struct attribute *p = v - 1;
            if (p->index > index ||
                (p->index == index &&
                 (p->length > length || (p->length == length && memcmp(p->name, v->name, length) >= 0))))
                return EIO;
        }
        pos = end;
    }
    return pos + 4 <= min_value && pos + 4 <= size && !r32(data + pos) ? EOK : EIO;
}
static int load(struct attributes *a, struct ext4_inode_ref *ref) {
    memset(a, 0, sizeof(*a));
    a->ref = ref;
    a->bs = ext4_sb_get_block_size(&ref->fs->sb);
    size_t inode_size = ext4_get16(&ref->fs->sb, inode_size);
    if (a->bs != 1024 && a->bs != 2048 && a->bs != 4096)
        return ENOTSUP;
    if (inode_size > 128) {
        uint16_t extra = r16((uint8_t *)ref->inode + 128);
        if (extra % 4 || extra > inode_size - 128)
            return EIO;
        if (extra >= 4 && inode_size >= 128u + extra + 8) {
            a->inline_offset = 128 + extra;
            a->inline_size = inode_size - a->inline_offset;
            uint8_t *data = (uint8_t *)ref->inode + a->inline_offset;
            if (r32(data) && r32(data) != MAGIC)
                return EIO;
            if (r32(data) == MAGIC) {
                int e = parse(a, data, a->inline_size, 4, 4, false);
                if (e)
                    return e;
            }
        }
    }
    a->block = ext4_inode_get_file_acl(ref->inode, &ref->fs->sb);
    if (!a->block)
        return EOK;
    if (a->block >= ext4_sb_get_blocks_cnt(&ref->fs->sb))
        return EIO;
    a->disk = malloc(a->bs);
    if (!a->disk)
        return ENOMEM;
    struct ext4_block block = EXT4_BLOCK_ZERO();
    int e = ext4_block_get(ref->fs->bdev, &block, a->block);
    if (e)
        return e;
    memcpy(a->disk, block.data, a->bs);
    e = ext4_block_set(ref->fs->bdev, &block);
    if (e)
        return e;
    a->references = r32(a->disk + 4);
    if (r32(a->disk) != MAGIC || !a->references || a->references > 1024 || r32(a->disk + 8) != 1 ||
        r32(a->disk + 20) || r32(a->disk + 24) || r32(a->disk + 28) ||
        (checksums(a) && r32(a->disk + 16) != checksum(a, a->block, a->disk)))
        return EIO;
    return parse(a, a->disk, a->bs, 32, 0, true);
}
static int store(struct attributes *a, uint64_t physical, const uint8_t *data) {
    struct ext4_block block = EXT4_BLOCK_ZERO();
    int e = ext4_trans_block_get_noread(a->ref->fs->bdev, &block, physical);
    if (e)
        return e;
    memcpy(block.data, data, a->bs);
    e = ext4_trans_set_block_dirty(block.buf);
    int close = ext4_block_set(a->ref->fs->bdev, &block);
    return e ? e : close;
}
static int release(struct attributes *a) {
    if (!a->block)
        return EOK;
    if (a->references == 1)
        return ext4_balloc_free_block(a->ref, a->block);
    uint64_t blocks = ext4_inode_get_blocks_count(&a->ref->fs->sb, a->ref->inode);
    if (blocks < a->bs / 512)
        return EIO;
    w32(a->disk + 4, a->references - 1);
    if (checksums(a))
        w32(a->disk + 16, checksum(a, a->block, a->disk));
    int e = store(a, a->block, a->disk);
    if (!e) {
        e = ext4_inode_set_blocks_count(&a->ref->fs->sb, a->ref->inode, blocks - a->bs / 512);
        a->ref->dirty = true;
    }
    return e;
}
/* Called by the BSD inode teardown code, including shared external blocks. */
int xetch_xattr_release(struct ext4_inode_ref *ref) {
    struct attributes a;
    int e = load(&a, ref);
    if (!e)
        e = release(&a);
    if (!e) {
        ext4_inode_set_file_acl(ref->inode, &ref->fs->sb, 0);
        ref->dirty = true;
    }
    dispose(&a);
    return e;
}
static int compare(const void *left, const void *right) {
    const struct attribute *a = left, *b = right;
    if (a->index != b->index)
        return a->index < b->index ? -1 : 1;
    if (a->length != b->length)
        return a->length < b->length ? -1 : 1;
    return memcmp(a->name, b->name, a->length);
}
static bool pack(uint8_t *data, size_t *front, size_t *back, size_t base, struct attribute *a,
                 bool external) {
    size_t record = aligned(16 + a->length), value = aligned(a->size);
    if (*back < value || *front + record + 4 > *back - value)
        return false;
    *back -= value;
    uint8_t *p = data + *front;
    p[0] = a->length;
    p[1] = a->index;
    w16(p + 2, a->size ? (uint16_t)(*back - base) : 0);
    w32(p + 8, (uint32_t)a->size);
    memcpy(p + 16, a->name, a->length);
    if (a->size)
        memcpy(data + *back, a->value, a->size);
    w32(p + 12, external ? entry_hash(a, false) : 0);
    *front += record;
    return true;
}
static int save(struct attributes *a) {
    uint8_t *inside = calloc(a->inline_size ? a->inline_size : 1, 1), *outside = calloc(a->bs, 1);
    if (!inside || !outside) {
        free(inside);
        free(outside);
        return ENOMEM;
    }
    qsort(a->items, a->count, sizeof(a->items[0]), compare);
    size_t in_front = 4, in_back = a->inline_size, out_front = 32, out_back = a->bs;
    bool in_used = false, out_used = false;
    int e = EOK;
    for (size_t i = 0; i < a->count; i++) {
        if (a->inline_size && pack(inside, &in_front, &in_back, 4, &a->items[i], false))
            in_used = true;
        else if (pack(outside, &out_front, &out_back, 0, &a->items[i], true))
            out_used = true;
        else {
            e = ENOSPC;
            break;
        }
    }
    uint64_t physical = 0;
    if (!e && out_used) {
        physical = a->references == 1 ? a->block : 0;
        if (!physical)
            e = ext4_balloc_alloc_block(a->ref, a->block ? a->block : 1, &physical);
        if (!e) {
            w32(outside, MAGIC);
            w32(outside + 4, 1);
            w32(outside + 8, 1);
            uint32_t hash = 0;
            bool unhashed = false;
            for (size_t p = 32; p < out_front; p += aligned(16 + outside[p])) {
                uint32_t h = r32(outside + p + 12);
                if (!h)
                    unhashed = true;
                hash = (hash << 16) ^ (hash >> 16) ^ h;
            }
            w32(outside + 12, unhashed ? 0 : hash);
            if (checksums(a))
                w32(outside + 16, checksum(a, physical, outside));
            e = store(a, physical, outside);
        }
    }
    if (!e && physical != a->block)
        e = release(a);
    if (!e) {
        if (in_used)
            w32(inside, MAGIC);
        if (a->inline_size)
            memcpy((uint8_t *)a->ref->inode + a->inline_offset, inside, a->inline_size);
        ext4_inode_set_file_acl(a->ref->inode, &a->ref->fs->sb, physical);
        a->ref->dirty = true;
    }
    free(inside);
    free(outside);
    return e;
}
static const struct {
    uint8_t index;
    const char *text;
} prefixes[] = {{2, "system.posix_acl_access"},
                {3, "system.posix_acl_default"},
                {8, "system.richacl"},
                {1, "user."},
                {4, "trusted."},
                {6, "security."},
                {7, "system."},
                {0, ""}};
const char *ext4_get_xattr_name_prefix(uint8_t index, size_t *length) {
    for (size_t i = 0; i < sizeof(prefixes) / sizeof(prefixes[0]); i++)
        if (prefixes[i].index == index) {
            *length = strlen(prefixes[i].text);
            return prefixes[i].text;
        }
    *length = 0;
    return "";
}
const char *ext4_extract_xattr_name(const char *full, size_t size, uint8_t *index, size_t *length,
                                    bool *found) {
    *found = false;
    for (size_t i = 0; i < sizeof(prefixes) / sizeof(prefixes[0]) - 1; i++) {
        size_t n = strlen(prefixes[i].text);
        bool exact = prefixes[i].index == 2 || prefixes[i].index == 3 || prefixes[i].index == 8;
        if (size >= n && !memcmp(full, prefixes[i].text, n) && (exact ? size == n : size > n) &&
            size - n <= 255) {
            *index = prefixes[i].index;
            *length = size - n;
            *found = true;
            return full + n;
        }
    }
    *length = 0;
    return NULL;
}
static size_t find(struct attributes *a, uint8_t index, const char *name, size_t size) {
    for (size_t i = 0; i < a->count; i++)
        if (a->items[i].index == index && a->items[i].length == size && !memcmp(a->items[i].name, name, size))
            return i;
    return a->count;
}
int ext4_xattr_get(struct ext4_inode_ref *ref, uint8_t index, const char *name, size_t size, void *buffer,
                   size_t capacity, size_t *read) {
    struct attributes a;
    int e = load(&a, ref);
    if (!e) {
        size_t i = find(&a, index, name, size);
        if (i == a.count)
            e = ENODATA;
        else {
            *read = a.items[i].size;
            if (buffer && capacity < a.items[i].size)
                e = ERANGE;
            else if (buffer && a.items[i].size)
                memcpy(buffer, a.items[i].value, a.items[i].size);
        }
    }
    dispose(&a);
    return e;
}
static size_t list_slot(size_t size) {
    size_t alignment = sizeof(void *);
    return (sizeof(struct ext4_xattr_list_entry) + size + 1 + alignment - 1) & ~(alignment - 1);
}
int ext4_xattr_list(struct ext4_inode_ref *ref, struct ext4_xattr_list_entry *list, size_t *length) {
    struct attributes a;
    int e = load(&a, ref);
    size_t bytes = 0;
    for (size_t i = 0; !e && i < a.count; i++)
        bytes += list_slot(a.items[i].length);
    if (!e && list && *length < bytes)
        e = ERANGE;
    if (!e && list) {
        uint8_t *p = (uint8_t *)list;
        for (size_t i = 0; i < a.count; i++) {
            struct ext4_xattr_list_entry *entry = (void *)p;
            entry->name_index = a.items[i].index;
            entry->name_len = a.items[i].length;
            entry->name = (char *)(p + sizeof(*entry));
            memcpy(entry->name, a.items[i].name, entry->name_len + 1);
            p += list_slot(entry->name_len);
            entry->next = i + 1 < a.count ? (void *)p : NULL;
        }
    }
    if (!e)
        *length = bytes;
    dispose(&a);
    return e;
}
static int mutate(struct ext4_inode_ref *ref, uint8_t index, const char *name, size_t size, const void *value,
                  size_t bytes, bool remove) {
    if (size > 255 || (!size && index != 2 && index != 3 && index != 8) || (size && memchr(name, 0, size)))
        return EINVAL;
    struct attributes a;
    int e = load(&a, ref);
    if (!e && bytes > a.bs)
        e = ENOSPC;
    if (!e) {
        size_t i = find(&a, index, name, size);
        if (remove) {
            if (i == a.count)
                e = ENODATA;
            else {
                free(a.items[i].name);
                free(a.items[i].value);
                memmove(a.items + i, a.items + i + 1, (a.count - i - 1) * sizeof(a.items[0]));
                a.count--;
            }
        } else if (i == a.count && a.count == MAX_ATTRS)
            e = ENOSPC;
        else {
            uint8_t *copy = malloc(bytes ? bytes : 1);
            char *key = malloc(size + 1);
            if (!copy || !key) {
                free(copy);
                free(key);
                e = ENOMEM;
            } else {
                if (bytes)
                    memcpy(copy, value, bytes);
                memcpy(key, name, size);
                key[size] = 0;
                if (i < a.count) {
                    free(a.items[i].name);
                    free(a.items[i].value);
                } else
                    a.count++;
                a.items[i] = (struct attribute){
                    .index = index, .length = (uint8_t)size, .name = key, .value = copy, .size = bytes};
            }
        }
    }
    if (!e)
        e = save(&a);
    dispose(&a);
    return e;
}
int ext4_xattr_set(struct ext4_inode_ref *ref, uint8_t index, const char *name, size_t size,
                   const void *value, size_t bytes) {
    return mutate(ref, index, name, size, value, bytes, false);
}
int ext4_xattr_remove(struct ext4_inode_ref *ref, uint8_t index, const char *name, size_t size) {
    return mutate(ref, index, name, size, NULL, 0, true);
}
