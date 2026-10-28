/* Trusted synthetic protocol fixture. It never loads application code. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <unistd.h>

static void wait_forever(void) { for (;;) pause(); }
static void bytes(const char *value, int fragmented) {
    size_t size = strlen(value);
    if (fragmented) {
        for (size_t index = 0; index < size; index++) {
            if (write(1, value + index, 1) != 1) exit(20);
            usleep(1000);
        }
    } else if (write(1, value, size) != (ssize_t)size) exit(21);
}
static void frame(const char *body, int fragmented) {
    char header[128];
    snprintf(header, sizeof(header), "Content-Length: %zu\r\n\r\n", strlen(body));
    bytes(header, fragmented);
    bytes(body, fragmented);
}
static char *message(void) {
    char line[128];
    unsigned long size = 0;
    while (fgets(line, sizeof(line), stdin)) {
        if (!strcmp(line, "\r\n")) break;
        unsigned long declared;
        if (sscanf(line, "Content-Length: %lu", &declared) == 1) size = declared;
    }
    if (feof(stdin) || size > 4 * 1024 * 1024) exit(22);
    char *body = calloc(size + 1, 1);
    if (!body || fread(body, 1, size, stdin) != size) exit(23);
    return body;
}
static long long id(const char *body) {
    const char *start = strstr(body, "\"id\":");
    long long result;
    if (!start || sscanf(start, "\"id\":%lld", &result) != 1) exit(24);
    return result;
}
static void answer(long long request, const char *mode) {
    char body[512];
    if (!strcmp(mode, "unknown")) request = 999;
    if (!strcmp(mode, "noninteger")) strcpy(body, "{\"jsonrpc\":\"2.0\",\"id\":\"x\",\"result\":true}");
    else if (!strcmp(mode, "missing")) snprintf(body, sizeof(body), "{\"jsonrpc\":\"2.0\",\"id\":%lld}", request);
    else if (!strcmp(mode, "error")) snprintf(body, sizeof(body), "{\"jsonrpc\":\"2.0\",\"id\":%lld,\"error\":{\"code\":-1,\"message\":\"fixture error\"}}", request);
    else if (!strcmp(mode, "envelope")) strcpy(body, "[]");
    else snprintf(body, sizeof(body), "{\"jsonrpc\":\"2.0\",\"id\":%lld,\"result\":true}", request);
    frame(body, !strcmp(mode, "fragmented"));
}
int main(int argc, char **argv) {
    if (argc != 3) return 2;
    const char *mode = argv[1];
    if (!strcmp(mode, "closed-input")) close(0);
    int ready = open(argv[2], O_WRONLY | O_CREAT | O_EXCL, 0600);
    char record[64];
    int length = snprintf(record,sizeof(record),"R %ld",(long)getpid());
    if (ready < 0 || write(ready, record, length) != length) return 3;
    close(ready);
    if (!strcmp(mode, "dynamic-extra")) {
        char path[4096];
        snprintf(path, sizeof(path), "%s.extra", argv[2]);
        int file = open(path, O_RDWR | O_CREAT | O_EXCL, 0600);
        if (file < 0 || ftruncate(file, 4096)) return 7;
        if (mmap(NULL, 4096, PROT_READ, MAP_PRIVATE, file, 0) == MAP_FAILED) return 8;
        close(file);
    }
    if (!strcmp(mode, "silent") || !strcmp(mode, "closed-input")) wait_forever();
    if (!strcmp(mode, "closed-output")) { close(1); wait_forever(); }
    for (;;) {
        char *body = message();
        if (strstr(body, "\"method\":\"exit\"")) { free(body); wait_forever(); }
        long long request = id(body);
        free(body);
        if (!strcmp(mode, "dynamic-guard")) {
            char *const args[] = {"/usr/bin/true", NULL};
            char *const environment[] = {NULL};
            execve(args[0], args, environment);
            int execution_denied = errno == EACCES;
            int network = socket(AF_INET, SOCK_STREAM, 0);
            int network_denied = network < 0 && errno == EPERM;
            if (network >= 0) close(network);
            char outside[128];
            snprintf(outside, sizeof(outside), "/tmp/codemap-dynamic-outside-%ld", (long)getpid());
            int file = open(outside, O_WRONLY | O_CREAT | O_EXCL, 0600);
            int write_denied = file < 0 && errno == EACCES;
            if (file >= 0) { close(file); unlink(outside); }
            char reply[256];
            snprintf(reply, sizeof(reply), "{\"jsonrpc\":\"2.0\",\"id\":%lld,\"result\":{\"execution_denied\":%s,\"network_denied\":%s,\"write_denied\":%s}}", request, execution_denied ? "true" : "false", network_denied ? "true" : "false", write_denied ? "true" : "false");
            frame(reply, 0);
            continue;
        }
        if (!strcmp(mode, "receive")) wait_forever();
        if (!strcmp(mode, "partial-header")) { bytes("Content-Length:",0); wait_forever(); }
        if (!strcmp(mode, "partial-body")) { bytes("Content-Length: 100\r\n\r\nx",0); wait_forever(); }
        if (!strcmp(mode, "exit")) return 0;
        if (!strcmp(mode, "header")) { char huge[9000]; memset(huge, 'x', sizeof(huge)-1); huge[sizeof(huge)-1]=0; bytes(huge, 0); wait_forever(); }
        if (!strcmp(mode, "body")) { bytes("Content-Length: 4194305\r\n\r\n",0); wait_forever(); }
        if (!strcmp(mode, "truncated")) { bytes("Content-Length: 100\r\n\r\nx",0); close(1); wait_forever(); }
        if (!strcmp(mode, "json")) { frame("{",0); wait_forever(); }
        if (!strcmp(mode, "wire")) { char spaces[256]; memset(spaces,' ',sizeof(spaces)-1); spaces[sizeof(spaces)-1]=0; frame(spaces,0); wait_forever(); }
        if (!strcmp(mode, "diagnostic")) { if (write(2,"diagnostic",10) != 10) return 4; }
        if (!strcmp(mode, "stderr")) { char data[8192]; memset(data,'x',sizeof(data)); for (int i=0;i<256;i++) if (write(2,data,sizeof(data)) != sizeof(data)) return 5; }
        if (!strcmp(mode, "notify") || !strcmp(mode, "queue") || !strcmp(mode,"frames")) {
            int count = !strcmp(mode,"notify") ? 1 : 3;
            for(int i=0;i<count;i++) frame("{\"jsonrpc\":\"2.0\",\"method\":\"$/progress\"}",0);
        }
        if (!strcmp(mode, "server")) {
            frame("{\"jsonrpc\":\"2.0\",\"id\":\"operator\",\"method\":\"fixture/unsupported\"}",0);
            char *reply = message();
            if (!strstr(reply,"-32601") || !strstr(reply,"\"operator\"")) return 6;
            free(reply);
        }
        if (!strcmp(mode,"reverse") || !strcmp(mode,"buffered")) {
            char *next = message(); long long second = id(next); free(next);
            answer(second,"echo"); answer(request,"echo");
        } else {
            answer(request,mode);
            if (!strcmp(mode,"duplicate")) answer(request,"echo");
        }
    }
}
