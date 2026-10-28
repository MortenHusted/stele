/* Trusted operator fixture: dump own kernel image or wait for parent inspection. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    if (!strcmp(argv[1], "dump")) {
        FILE *maps = fopen("/proc/self/maps", "r");
        if (!maps) return 3;
        char line[1024];
        uintptr_t address = getauxval(AT_SYSINFO_EHDR);
        while (fgets(line, sizeof(line), maps)) {
            unsigned long start, end;
            if (sscanf(line, "%lx-%lx", &start, &end) == 2 && start == address && strstr(line, "[vdso]")) {
                fclose(maps);
                FILE *output = fopen(argv[2], "wb");
                if (!output) return 4;
                size_t size = end - start;
                if (fwrite((void *)start, 1, size, output) != size || fclose(output)) return 5;
                return 0;
            }
        }
        fclose(maps);
        return 6;
    }
    if (strcmp(argv[1], "wait")) return 7;
    int ready = open(argv[2], O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (ready < 0 || write(ready, "R", 1) != 1) return 8;
    close(ready);
    for (;;) pause();
}
