/* Trusted operator fixture for pipe ownership and cleanup, not application code. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    int ready = open(argv[2], O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (ready < 0 || write(ready, "R", 1) != 1) return 3;
    close(ready);
    if (!strcmp(argv[1], "flood")) {
        char output[4096]; memset(output, 'x', sizeof(output));
        for (;;) if (write(1, output, sizeof(output)) <= 0) return 4;
    }
    if (!strcmp(argv[1], "echo") || !strcmp(argv[1], "exit")) {
        char input;
        if (read(0, &input, 1) != 1 || input != 'Q') return 5;
        if (!strcmp(argv[1], "exit")) return 0;
        if (write(1, "answer", 6) != 6 || write(2, "error", 5) != 5) return 6;
    }
    for (;;) pause();
}
