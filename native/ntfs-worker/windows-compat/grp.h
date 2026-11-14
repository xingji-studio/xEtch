/* SPDX-License-Identifier: GPL-2.0-or-later */
#ifndef XETCH_GRP_H
#define XETCH_GRP_H
#include <sys/types.h>
struct group {
    char *gr_name, *gr_passwd;
    gid_t gr_gid;
    char **gr_mem;
};
struct group *getgrgid(gid_t gid);
struct group *getgrnam(const char *name);
#endif
