//! Prepared Linux filesystem/syscall guard. Runtime/source/image admission and
//! bounded query supervision are separate prerequisites for semantic facts.

use std::fs::{self, File};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::linux_runtime::{AnonymousExecutePolicy, CapturedLinuxRuntime, LinuxRuntimeRole};
use anyhow::{Result, bail};

pub const LINUX_GUARD_POLICY: &str = "linux-landlock3-seccomp-allowlist-deny-anonymous/2";
const READ_FILE: u64 = 1 << 2;
const READ_DIR: u64 = 1 << 3;
const EXECUTE: u64 = 1;
const HANDLED: u64 = (1 << 15) - 1;
const PRIVATE: u64 =
    READ_FILE | READ_DIR | (1 << 1) | (1 << 4) | (1 << 5) | (1 << 7) | (1 << 8) | (1 << 14);
const ALLOW: u32 = 0x7fff_0000;
const DENY: u32 = 0x0005_0001;

#[repr(C)]
struct Ruleset {
    handled_access_fs: u64,
}
#[repr(C, packed)]
struct PathRule {
    allowed_access: u64,
    parent_fd: i32,
}

#[repr(C)]
struct CapabilityHeader {
    version: u32,
    pid: i32,
}

/// Prepared before fork; the child callback performs no allocation or locking.
/// Read paths are permissions only: callers still authenticate their content.
pub struct LinuxGuard {
    ruleset: File,
    filters: Vec<libc::sock_filter>,
    pid_slots: Vec<usize>,
}

