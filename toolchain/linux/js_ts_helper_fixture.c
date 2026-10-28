/* Trusted independent protocol producer. It never loads application source. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void emit(const char *value) {
    size_t length = strlen(value);
    if (write(1, value, length) != (ssize_t)length) exit(20);
}
static void control(char expected) {
    char byte;
    if (read(0, &byte, 1) != 1 || byte != expected) exit(21);
}
static void hang(void) { for (;;) pause(); }
int main(int argc, char **argv) {
    if (argc < 2) return 2;
    const char *mode = argc == 2 ? argv[1] : strrchr(argv[0], '/') + 1;
    if (!strcmp(mode, "ready-version")) {
        emit("{\"phase\":\"ready\",\"node\":\"v99.0.0\",\"typescript\":\"5.9.3\"}\n"); hang();
    }
    if (!strcmp(mode, "result-schema")) {
        emit("{\"phase\":\"ready\",\"node\":\"v22.22.2\",\"typescript\":\"5.9.3\"}\n");
        control('1');
        emit("{\"phase\":\"result\",\"schema\":\"wrong\",\"compiler\":\"5.9.3\",\"node\":\"v22.22.2\",\"project\":\"tsconfig.json\",\"options\":{},\"program_files\":[],\"inputs\":{},\"lookups\":[],\"sites\":[],\"aliases\":[],\"diagnostics\":[],\"program_diagnostics\":{\"options\":0,\"global\":0,\"syntactic\":0}}\n");
        hang();
    }
    if (!strcmp(mode, "silent")) hang();
    if (!strcmp(mode, "ready-eof")) return 0;
    if (!strcmp(mode, "ready-partial")) { emit("{\"phase\":"); hang(); }
    if (!strcmp(mode, "ready-large")) {
        char bytes[4097]; memset(bytes, 'x', sizeof(bytes));
        if (write(1, bytes, sizeof(bytes)) != sizeof(bytes)) return 3;
        hang();
    }
    if (!strcmp(mode, "extra-frame")) { emit("{}\n{}\n"); hang(); }
    if (!strcmp(mode, "stderr-large")) {
        char bytes[4096]; memset(bytes, 'x', sizeof(bytes));
        for (int index = 0; index < 257; index++)
            if (write(2, bytes, sizeof(bytes)) != sizeof(bytes)) return 4;
        hang();
    }
    if (!strcmp(mode, "fragmented")) {
        const char *message = "{\"phase\":\"ready\"}\n";
        for (size_t index = 0; index < strlen(message); index++) {
            if (write(1, message + index, 1) != 1) return 5;
            usleep(1000);
        }
    } else emit("{\"phase\":\"ready\"}\n");
    control('1');
    if (!strcmp(mode, "result-eof")) return 0;
    if (!strcmp(mode, "control-eof")) close(0);
    if (!strcmp(mode, "result-large")) {
        char bytes[8192]; memset(bytes, 'x', sizeof(bytes));
        for (int index = 0; index < 16385; index++)
            if (write(1, bytes, sizeof(bytes)) != sizeof(bytes)) return 6;
        hang();
    }
    if (!strcmp(mode, "result-partial")) { emit("{"); hang(); }
    emit("{\"result\":true}\n");
    if (!strcmp(mode, "control-eof")) hang();
    control('2');
    if (!strcmp(mode, "extra-output")) emit("x");
    if (!strcmp(mode, "failed-exit")) return 9;
    if (!strcmp(mode, "closed-output-live")) { close(1); hang(); }
    return 0;
}
