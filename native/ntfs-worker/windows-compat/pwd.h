/* SPDX-License-Identifier: GPL-2.0-or-later
 * NTFS-3G's optional POSIX account mapping is unavailable in this worker.
 * Real NTFS security descriptors/inheritance still use the upstream engine.
 */
#ifndef XETCH_PWD_H
#define XETCH_PWD_H
#include <sys/types.h>
struct passwd {
    char *pw_name, *pw_passwd;
    uid_t pw_uid;
    gid_t pw_gid;
    char *pw_gecos, *pw_dir, *pw_shell;
};
struct passwd *getpwuid(uid_t uid);
struct passwd *getpwnam(const char *name);
#endif
