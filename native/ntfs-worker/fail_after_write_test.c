/* SPDX-License-Identifier: Apache-2.0
 * TEST ONLY: a protocol-compatible worker that corrupts one overlay sector,
 * acknowledges the parent's response, then exits before successful unmount.
 * Never install this executable alongside an application.
 */
#include <stdio.h>
#include <string.h>
#ifdef _WIN32
#include <io.h>
#include <fcntl.h>
#endif
int main(int argc, char **argv) {
    unsigned char header[24] = {0}, sector[512] = {0}, reply[8];
    if (argc == 2 && !strcmp(argv[1], "--version")) {
        puts("{\"engine\":\"ntfs-3g\",\"version\":\"2026.9.28\",\"protocol\":1}");
        return 0;
    }
#ifdef _WIN32
    _setmode(_fileno(stdin), _O_BINARY);
    _setmode(_fileno(stdout), _O_BINARY);
#endif
    memcpy(header, "XNT1W", 5);
    header[17] = 2;
    fwrite(header, 1, 24, stdout);
    fwrite(sector, 1, 512, stdout);
    fflush(stdout);
    if (fread(reply, 1, 8, stdin) != 8)
        return 18;
    return 17;
}
