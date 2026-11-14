/* SPDX-License-Identifier: GPL-2.0-or-later
 * Upstream ioctl.c includes syslog.h unconditionally but uses no syslog API.
 * The worker sends library diagnostics to stderr using ntfs_log_set_handler.
 */
