/* Trusted, static operator fixture. Never compiled from an application repo. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

__attribute__((noinline)) static int fixture_text(void) { return 7; }

int main(int argc, char **argv) {
    if (argc != 4) return 2;
    long page = sysconf(_SC_PAGESIZE);
    if (page <= 0) return 3;
    if (!strcmp(argv[1], "tamper") || !strcmp(argv[1], "writable")) {
        uintptr_t address = (uintptr_t)fixture_text & ~((uintptr_t)page - 1);
        if (mprotect((void *)address, page, PROT_READ | PROT_WRITE | PROT_EXEC)) return 4;
        if (!strcmp(argv[1], "tamper")) {
            volatile unsigned char *text = (volatile unsigned char *)fixture_text;
            *text ^= 1;
            if (mprotect((void *)address, page, PROT_READ | PROT_EXEC)) return 5;
        }
    } else if (strcmp(argv[1], "ordinary")) {
        int file = open(argv[3], O_RDONLY);
        if (file < 0) return 6;
        int permission = !strcmp(argv[1], "outside") ? PROT_READ | PROT_EXEC : PROT_READ;
        off_t offset = 0;
        if (!strcmp(argv[1], "outside")) {
            off_t size = lseek(file, 0, SEEK_END);
            if (size <= 0) return 10;
            offset = (size - 1) & ~((off_t)page - 1);
        }
        if (mmap(NULL, page, permission, MAP_PRIVATE, file, offset) == MAP_FAILED) return 7;
        close(file);
        if (!strcmp(argv[1], "deleted") && unlink(argv[3])) return 8;
    }
    int ready = open(argv[2], O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (ready < 0 || write(ready, "R", 1) != 1) return 9;
    close(ready);
    for (;;) pause();
}
