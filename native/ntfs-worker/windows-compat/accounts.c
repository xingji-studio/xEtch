/* SPDX-License-Identifier: GPL-2.0-or-later */
#include "pwd.h"
#include "grp.h"
#include <stddef.h>
#include <errno.h>
#include <stdlib.h>
struct passwd *getpwuid(uid_t id) {
    (void)id;
    errno = ENOENT;
    return NULL;
}
struct passwd *getpwnam(const char *name) {
    (void)name;
    errno = ENOENT;
    return NULL;
}
struct group *getgrgid(gid_t id) {
    (void)id;
    errno = ENOENT;
    return NULL;
}
struct group *getgrnam(const char *name) {
    (void)name;
    errno = ENOENT;
    return NULL;
}
uid_t getuid(void) { return 1; }
gid_t getgid(void) { return 1; }
uid_t geteuid(void) { return 1; }
gid_t getegid(void) { return 1; }
/* Only used by NTFS-3G's optional fallback SID mapping, never by our inherited
 * security path. Use the CRT's system RNG rather than emulate a POSIX PRNG. */
extern int rand_s(unsigned int *value);
void srandom(unsigned int seed) { (void)seed; }
long random(void) {
    unsigned int value;
    if (rand_s(&value))
        abort();
    return (long)(value & 0x7fffffff);
}
