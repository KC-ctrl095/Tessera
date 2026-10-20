/* Loader regression. Reports every process in which our constructor runs.
 * All files are in a disposable regression folder, never real data. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
__attribute__((constructor)) static void loaded(void) {
    const char *marker = getenv("TESSERA_CONSTRUCTOR_MARKER");
    if (!marker) return;
    int out = open(marker, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (out < 0) return;
    int in = open("/proc/self/cmdline", O_RDONLY);
    char buf[4096];
    int n = in < 0 ? 0 : read(in, buf, sizeof(buf)-1);
    if (n > 0) {
        for (int i=0; i<n; i++) if (!buf[i]) buf[i]=' ';
        write(out, buf, n);
    }
    write(out, "\n", 1);
    if (in >= 0) close(in);
    close(out);
}
