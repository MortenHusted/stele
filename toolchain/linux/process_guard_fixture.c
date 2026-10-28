#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/prctl.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/ptrace.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

static FILE *report;
static void record(const char *name, const char *kind, int result) { fprintf(report, "%s=%d:%s\n", name, !!result, kind); }
static int denied(long result, int expected) { if (result >= 0) close((int)result); return result == -1 && errno == expected; }
static int blocked_open(const char *path, int flags) { return denied(open(path, flags, 0600), EACCES); }
static void *thread(void *value) { *(int *)value = 1; return NULL; }
static int copy_file(const char *source, const char *destination) {
    int input = open(source, O_RDONLY);
    int output = open(destination, O_WRONLY | O_CREAT | O_EXCL, 0700);
    if (input < 0 || output < 0) return -1;
    char buffer[4096]; ssize_t count;
    while ((count = read(input, buffer, sizeof buffer)) > 0) if (write(output, buffer, count) != count) return -1;
    close(input); close(output); return count == 0 ? 0 : -1;
}
int main(int argc, char **argv) {
    if (argc != 7) return 2;
    errno = 0; int file_closed = fcntl(atoi(argv[4]), F_GETFD) == -1 && errno == EBADF;
    errno = 0; int socket_closed = fcntl(atoi(argv[5]), F_GETFD) == -1 && errno == EBADF;
    char path[4096], destination[4096];
    snprintf(path, sizeof path, "%s/report", argv[3]);
    report = fopen(path, "wx"); if (!report) return 3;
    record("selected_executable", "accept", 1);
    record("inherited_file_descriptor", "closed", file_closed);
    record("inherited_socket_descriptor", "closed", socket_closed);
    char buffer[16] = {0}; int fd = open(argv[1], O_RDONLY);
    record("pinned_read", "accept", fd >= 0 && read(fd, buffer, 8) == 8 && !memcmp(buffer, "original", 8));
    if (fd >= 0) close(fd);
    snprintf(path, sizeof path, "%s/private", argv[3]);
    fd = open(path, O_WRONLY | O_CREAT | O_EXCL, 0600);
    record("private_write", "accept", fd >= 0 && write(fd, "private", 7) == 7); if (fd >= 0) close(fd);
    record("outside_read", "reject", blocked_open(argv[2], O_RDONLY));
    record("outside_write", "reject", blocked_open(argv[2], O_WRONLY));
    record("pinned_truncate", "reject", blocked_open(argv[1], O_RDONLY | O_TRUNC));
    snprintf(path, sizeof path, "%s.new", argv[2]);
    record("outside_create", "reject", blocked_open(path, O_WRONLY | O_CREAT));
    record("outside_remove", "reject", unlink(argv[2]) == -1 && errno == EACCES);
    record("outside_rename", "reject", rename(argv[2], path) == -1 && errno == EACCES);
    snprintf(path, sizeof path, "%s/link", argv[3]);
    record("cross_boundary_link", "reject", link(argv[1], path) == -1 && errno == EXDEV);
    char *args[] = {"true", NULL}; char *environment[] = {NULL};
    record("unapproved_exec", "reject", syscall(SYS_execve, "/usr/bin/true", args, environment) == -1 && errno == EACCES);
    snprintf(destination, sizeof destination, "%s/copied-tool", argv[3]);
    int copied = copy_file(argv[6], destination) == 0;
    record("private_state_exec", "reject", copied && syscall(SYS_execve, destination, args, environment) == -1 && errno == EACCES);
    record("tcp_socket", "reject", denied(socket(AF_INET, SOCK_STREAM, 0), EPERM));
    record("udp_socket", "reject", denied(socket(AF_INET, SOCK_DGRAM, 0), EPERM));
    record("unix_socket", "reject", denied(socket(AF_UNIX, SOCK_STREAM, 0), EPERM));
    int pair[2]; record("socketpair", "reject", socketpair(AF_UNIX, SOCK_STREAM, 0, pair) == -1 && errno == EPERM);
    record("io_uring", "reject", syscall(SYS_io_uring_setup, 1, NULL) == -1 && errno == EPERM);
    record("auxv_map_rewrite_guarded", "reject", prctl(PR_SET_MM, PR_SET_MM_MAP, NULL, sizeof(struct prctl_mm_map), 0) == -1 && errno == EPERM);
    record("auxv_direct_rewrite", "reject", prctl(PR_SET_MM, PR_SET_MM_AUXV, NULL, 0, 0) == -1 && errno == EPERM);
    pthread_t worker; int value = 0;
    int created = pthread_create(&worker, NULL, thread, &value);
    record("thread_clone", "accept", created == 0 && pthread_join(worker, NULL) == 0 && value == 1);
    record("process_clone", "reject", syscall(SYS_clone, SIGCHLD, NULL, NULL, NULL, 0) == -1 && errno == EPERM);
    record("fork", "reject", fork() == -1 && errno == EPERM);
    record("clone3", "enosys", syscall(SYS_clone3, NULL, 0) == -1 && errno == ENOSYS);
    record("setsid", "reject", setsid() == -1 && errno == EPERM);
    record("setpgid", "reject", setpgid(0, getppid()) == -1 && errno == EPERM);
    record("unshare", "reject", unshare(CLONE_NEWUSER) == -1 && errno == EPERM);
    record("setns", "reject", setns(-1, CLONE_NEWUSER) == -1 && errno == EPERM);
    record("ptrace", "reject", ptrace(PTRACE_TRACEME, 0, NULL, NULL) == -1 && errno == EPERM);
    char target = 'A', replacement = 'B';
    struct iovec local = {&replacement, 1}, remote = {&target, 1};
    record("process_vm_writev", "reject", process_vm_writev(getpid(), &local, 1, &remote, 1, 0) == -1 && errno == EPERM && target == 'A');
    void *memory = mmap(NULL, 4096, PROT_READ | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    record("anonymous_executable_mmap", "reject", memory == MAP_FAILED && errno == EPERM);
    if (memory != MAP_FAILED) munmap(memory, 4096);
    memory = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    record("executable_mprotect", "reject", memory != MAP_FAILED && mprotect(memory, 4096, PROT_READ | PROT_EXEC) == -1 && errno == EPERM);
    if (memory != MAP_FAILED) munmap(memory, 4096);
    record("unknown_syscall", "reject", syscall(0x3fffffff) == -1 && errno == EPERM);
    struct rlimit limits;
    int got_limits = getrlimit(RLIMIT_NOFILE, &limits) == 0;
    record("parent_resource_limit", "reject", got_limits && prlimit(getppid(), RLIMIT_NOFILE, &limits, NULL) == -1 && errno == EPERM);
    int descriptor_pipe[2];
    int piped = pipe(descriptor_pipe) == 0;
    record("fcntl_signal_owner", "reject", piped && fcntl(descriptor_pipe[0], F_SETOWN, getppid()) == -1 && errno == EPERM);
    if (piped) { close(descriptor_pipe[0]); close(descriptor_pipe[1]); }
    piped = pipe(descriptor_pipe) == 0;
    int flags = piped ? fcntl(descriptor_pipe[0], F_GETFL) : -1;
    record("fcntl_async_signal", "reject", flags >= 0 && fcntl(descriptor_pipe[0], F_SETFL, flags | O_ASYNC) == -1 && errno == EPERM);
    if (piped) { close(descriptor_pipe[0]); close(descriptor_pipe[1]); }
    record("outside_signal", "reject", kill(getppid(), 0) == -1 && errno == EPERM);
    record("proc_self_memory_write", "reject", blocked_open("/proc/self/mem", O_WRONLY));
    fprintf(report, "DONE\n"); if (fclose(report)) return 4;
    for (;;) pause();
}
