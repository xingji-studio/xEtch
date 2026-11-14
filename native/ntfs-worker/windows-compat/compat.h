/* SPDX-License-Identifier: GPL-2.0-or-later
 * POSIX account/mode vocabulary needed by NTFS-3G's descriptor algorithms.
 * It does not map NTFS permissions to the host or grant host access.
 */
#ifndef XETCH_WINDOWS_COMPAT_H
#define XETCH_WINDOWS_COMPAT_H
#include <sys/types.h>
#include <sys/stat.h>
typedef unsigned int uid_t;
typedef unsigned int gid_t;
#ifndef S_ISUID
#define S_ISUID 04000
#define S_ISGID 02000
#define S_ISVTX 01000
#endif
#ifndef S_IRGRP
#define S_IRGRP 00040
#define S_IWGRP 00020
#define S_IXGRP 00010
#define S_IROTH 00004
#define S_IWOTH 00002
#define S_IXOTH 00001
#define S_IRWXU 00700
#define S_IRWXG 00070
#define S_IRWXO 00007
#endif
#ifndef S_IFLNK
#define S_IFLNK 0120000
#define S_IFSOCK 0140000
#endif
#ifndef S_ISLNK
#define S_ISLNK(mode) (((mode) & S_IFMT) == S_IFLNK)
#define S_ISSOCK(mode) (((mode) & S_IFMT) == S_IFSOCK)
#endif
#ifndef major
#define major(dev) ((unsigned int)(((dev) >> 8) & 0xfff))
#define minor(dev) ((unsigned int)((dev) & 0xff))
#define makedev(major, minor) (((major) << 8) | (minor))
#endif
uid_t getuid(void);
uid_t geteuid(void);
gid_t getgid(void);
gid_t getegid(void);
long random(void);
void srandom(unsigned int seed);
#endif
