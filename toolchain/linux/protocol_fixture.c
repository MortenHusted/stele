/* Trusted synthetic LSP frames; never application code. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static void output(const char *text) {
    size_t length = strlen(text);
    while (length) {
        ssize_t count = write(1, text, length);
        if (count <= 0) _exit(20);
        text += count; length -= count;
    }
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    int ready = open(argv[2], O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (ready < 0) return 3;
    if (!strcmp(argv[1], "closed-input")) close(0);
    if (write(ready, "R", 1) != 1) return 3;
    close(ready);
    const char *mode = argv[1];
    if (!strcmp(mode, "valid") || !strcmp(mode, "multiple") || !strcmp(mode, "fragmented")) {
        const char *body = "{\"id\":1,\"result\":true}";
        char header[128]; snprintf(header, sizeof(header), "Content-Length: %zu\r\n\r\n", strlen(body));
        if (!strcmp(mode, "fragmented")) {
            for (const char *part = header; *part; ++part) { if (write(1, part, 1) != 1) return 4; usleep(1000); }
            for (const char *part = body; *part; ++part) { if (write(1, part, 1) != 1) return 5; usleep(1000); }
        } else { output(header); output(body); }
        if (!strcmp(mode, "multiple")) { output(header); output(body); }
        if (write(2, "diagnostic", 10) != 10) return 6;
    } else if (!strcmp(mode, "header")) {
        char header[256]; memset(header, 'x', sizeof(header)); if (write(1, header, sizeof(header)) != sizeof(header)) return 7;
    } else if (!strcmp(mode, "body")) output("Content-Length: 8192\r\n\r\n");
    else if (!strcmp(mode, "duplicate")) output("Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}");
    else if (!strcmp(mode, "length")) output("Content-Length: q\r\n\r\n");
    else if (!strcmp(mode, "missing")) output("Content-Type: text/plain\r\n\r\n{}");
    else if (!strcmp(mode, "partial-header")) output("Content-Length: 2");
    else if (!strcmp(mode, "partial-body")) output("Content-Length: 20\r\n\r\n{}");
    else if (!strcmp(mode, "json")) output("Content-Length: 2\r\n\r\nxx");
    else if (!strcmp(mode, "utf8")) output("Content-Length: \377\r\n\r\n");
    else if (!strcmp(mode, "stalled-header")) output("Content-Length:");
    else if (!strcmp(mode, "stalled-body")) output("Content-Length: 20\r\n\r\n{}");
    else if (!strcmp(mode, "closed-input")) close(0);
    else if (!strcmp(mode, "write")) {
        char header[128], blank[4], input[4096]; size_t count;
        if (!fgets(header, sizeof(header), stdin) || sscanf(header, "Content-Length: %zu", &count) != 1 || count > sizeof(input)) return 8;
        if (!fgets(blank, sizeof(blank), stdin) || strcmp(blank, "\r\n") || fread(input, 1, count, stdin) != count) return 9;
        output(header); output(blank); if (write(1, input, count) != (ssize_t)count) return 11;
        for (;;) pause();
    }
    if (!strcmp(mode, "partial-header") || !strcmp(mode, "partial-body")) {
        char input; if (read(0, &input, 1) != 1) return 10;
        close(1);
    }
    for (;;) pause();
}
