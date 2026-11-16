/* SPDX-License-Identifier: BSD-3-Clause
 * Copyright (c) 2026 xEtch contributors.
 * Independent extent implementation using ext4's published disk format.
 * This does not include or translate lwext4's excluded GPL extent module.
 * The bounded flatten/rebuild strategy favors reviewability for the P0 worker.
 */
#include <ext4_balloc.h>
#include <ext4_bcache.h>
#include <ext4_blockdev.h>
#include <ext4_crc32.h>
#include <ext4_errno.h>
#include <ext4_extent.h>
#include <ext4_fs.h>
#include <ext4_inode.h>
#include <ext4_super.h>
#include <ext4_trans.h>
#include <stdlib.h>
#include <string.h>

#define MAX_RECORDS 16384u
#define MAX_NODES 4096u
struct run {
    uint32_t logical, length;
    uint64_t physical;
    bool unwritten;
};
struct tree {
    struct run *runs;
    uint64_t *nodes;
    size_t count, node_count;
    uint32_t block_size, seed;
    bool checksums;
    struct ext4_inode_ref *ref;
};
static uint16_t rd16(const void *p) {
    const uint8_t *b = p;
    return b[0] | (uint16_t)b[1] << 8;
}
static uint32_t rd32(const void *p) {
    const uint8_t *b = p;
    return rd16(b) | (uint32_t)rd16(b + 2) << 16;
}
static void wr16(void *p, uint16_t n) {
    uint8_t *b = p;
    b[0] = (uint8_t)n;
    b[1] = (uint8_t)(n >> 8);
}
static void wr32(void *p, uint32_t n) {
    wr16(p, (uint16_t)n);
    wr16((uint8_t *)p + 2, (uint16_t)(n >> 16));
}
static int valid_blocks(struct tree *t, uint64_t block, uint32_t count) {
    uint64_t total = ext4_sb_get_blocks_cnt(&t->ref->fs->sb);
    return block && block < total && count && count <= total - block ? EOK : EIO;
}
static int visit(struct tree *t, const uint8_t *data, uint16_t depth, bool root, uint64_t lower,
                 uint64_t upper) {
    uint16_t count = rd16(data + 2), maximum = rd16(data + 4);
    uint32_t capacity = root ? 4 : (t->block_size - 12) / 12;
    if (rd16(data) != 0xf30a || rd16(data + 6) != depth || !maximum || maximum > capacity ||
        count > maximum || (depth && !count))
        return EIO;
    if (!root && t->checksums) {
        uint32_t tail = 12 + maximum * 12u;
        if (tail + 4 > t->block_size || ext4_crc32c(t->seed, data, tail) != rd32(data + tail))
            return EIO;
    }
    uint64_t previous = lower;
    for (uint16_t i = 0; i < count; i++) {
        const uint8_t *record = data + 12 + i * 12u;
        uint64_t key = rd32(record);
        if (key < previous || key >= upper)
            return EIO;
        if (depth) {
            uint64_t child = rd32(record + 4) | ((uint64_t)rd16(record + 8) << 32);
            uint64_t next = i + 1 < count ? rd32(record + 12) : upper;
            if (next <= key || next > upper || valid_blocks(t, child, 1))
                return EIO;
            if (t->node_count == MAX_NODES)
                return E2BIG;
            for (size_t n = 0; n < t->node_count; n++)
                if (t->nodes[n] == child)
                    return EIO;
            t->nodes[t->node_count++] = child;
            struct ext4_block block = EXT4_BLOCK_ZERO();
            int error = ext4_block_get(t->ref->fs->bdev, &block, child);
            if (error)
                return error;
            error = visit(t, block.data, depth - 1, false, key, next);
            int release = ext4_block_set(t->ref->fs->bdev, &block);
            if (error || release)
                return error ? error : release;
            previous = key + 1;
        } else {
            uint16_t encoded = rd16(record + 4);
            bool unwritten = encoded > 32768u;
            uint32_t length = unwritten ? encoded - 32768u : encoded;
            uint64_t physical = rd32(record + 8) | ((uint64_t)rd16(record + 6) << 32);
            if (!length || key + length > upper || valid_blocks(t, physical, length))
                return EIO;
            if (t->count == MAX_RECORDS)
                return E2BIG;
            t->runs[t->count++] = (struct run){(uint32_t)key, length, physical, unwritten};
            previous = key + length;
        }
    }
    return EOK;
}
static void dispose(struct tree *t) {
    free(t->runs);
    free(t->nodes);
}
static int by_physical(const void *a, const void *b) {
    const struct run *x = a, *y = b;
    return x->physical < y->physical ? -1 : x->physical > y->physical;
}
static int load(struct tree *t, struct ext4_inode_ref *ref) {
    memset(t, 0, sizeof(*t));
    t->ref = ref;
    t->block_size = ext4_sb_get_block_size(&ref->fs->sb);
    if (t->block_size != 1024 && t->block_size != 2048 && t->block_size != 4096)
        return ENOTSUP;
    t->checksums = ext4_sb_feature_ro_com(&ref->fs->sb, EXT4_FRO_COM_METADATA_CSUM);
    uint8_t number[4], generation[4];
    wr32(number, ref->index);
    wr32(generation, ext4_inode_get_generation(ref->inode));
    t->seed = ext4_crc32c(0xffffffffu, ref->fs->sb.uuid, 16);
    t->seed = ext4_crc32c(t->seed, number, 4);
    t->seed = ext4_crc32c(t->seed, generation, 4);
    t->runs = calloc(MAX_RECORDS + 3, sizeof(*t->runs));
    t->nodes = calloc(MAX_NODES, sizeof(*t->nodes));
    if (!t->runs || !t->nodes) {
        dispose(t);
        return ENOMEM;
    }
    const uint8_t *root = (const uint8_t *)ref->inode->blocks;
    int error = rd16(root + 6) > 5 ? EIO : visit(t, root, rd16(root + 6), true, 0, UINT64_C(1) << 32);
    if (!error && t->count) {
        struct run *sorted = malloc(t->count * sizeof(*sorted));
        if (!sorted)
            error = ENOMEM;
        else {
            memcpy(sorted, t->runs, t->count * sizeof(*sorted));
            qsort(sorted, t->count, sizeof(*sorted), by_physical);
            for (size_t i = 1; i < t->count; i++)
                if (sorted[i - 1].physical + sorted[i - 1].length > sorted[i].physical)
                    error = EIO;
            free(sorted);
        }
    }
    if (!error)
        for (size_t n = 0; n < t->node_count; n++)
            for (size_t r = 0; r < t->count; r++)
                if (t->nodes[n] >= t->runs[r].physical &&
                    t->nodes[n] - t->runs[r].physical < t->runs[r].length)
                    error = EIO;
    if (error)
        dispose(t);
    return error;
}
static void header(uint8_t *data, uint16_t count, uint16_t maximum, uint16_t depth) {
    wr16(data, 0xf30a);
    wr16(data + 2, count);
    wr16(data + 4, maximum);
    wr16(data + 6, depth);
    wr32(data + 8, 0);
}
static void encode_run(uint8_t *data, const struct run *r) {
    wr32(data, r->logical);
    wr16(data + 4, (uint16_t)(r->length + (r->unwritten ? 32768 : 0)));
    wr16(data + 6, (uint16_t)(r->physical >> 32));
    wr32(data + 8, (uint32_t)r->physical);
}
struct child {
    uint32_t key;
    uint64_t physical;
};
static int store_block(struct tree *t, uint64_t physical, const uint8_t *data) {
    struct ext4_block block = EXT4_BLOCK_ZERO();
    int error = ext4_trans_block_get_noread(t->ref->fs->bdev, &block, physical);
    if (error)
        return error;
    memcpy(block.data, data, t->block_size);
    error = ext4_trans_set_block_dirty(block.buf);
    int release = ext4_block_set(t->ref->fs->bdev, &block);
    return error ? error : release;
}
static void seal(struct tree *t, uint8_t *data, uint16_t maximum) {
    if (t->checksums) {
        uint32_t tail = 12 + maximum * 12u;
        wr32(data + tail, ext4_crc32c(t->seed, data, tail));
    }
}
static int save(struct tree *t) {
    size_t compact = 0;
    for (size_t i = 0; i < t->count; i++) {
        struct run r = t->runs[i];
        if (compact) {
            struct run *p = &t->runs[compact - 1];
            if (p->unwritten == r.unwritten && (uint64_t)p->logical + p->length == r.logical &&
                p->physical + p->length == r.physical &&
                p->length + r.length <= (r.unwritten ? 32767u : 32768u)) {
                p->length += r.length;
                continue;
            }
        }
        t->runs[compact++] = r;
    }
    t->count = compact;
    if (t->count > MAX_RECORDS)
        return E2BIG;
    uint32_t capacity = (t->block_size - 12) / 12;
    size_t needed = 0, level = t->count > 4 ? (t->count + capacity - 1) / capacity : 0;
    for (size_t n = level; n; n = n > 4 ? (n + capacity - 1) / capacity : 0)
        needed += n;
    if (needed > MAX_NODES)
        return E2BIG;
    size_t old_count = t->node_count;
    for (size_t i = old_count; i < needed; i++) {
        int error =
            ext4_balloc_alloc_block(t->ref, ext4_get32(&t->ref->fs->sb, first_data_block), &t->nodes[i]);
        if (error)
            return error;
    }
    struct child *children = calloc(MAX_NODES, sizeof(*children));
    struct child *parents = calloc(MAX_NODES, sizeof(*parents));
    uint8_t *buffer = calloc(1, t->block_size);
    if (!children || !parents || !buffer) {
        free(children);
        free(parents);
        free(buffer);
        return ENOMEM;
    }
    size_t consumed = 0, offset = 0;
    uint16_t depth = 0;
    int error = EOK;
    for (size_t i = 0; i < level && !error; i++) {
        size_t count = t->count - offset;
        if (count > capacity)
            count = capacity;
        memset(buffer, 0, t->block_size);
        header(buffer, (uint16_t)count, (uint16_t)capacity, 0);
        for (size_t j = 0; j < count; j++)
            encode_run(buffer + 12 + j * 12, &t->runs[offset + j]);
        children[i] = (struct child){t->runs[offset].logical, t->nodes[consumed++]};
        seal(t, buffer, (uint16_t)capacity);
        error = store_block(t, children[i].physical, buffer);
        offset += count;
    }
    if (level)
        depth = 1;
    while (level > 4 && !error) {
        size_t next = (level + capacity - 1) / capacity;
        offset = 0;
        for (size_t i = 0; i < next && !error; i++) {
            size_t count = level - offset;
            if (count > capacity)
                count = capacity;
            memset(buffer, 0, t->block_size);
            header(buffer, (uint16_t)count, (uint16_t)capacity, depth);
            for (size_t j = 0; j < count; j++) {
                struct child c = children[offset + j];
                uint8_t *entry = buffer + 12 + j * 12;
                wr32(entry, c.key);
                wr32(entry + 4, (uint32_t)c.physical);
                wr16(entry + 8, (uint16_t)(c.physical >> 32));
            }
            parents[i] = (struct child){children[offset].key, t->nodes[consumed++]};
            seal(t, buffer, (uint16_t)capacity);
            error = store_block(t, parents[i].physical, buffer);
            offset += count;
        }
        struct child *swap = children;
        children = parents;
        parents = swap;
        level = next;
        depth++;
    }
    if (!error) {
        uint8_t *root = (uint8_t *)t->ref->inode->blocks;
        memset(root, 0, 60);
        header(root, (uint16_t)(level ? level : t->count), 4, depth);
        if (level)
            for (size_t i = 0; i < level; i++) {
                uint8_t *entry = root + 12 + i * 12;
                wr32(entry, children[i].key);
                wr32(entry + 4, (uint32_t)children[i].physical);
                wr16(entry + 8, (uint16_t)(children[i].physical >> 32));
            }
        else
            for (size_t i = 0; i < t->count; i++)
                encode_run(root + 12 + i * 12, &t->runs[i]);
        t->ref->dirty = true;
        for (size_t i = needed; i < old_count && !error; i++)
            error = ext4_balloc_free_blocks(t->ref, t->nodes[i], 1);
    }
    free(children);
    free(parents);
    free(buffer);
    return error;
}
void ext4_extent_tree_init(struct ext4_inode_ref *ref) {
    uint8_t *root = (uint8_t *)ref->inode->blocks;
    memset(root, 0, 60);
    header(root, 0, 4, 0);
    ref->dirty = true;
}
int ext4_extent_get_blocks(struct ext4_inode_ref *ref, ext4_lblk_t logical, uint32_t maximum,
                           ext4_fsblk_t *result, bool create, uint32_t *count) {
    *result = 0;
    if (count)
        *count = 0;
    if (!maximum)
        return EINVAL;
    struct tree t;
    int error = load(&t, ref);
    if (error)
        return error;
    uint32_t allocated = 1;
    size_t i = 0;
    while (i < t.count && (uint64_t)t.runs[i].logical + t.runs[i].length <= logical)
        i++;
    if (i < t.count && t.runs[i].logical <= logical) {
        struct run r = t.runs[i];
        uint32_t delta = logical - r.logical;
        if (!r.unwritten || !create) {
            *result = r.unwritten ? 0 : r.physical + delta;
            if (count)
                *count = maximum < r.length - delta ? maximum : r.length - delta;
            dispose(&t);
            return EOK;
        }
        size_t pieces = (delta ? 1 : 0) + 1 + (delta + 1 < r.length ? 1 : 0);
        memmove(t.runs + i + pieces, t.runs + i + 1, (t.count - i - 1) * sizeof(*t.runs));
        size_t out = i;
        if (delta)
            t.runs[out++] = (struct run){r.logical, delta, r.physical, true};
        t.runs[out++] = (struct run){logical, 1, r.physical + delta, false};
        if (delta + 1 < r.length)
            t.runs[out++] = (struct run){logical + 1, r.length - delta - 1, r.physical + delta + 1, true};
        t.count += pieces - 1;
        *result = r.physical + delta;
    } else if (create) {
        uint32_t wanted = maximum > 256 ? 256 : maximum;
        if (i < t.count && wanted > t.runs[i].logical - logical)
            wanted = t.runs[i].logical - logical;
        uint64_t goal =
            i ? t.runs[i - 1].physical + t.runs[i - 1].length : ext4_get32(&ref->fs->sb, first_data_block);
        if (goal >= ext4_sb_get_blocks_cnt(&ref->fs->sb))
            goal = ext4_get32(&ref->fs->sb, first_data_block);
        error = ext4_balloc_alloc_block(ref, goal, result);
        if (error) {
            dispose(&t);
            return error;
        }
        memmove(t.runs + i + 1, t.runs + i, (t.count - i) * sizeof(*t.runs));
        allocated = 1;
        while (allocated < wanted && *result + allocated < ext4_sb_get_blocks_cnt(&ref->fs->sb)) {
            bool available = false;
            error = ext4_balloc_try_alloc_block(ref, *result + allocated, &available);
            if (error) {
                dispose(&t);
                return error;
            }
            if (!available)
                break;
            allocated++;
        }
        t.runs[i] = (struct run){logical, allocated, *result, false};
        t.count++;
    } else {
        uint64_t hole = i < t.count ? t.runs[i].logical - logical : maximum;
        if (count)
            *count = (uint32_t)(hole < maximum ? hole : maximum);
        dispose(&t);
        return EOK;
    }
    /* Unwritten conversion splits off exactly one initialized block. */
    uint8_t *zero = calloc(allocated, t.block_size);
    /* lwext4 writes regular-file payloads directly. A dirty zero-filled cache
     * entry would later overwrite those bytes at journal checkpoint time.
     * Initialize data durably without admitting it as dirty metadata. */
    error = zero ? ext4_blocks_set_direct(ref->fs->bdev, zero, *result, allocated) : ENOMEM;
    free(zero);
    if (!error)
        error = save(&t);
    if (!error && count)
        *count = allocated;
    dispose(&t);
    return error;
}
int ext4_extent_remove_space(struct ext4_inode_ref *ref, ext4_lblk_t from, ext4_lblk_t to) {
    if (from > to)
        return EINVAL;
    struct tree t;
    int error = load(&t, ref);
    if (error)
        return error;
    struct run *keep = calloc(MAX_RECORDS + 3, sizeof(*keep));
    size_t n = 0;
    if (!keep) {
        dispose(&t);
        return ENOMEM;
    }
    for (size_t i = 0; i < t.count && !error; i++) {
        struct run r = t.runs[i];
        uint64_t start = r.logical, end = start + r.length;
        uint64_t first = start > from ? start : from, last = end < (uint64_t)to + 1 ? end : (uint64_t)to + 1;
        if (first >= last) {
            keep[n++] = r;
            continue;
        }
        if (first > start)
            keep[n++] = (struct run){r.logical, (uint32_t)(first - start), r.physical, r.unwritten};
        error = ext4_balloc_free_blocks(ref, r.physical + first - start, (uint32_t)(last - first));
        if (last < end)
            keep[n++] =
                (struct run){(uint32_t)last, (uint32_t)(end - last), r.physical + last - start, r.unwritten};
    }
    if (!error) {
        free(t.runs);
        t.runs = keep;
        t.count = n;
        error = save(&t);
    } else
        free(keep);
    dispose(&t);
    return error;
}