impl LinuxGuard {
    pub fn prepare(
        runtime: &CapturedLinuxRuntime,
        reads: &[PathBuf],
        directories: &[PathBuf],
        state: &Path,
    ) -> Result<Self> {
        if runtime.manifest.anonymous_execute != AnonymousExecutePolicy::Deny {
            bail!("Linux anonymous-execution guard extension is not accepted");
        }
        runtime.verify_unchanged()?;
        // SAFETY: identity queries have no pointer arguments or side effects.
        if unsafe { libc::geteuid() } == 0 {
            bail!("Linux guarded queries require an ordinary identity");
        }
        let metadata = fs::symlink_metadata(state)?;
        if !metadata.is_dir()
            || fs::canonicalize(state)? != state
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
            || fs::read_dir(state)?.next().is_some()
        {
            bail!("Linux guard requires a fresh owned private state directory");
        }
        // SAFETY: ABI query uses no userspace buffer and zero size.
        let abi = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, 0, 0, 1) };
        if abi < 3 {
            bail!("Linux guard requires Landlock ABI 3 or newer");
        }
        let attributes = Ruleset {
            handled_access_fs: HANDLED,
        };
        // SAFETY: initialized structure, exact size and flags zero.
        let descriptor = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &raw const attributes,
                std::mem::size_of::<Ruleset>(),
                0,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: syscall returned a fresh owned descriptor.
        let ruleset = unsafe { File::from_raw_fd(i32::try_from(descriptor)?) };
        for (name, pin) in &runtime.manifest.files {
            if pin.path.starts_with(state) {
                bail!("runtime files cannot be private writable state");
            }
            let handle = path_handle(&pin.path, false)?;
            if super::linux_runtime::FileStamp::of(&handle.metadata()?) != runtime.files[name].stamp
            {
                bail!("Linux guard runtime handle differs from retained pin");
            }
            let execute = matches!(
                pin.role,
                LinuxRuntimeRole::Clangd
                    | LinuxRuntimeRole::Node
                    | LinuxRuntimeRole::Python
                    | LinuxRuntimeRole::Loader
            );
            add_rule(
                &ruleset,
                &handle,
                READ_FILE | if execute { EXECUTE } else { 0 },
            )?;
        }
        for path in reads {
            if path.starts_with(state) {
                bail!("read inputs cannot be private writable state");
            }
            add_rule(&ruleset, &path_handle(path, false)?, READ_FILE)?;
        }
        for path in directories {
            add_rule(&ruleset, &path_handle(path, true)?, READ_DIR)?;
        }
        add_rule(&ruleset, &path_handle(state, true)?, PRIVATE)?;
        runtime.verify_unchanged()?;
        let (filters, pid_slots) = syscall_filter()?;
        Ok(Self {
            ruleset,
            filters,
            pid_slots,
        })
    }

    pub(crate) fn enforce(&mut self) -> std::io::Result<()> {
        // SAFETY: all child operations below use initialized stack/retained
        // storage and async-signal-safe syscalls; no heap allocation or locks.
        unsafe {
            if libc::getuid() == 0
                || libc::getuid() != libc::geteuid()
                || libc::getgid() != libc::getegid()
            {
                return Err(std::io::Error::from_raw_os_error(libc::EPERM));
            }
            let header = CapabilityHeader {
                version: 0x2008_0522,
                pid: 0,
            };
            let mut capabilities = [0u32; 6];
            if libc::syscall(
                libc::SYS_capget,
                &raw const header,
                capabilities.as_mut_ptr(),
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if capabilities.iter().any(|value| *value != 0) {
                return Err(std::io::Error::from_raw_os_error(libc::EPERM));
            }
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::syscall(
                    libc::SYS_landlock_restrict_self,
                    self.ruleset.as_raw_fd(),
                    0,
                ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            for slot in &self.pid_slots {
                self.filters[*slot].k = u32::try_from(libc::getpid())
                    .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
            }
            // Preserve the spawn error pipe until exec; every ambient fd then
            // closes, including prepared ruleset handles and inherited sockets.
            if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 4u32) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let program = libc::sock_fprog {
                len: u16::try_from(self.filters.len())
                    .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?,
                filter: self.filters.as_mut_ptr(),
            };
            if libc::prctl(libc::PR_SET_SECCOMP, 2, &raw const program, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

fn path_handle(path: &Path, directory: bool) -> Result<File> {
    if !path.is_absolute() || fs::canonicalize(path)? != path {
        bail!("Linux guard requires canonical absolute paths");
    }
    let handle = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = handle.metadata()?;
    if if directory {
        !metadata.is_dir()
    } else {
        !metadata.is_file()
    } {
        bail!("Linux guard path has wrong file type");
    }
    Ok(handle)
}

fn add_rule(ruleset: &File, path: &File, rights: u64) -> Result<()> {
    let rule = PathRule {
        allowed_access: rights,
        parent_fd: path.as_raw_fd(),
    };
    // SAFETY: packed UAPI structure has initialized fields and exact layout.
    if unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.as_raw_fd(),
            1,
            &raw const rule,
            0,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

const fn instruction(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}
const ALLOWED_SYSCALLS: &[libc::c_long] = &[
    libc::SYS_read,
    libc::SYS_write,
    libc::SYS_readv,
    libc::SYS_writev,
    libc::SYS_pread64,
    libc::SYS_pwrite64,
    libc::SYS_close,
    libc::SYS_openat,
    libc::SYS_newfstatat,
    libc::SYS_statx,
    libc::SYS_fstat,
    libc::SYS_lseek,
    libc::SYS_munmap,
    libc::SYS_madvise,
    libc::SYS_brk,
    libc::SYS_mremap,
    libc::SYS_getrandom,
    libc::SYS_getpid,
    libc::SYS_gettid,
    libc::SYS_getppid,
    libc::SYS_getuid,
    libc::SYS_geteuid,
    libc::SYS_getgid,
    libc::SYS_getegid,
    libc::SYS_pipe2,
    libc::SYS_dup,
    libc::SYS_dup3,
    libc::SYS_ppoll,
    libc::SYS_pselect6,
    libc::SYS_epoll_create1,
    libc::SYS_epoll_ctl,
    libc::SYS_epoll_pwait,
    libc::SYS_eventfd2,
    libc::SYS_futex,
    libc::SYS_set_robust_list,
    libc::SYS_rseq,
    libc::SYS_rt_sigaction,
    libc::SYS_rt_sigprocmask,
    libc::SYS_rt_sigreturn,
    libc::SYS_sigaltstack,
    libc::SYS_rt_sigsuspend,
    libc::SYS_rt_sigtimedwait,
    libc::SYS_restart_syscall,
    libc::SYS_nanosleep,
    libc::SYS_clock_nanosleep,
    libc::SYS_clock_gettime,
    libc::SYS_clock_getres,
    libc::SYS_times,
    libc::SYS_getrusage,
    libc::SYS_uname,
    libc::SYS_sysinfo,
    libc::SYS_getrlimit,
    libc::SYS_fstatfs,
    libc::SYS_statfs,
    libc::SYS_getcwd,
    libc::SYS_faccessat,
    libc::SYS_faccessat2,
    libc::SYS_readlinkat,
    libc::SYS_getdents64,
    libc::SYS_chdir,
    libc::SYS_fchdir,
    libc::SYS_umask,
    libc::SYS_sched_yield,
    libc::SYS_sched_getaffinity,
    libc::SYS_getcpu,
    libc::SYS_unlinkat,
    libc::SYS_mkdirat,
    libc::SYS_renameat,
    libc::SYS_renameat2,
    libc::SYS_linkat,
    libc::SYS_symlinkat,
    libc::SYS_ftruncate,
    libc::SYS_execve,
    libc::SYS_execveat,
    libc::SYS_exit,
    libc::SYS_exit_group,
];
#[cfg(target_arch = "x86_64")]
const LEGACY_SYSCALLS: &[libc::c_long] = &[
    libc::SYS_pause,
    libc::SYS_arch_prctl,
    libc::SYS_open,
    libc::SYS_stat,
    libc::SYS_lstat,
    libc::SYS_access,
    libc::SYS_readlink,
    libc::SYS_pipe,
    libc::SYS_dup2,
    libc::SYS_poll,
    libc::SYS_select,
    libc::SYS_epoll_wait,
    libc::SYS_mkdir,
    libc::SYS_unlink,
    libc::SYS_rmdir,
    libc::SYS_rename,
    libc::SYS_link,
    libc::SYS_symlink,
];

#[cfg(not(target_arch = "x86_64"))]
const LEGACY_SYSCALLS: &[libc::c_long] = &[];

fn append_case(
    filters: &mut Vec<libc::sock_filter>,
    number: libc::c_long,
    body: &[libc::sock_filter],
) -> Result<()> {
    filters.push(instruction(
        0x15,
        0,
        u8::try_from(body.len())?,
        u32::try_from(number)?,
    ));
    filters.extend_from_slice(body);
    Ok(())
}

fn syscall_filter() -> Result<(Vec<libc::sock_filter>, Vec<usize>)> {
    let architecture = if cfg!(target_arch = "x86_64") {
        0xc000_003e
    } else if cfg!(target_arch = "aarch64") {
        0xc000_00b7
    } else {
        bail!("unsupported Linux guard architecture");
    };
    let mut filters = vec![
        instruction(0x20, 0, 0, 4),
        instruction(0x15, 1, 0, architecture),
        instruction(0x06, 0, 0, 0x8000_0000),
        instruction(0x20, 0, 0, 0),
        instruction(0x35, 0, 1, 0x4000_0000),
        instruction(0x06, 0, 0, 0x8000_0000),
    ];
    let pid_slots = process_filter(&mut filters)?;
    descriptor_filter(&mut filters)?;
    memory_filter(&mut filters)?;
    append_case(
        &mut filters,
        libc::SYS_prctl,
        &[
            instruction(0x20, 0, 0, 16),
            instruction(0x15, 2, 0, u32::try_from(libc::PR_SET_NAME)?),
            instruction(0x15, 1, 0, u32::try_from(libc::PR_GET_NAME)?),
            instruction(0x06, 0, 0, DENY),
            instruction(0x06, 0, 0, ALLOW),
        ],
    )?;
    for &number in ALLOWED_SYSCALLS.iter().chain(LEGACY_SYSCALLS) {
        append_case(&mut filters, number, &[instruction(0x06, 0, 0, ALLOW)])?;
    }
    filters.push(instruction(0x06, 0, 0, DENY));
    if filters.len() > 4096 {
        bail!("Linux seccomp program exceeds kernel bound");
    }
    Ok((filters, pid_slots))
}

fn process_filter(filters: &mut Vec<libc::sock_filter>) -> Result<Vec<usize>> {
    let mut pid_slots = Vec::new();
    for number in [libc::SYS_kill, libc::SYS_tgkill] {
        pid_slots.push(filters.len() + 2);
        append_case(
            filters,
            number,
            &[
                instruction(0x20, 0, 0, 16),
                instruction(0x15, 1, 0, 0),
                instruction(0x06, 0, 0, DENY),
                instruction(0x06, 0, 0, ALLOW),
            ],
        )?;
    }
    pid_slots.push(filters.len() + 3);
    append_case(
        filters,
        libc::SYS_prlimit64,
        &[
            instruction(0x20, 0, 0, 16),
            instruction(0x15, 2, 0, 0),
            instruction(0x15, 1, 0, 0),
            instruction(0x06, 0, 0, DENY),
            instruction(0x06, 0, 0, ALLOW),
        ],
    )?;
    let thread_flags = u32::try_from(
        libc::CLONE_VM
            | libc::CLONE_FS
            | libc::CLONE_FILES
            | libc::CLONE_SIGHAND
            | libc::CLONE_THREAD
            | libc::CLONE_SYSVSEM
            | libc::CLONE_SETTLS
            | libc::CLONE_PARENT_SETTID
            | libc::CLONE_CHILD_CLEARTID
            | libc::CLONE_CHILD_SETTID,
    )?;
    append_case(
        filters,
        libc::SYS_clone,
        &[
            instruction(0x20, 0, 0, 20),
            instruction(0x15, 0, 4, 0),
            instruction(0x20, 0, 0, 16),
            instruction(0x45, 0, 2, u32::try_from(libc::CLONE_THREAD)?),
            instruction(0x45, 1, 0, !thread_flags),
            instruction(0x06, 0, 0, ALLOW),
            instruction(0x06, 0, 0, DENY),
        ],
    )?;
    append_case(
        filters,
        libc::SYS_clone3,
        &[instruction(0x06, 0, 0, 0x0005_0026)],
    )?;
    Ok(pid_slots)
}

fn descriptor_filter(filters: &mut Vec<libc::sock_filter>) -> Result<()> {
    let commands = [
        libc::F_DUPFD,
        libc::F_DUPFD_CLOEXEC,
        libc::F_GETFD,
        libc::F_SETFD,
        libc::F_GETFL,
        libc::F_GETPIPE_SZ,
        libc::F_SETPIPE_SZ,
    ];
    let mut body = vec![
        instruction(0x20, 0, 0, 24),
        instruction(
            0x15,
            u8::try_from(commands.len() + 2)?,
            0,
            u32::try_from(libc::F_SETFL)?,
        ),
    ];
    for (index, command) in commands.iter().enumerate() {
        body.push(instruction(
            0x15,
            u8::try_from(commands.len() - index)?,
            0,
            u32::try_from(*command)?,
        ));
    }
    body.extend([
        instruction(0x06, 0, 0, DENY),
        instruction(0x06, 0, 0, ALLOW),
        instruction(0x20, 0, 0, 32),
        instruction(0x45, 1, 0, u32::try_from(libc::O_ASYNC)?),
        instruction(0x06, 0, 0, ALLOW),
        instruction(0x06, 0, 0, DENY),
    ]);
    append_case(filters, libc::SYS_fcntl, &body)
}

fn memory_filter(filters: &mut Vec<libc::sock_filter>) -> Result<()> {
    // Executable protection cannot be restored after writing private code.
    append_case(
        filters,
        libc::SYS_mprotect,
        &[
            instruction(0x20, 0, 0, 32),
            instruction(0x45, 1, 0, u32::try_from(libc::PROT_EXEC)?),
            instruction(0x06, 0, 0, ALLOW),
            instruction(0x06, 0, 0, DENY),
        ],
    )?;
    append_case(
        filters,
        libc::SYS_mmap,
        &[
            instruction(0x20, 0, 0, 32),
            instruction(0x45, 0, 3, u32::try_from(libc::PROT_EXEC)?),
            instruction(0x45, 3, 0, u32::try_from(libc::PROT_WRITE)?),
            instruction(0x20, 0, 0, 40),
            instruction(0x45, 1, 0, u32::try_from(libc::MAP_ANONYMOUS)?),
            instruction(0x06, 0, 0, ALLOW),
            instruction(0x06, 0, 0, DENY),
        ],
    )?;
    Ok(())
}
