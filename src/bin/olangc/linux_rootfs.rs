//! Linux execution boundary for embedded runtime filesystem images.
//!
//! The kernel and the caller's three standard streams remain explicit inputs.
//! Mount-changing syscalls are unavailable after entering the immutable image.
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::ExitStatus;

pub const ACTIVE_ENV: &str = "O_EMBEDDED_ROOTFS_IMAGE";

#[cfg(target_os = "linux")]
pub fn verify_active(image: &str) -> Result<bool> {
    let Some(active) = std::env::var_os(ACTIVE_ENV) else {
        return Ok(false);
    };
    if active != image || std::fs::read_to_string("/.ostadix/image.sha256")? != image {
        bail!("rootfs activation does not match the embedded image");
    }
    for map in ["/proc/self/uid_map", "/proc/self/gid_map"] {
        let contents = std::fs::read_to_string(map)?;
        let fields = contents.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || fields[0] != "0" || fields[2] != "1" {
            bail!("rootfs entry requires a single-identity user namespace");
        }
    }
    let status = std::fs::read_to_string("/proc/self/status")?;
    for field in ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"] {
        let value = status
            .lines()
            .find_map(|line| line.strip_prefix(field))
            .context("rootfs process capability status is unavailable")?;
        if u64::from_str_radix(value.trim(), 16)? != 0 {
            bail!("rootfs entry retained process capabilities");
        }
    }
    // SAFETY: read-only process-local prctl query with no pointer arguments.
    if unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } != 1 {
        bail!("rootfs entry requires no_new_privs");
    }
    // Reinstall our exact filter at every multicall entry; an environment
    // string or an unrelated existing seccomp filter is never sufficient.
    install_mount_filter()?;
    Ok(true)
}

#[cfg(not(target_os = "linux"))]
pub fn verify_active(_image: &str) -> Result<bool> {
    bail!("embedded rootfs execution requires Linux user, mount, PID and network namespaces")
}

#[cfg(target_os = "linux")]
fn install_mount_filter() -> std::io::Result<()> {
    install_prepared_mount_filter(&mut prepare_mount_filter()?)
}

#[cfg(target_os = "linux")]
fn prepare_mount_filter() -> std::io::Result<Vec<libc::sock_filter>> {
    use libc::sock_filter;
    const LOAD: u16 = 0x20;
    const JEQ: u16 = 0x15;
    const RET: u16 = 0x06;
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xc000003e;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xc00000b7;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    const ARCH: u32 = 0;
    if ARCH == 0 {
        return Err(std::io::Error::from_raw_os_error(libc::ENOTSUP));
    }
    let statement = |code, k| sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    };
    let mut filter = vec![
        statement(LOAD, 4),
        sock_filter {
            code: JEQ,
            jt: 1,
            jf: 0,
            k: ARCH,
        },
        statement(RET, 0x80000000), // SECCOMP_RET_KILL_PROCESS
        statement(LOAD, 0),
    ];
    #[cfg(target_arch = "x86_64")]
    filter.extend([
        sock_filter {
            code: 0x35,
            jt: 0,
            jf: 1,
            k: 0x40000000,
        },
        statement(RET, 0x00050000 | libc::ENOSYS as u32),
    ]);
    for syscall in [
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_setns,
        libc::SYS_open_tree,
        libc::SYS_move_mount,
        libc::SYS_fsopen,
        libc::SYS_fsconfig,
        libc::SYS_fsmount,
        libc::SYS_mount_setattr,
    ] {
        filter.push(sock_filter {
            code: JEQ,
            jt: 0,
            jf: 1,
            k: syscall as u32,
        });
        filter.push(statement(RET, 0x00050000 | libc::EPERM as u32));
    }
    filter.push(statement(RET, 0x7fff0000)); // SECCOMP_RET_ALLOW
    Ok(filter)
}

#[cfg(target_os = "linux")]
fn install_prepared_mount_filter(filter: &mut [libc::sock_filter]) -> std::io::Result<()> {
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_mut_ptr(),
    };
    // SAFETY: the kernel copies this valid BPF slice before prctl returns.
    if unsafe { libc::prctl(libc::PR_SET_SECCOMP, 2, &program, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn launch(root: &Path, image: &str) -> Result<ExitStatus> {
    use std::ffi::CString;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    for descriptor in 0..3 {
        // Standard streams are explicit byte channels, not ambient directory
        // capabilities that could retain access outside the detached root.
        let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(descriptor, &mut metadata) } == 0
            && metadata.st_mode & libc::S_IFMT == libc::S_IFDIR
        {
            bail!("rootfs standard stream {descriptor} is a directory");
        }
    }
    let root = root.canonicalize()?;
    for directory in [".ostadix", ".old-root", "proc", "dev", "tmp", "work", "run"] {
        let path = root.join(directory);
        if path.exists() {
            bail!("reserved rootfs execution path already exists: {directory}");
        }
        std::fs::create_dir(&path)?;
    }
    let runner = root.join(".ostadix/runner");
    std::fs::copy(std::env::current_exe()?, &runner)?;
    std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755))?;
    std::fs::write(root.join(".ostadix/image.sha256"), image)?;
    for device in ["null", "zero", "random", "urandom"] {
        std::fs::write(root.join("dev").join(device), [])?;
    }
    for (name, target) in [
        ("fd", "/proc/self/fd"),
        ("stdin", "/proc/self/fd/0"),
        ("stdout", "/proc/self/fd/1"),
        ("stderr", "/proc/self/fd/2"),
    ] {
        std::os::unix::fs::symlink(target, root.join("dev").join(name))?;
    }
    std::fs::create_dir(root.join("dev/shm"))?;
    let root_c = CString::new(root.as_os_str().as_encoded_bytes())?;
    // Everything used before exec is allocated in the parent. The pre_exec
    // closure performs only syscalls, stack operations and errno construction.
    let root_proc = CString::new(root.join("proc").as_os_str().as_encoded_bytes())?;
    let root_old = CString::new(root.join(".old-root").as_os_str().as_encoded_bytes())?;
    let mounts = ["tmp", "work", "run", "dev/shm"]
        .into_iter()
        .map(|path| CString::new(root.join(path).as_os_str().as_encoded_bytes()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let devices = ["null", "zero", "random", "urandom"]
        .into_iter()
        .map(|name| {
            Ok((
                CString::new(format!("/dev/{name}"))?,
                CString::new(root.join("dev").join(name).as_os_str().as_encoded_bytes())?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    // SAFETY: these getters have no side effects or pointer requirements.
    let (uid, gid, parent) = unsafe { (libc::getuid(), libc::getgid(), libc::getpid()) };
    let uid_map = format!("0 {uid} 1\n").into_bytes();
    let gid_map = format!("0 {gid} 1\n").into_bytes();
    let mut mount_filter = prepare_mount_filter()?;
    let mut command = Command::new("/.ostadix/runner");
    command
        .args(std::env::args_os().skip(1))
        .env_clear()
        .env(ACTIVE_ENV, image)
        .env("PATH", "/bin")
        .env("HOME", "/work")
        .env("TMPDIR", "/tmp")
        .env("XDG_CACHE_HOME", "/work/.cache")
        .env("LANG", "C.UTF-8")
        .env("TZ", "UTC");
    // SAFETY: all allocation happened above; no locks or Rust I/O are used
    // between fork and exec. Namespace changes affect only the child.
    unsafe {
        command.pre_exec(move || {
            fn checked(value: libc::c_int) -> std::io::Result<()> {
                if value < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            }
            unsafe fn write_map(path: &[u8], bytes: &[u8]) -> std::io::Result<()> {
                let fd = libc::open(path.as_ptr().cast(), libc::O_WRONLY | libc::O_CLOEXEC);
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let mut done = 0;
                while done < bytes.len() {
                    let count = libc::write(fd, bytes[done..].as_ptr().cast(), bytes.len() - done);
                    if count < 0 && *libc::__errno_location() == libc::EINTR {
                        continue;
                    }
                    if count <= 0 {
                        let error = std::io::Error::last_os_error();
                        libc::close(fd);
                        return Err(error);
                    }
                    done += count as usize;
                }
                libc::close(fd);
                Ok(())
            }
            unsafe fn close_except(kept: &[libc::c_int]) -> std::io::Result<()> {
                let mut first = 3u32;
                for &descriptor in kept {
                    if descriptor < 3 {
                        continue;
                    }
                    let descriptor = descriptor as u32;
                    if first < descriptor {
                        checked(
                            libc::syscall(libc::SYS_close_range, first, descriptor - 1, 0u32)
                                as i32,
                        )?;
                    }
                    first = descriptor + 1;
                }
                checked(libc::syscall(libc::SYS_close_range, first, u32::MAX, 0u32) as i32)
            }
            unsafe fn private_pipe(flags: libc::c_int) -> std::io::Result<[libc::c_int; 2]> {
                let mut descriptors = [-1; 2];
                checked(libc::pipe2(
                    descriptors.as_mut_ptr(),
                    flags | libc::O_CLOEXEC,
                ))?;
                // A caller may have closed a standard stream. Keep internal
                // channels above it so descriptor cleanup never retains them
                // as if they were an explicitly supplied standard stream.
                for descriptor in &mut descriptors {
                    if *descriptor < 3 {
                        let replacement = libc::fcntl(*descriptor, libc::F_DUPFD_CLOEXEC, 3);
                        checked(replacement)?;
                        libc::close(*descriptor);
                        *descriptor = replacement;
                    }
                }
                Ok(descriptors)
            }
            extern "C" fn forward_termination(signal: libc::c_int) {
                // This handler runs only in namespace PID 1. kill(-1) cannot
                // reach an ancestor namespace and excludes PID 1 itself.
                unsafe {
                    let saved_errno = *libc::__errno_location();
                    libc::kill(-1, signal);
                    *libc::__errno_location() = saved_errno;
                }
            }
            checked(libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0))?;
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            checked(libc::unshare(libc::CLONE_NEWUSER))?;
            write_map(b"/proc/self/setgroups\0", b"deny")?;
            write_map(b"/proc/self/uid_map\0", &uid_map)?;
            write_map(b"/proc/self/gid_map\0", &gid_map)?;
            checked(libc::unshare(
                libc::CLONE_NEWNS
                    | libc::CLONE_NEWPID
                    | libc::CLONE_NEWNET
                    | libc::CLONE_NEWIPC
                    | libc::CLONE_NEWUTS,
            ))?;
            checked(libc::sethostname(c"ostadix".as_ptr(), 7))?;
            let alive = private_pipe(libc::O_NONBLOCK)?;
            let outcome = private_pipe(0)?;
            let child = libc::fork();
            checked(child)?;
            if child != 0 {
                libc::close(alive[0]);
                libc::close(outcome[1]);
                // Close the Command exec-error pipe in this waiting process;
                // the eventual runner retains its copy until exec/error.
                close_except(&[alive[1].min(outcome[0]), alive[1].max(outcome[0])])?;
                let mut status = 0;
                while libc::waitpid(child, &mut status, 0) < 0 {
                    if *libc::__errno_location() != libc::EINTR {
                        libc::_exit(125);
                    }
                }
                // Namespace PID 1 cannot reproduce ordinary fatal signals by
                // signaling itself. Receive the primary runner's raw wait
                // status instead; if init failed before reporting, retain its
                // own status as the failure outcome.
                let mut bytes = [0u8; std::mem::size_of::<libc::c_int>()];
                let mut received = 0;
                while received < bytes.len() {
                    let count = libc::read(
                        outcome[0],
                        bytes[received..].as_mut_ptr().cast(),
                        bytes.len() - received,
                    );
                    if count < 0 && *libc::__errno_location() == libc::EINTR {
                        continue;
                    }
                    if count <= 0 {
                        break;
                    }
                    received += count as usize;
                }
                if received == bytes.len() {
                    status = libc::c_int::from_ne_bytes(bytes);
                }
                libc::close(outcome[0]);
                libc::close(alive[1]);
                if libc::WIFSIGNALED(status) {
                    let signal = libc::WTERMSIG(status);
                    libc::signal(signal, libc::SIG_DFL);
                    let mut unblocked: libc::sigset_t = std::mem::zeroed();
                    libc::sigemptyset(&mut unblocked);
                    libc::sigaddset(&mut unblocked, signal);
                    libc::sigprocmask(libc::SIG_UNBLOCK, &unblocked, std::ptr::null_mut());
                    libc::kill(libc::getpid(), signal);
                    libc::_exit(128 + signal);
                }
                libc::_exit(libc::WEXITSTATUS(status));
            }
            libc::close(alive[1]);
            libc::close(outcome[0]);
            checked(libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0))?;
            let mut byte = 0u8;
            if libc::read(alive[0], (&mut byte as *mut u8).cast(), 1) != -1
                || *libc::__errno_location() != libc::EAGAIN
            {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            libc::close(alive[0]);
            checked(libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            ))?;
            checked(libc::mount(
                root_c.as_ptr(),
                root_c.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REC,
                std::ptr::null(),
            ))?;
            for path in &mounts {
                checked(libc::mount(
                    c"tmpfs".as_ptr(),
                    path.as_ptr(),
                    c"tmpfs".as_ptr(),
                    libc::MS_NOSUID | libc::MS_NODEV,
                    c"mode=1777".as_ptr().cast(),
                ))?;
            }
            for (source, target) in &devices {
                checked(libc::mount(
                    source.as_ptr(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                ))?;
            }
            checked(libc::mount(
                c"proc".as_ptr(),
                root_proc.as_ptr(),
                c"proc".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC | libc::MS_RDONLY,
                std::ptr::null(),
            ))?;
            // Bring up only the private namespace's loopback interface.
            let socket = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
            if socket < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut interface: libc::ifreq = std::mem::zeroed();
            interface.ifr_name[0] = b'l' as _;
            interface.ifr_name[1] = b'o' as _;
            checked(libc::ioctl(socket, libc::SIOCGIFFLAGS, &mut interface))?;
            interface.ifr_ifru.ifru_flags |= libc::IFF_UP as i16;
            checked(libc::ioctl(socket, libc::SIOCSIFFLAGS, &interface))?;
            libc::close(socket);
            checked(libc::chdir(root_c.as_ptr()))?;
            checked(
                libc::syscall(libc::SYS_pivot_root, root_c.as_ptr(), root_old.as_ptr()) as i32,
            )?;
            checked(libc::chdir(c"/".as_ptr()))?;
            checked(libc::umount2(c"/.old-root".as_ptr(), libc::MNT_DETACH))?;
            checked(libc::rmdir(c"/.old-root".as_ptr()))?;
            checked(libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REMOUNT
                    | libc::MS_BIND
                    | libc::MS_RDONLY
                    | libc::MS_NOSUID
                    | libc::MS_NODEV,
                std::ptr::null(),
            ))?;
            checked(libc::chdir(c"/work".as_ptr()))?;
            checked(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))?;
            for capability in 0..64 {
                if libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) < 0
                    && *libc::__errno_location() != libc::EINVAL
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            #[repr(C)]
            struct Header {
                version: u32,
                pid: i32,
            }
            #[repr(C)]
            struct Data {
                effective: u32,
                permitted: u32,
                inheritable: u32,
            }
            let header = Header {
                version: 0x20080522,
                pid: 0,
            };
            let data = [
                Data {
                    effective: 0,
                    permitted: 0,
                    inheritable: 0,
                },
                Data {
                    effective: 0,
                    permitted: 0,
                    inheritable: 0,
                },
            ];
            checked(libc::syscall(libc::SYS_capset, &header, data.as_ptr()) as i32)?;
            // Both namespace init and the eventual evaluator receive the same
            // syscall boundary. The BPF storage was allocated before pre_exec.
            install_prepared_mount_filter(&mut mount_filter)?;

            // Keep PID 1 as a small init, not the evaluator: orphaned runtime
            // grandchildren must be reaped without stealing the evaluator's
            // managed Child wait statuses or changing its SIGCHLD behavior.
            let signals = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT];
            let mut blocked: libc::sigset_t = std::mem::zeroed();
            let mut previous_mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut blocked);
            for signal in signals {
                libc::sigaddset(&mut blocked, signal);
            }
            checked(libc::sigprocmask(
                libc::SIG_BLOCK,
                &blocked,
                &mut previous_mask,
            ))?;
            let mut forward: libc::sigaction = std::mem::zeroed();
            forward.sa_sigaction = forward_termination as *const () as usize;
            libc::sigemptyset(&mut forward.sa_mask);
            let mut previous_actions: [libc::sigaction; 4] = std::mem::zeroed();
            for (index, signal) in signals.into_iter().enumerate() {
                checked(libc::sigaction(
                    signal,
                    &forward,
                    &mut previous_actions[index],
                ))?;
            }
            let mut default_child: libc::sigaction = std::mem::zeroed();
            default_child.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&mut default_child.sa_mask);
            let mut previous_child: libc::sigaction = std::mem::zeroed();
            checked(libc::sigaction(
                libc::SIGCHLD,
                &default_child,
                &mut previous_child,
            ))?;
            let primary = libc::fork();
            checked(primary)?;
            if primary != 0 {
                // In particular, close Command's exec-error channel here: only
                // the actual exec child may keep the host spawn handshake open.
                close_except(&[outcome[1]])?;
                checked(libc::sigprocmask(
                    libc::SIG_UNBLOCK,
                    &blocked,
                    std::ptr::null_mut(),
                ))?;
                let primary_status;
                loop {
                    let mut status = 0;
                    let reaped = libc::waitpid(-1, &mut status, 0);
                    if reaped == primary {
                        primary_status = status;
                        break;
                    }
                    if reaped < 0 && *libc::__errno_location() != libc::EINTR {
                        libc::_exit(125);
                    }
                }
                // The primary program is finished. Kill and reap remaining
                // namespace descendants before publishing its exact outcome.
                libc::kill(-1, libc::SIGKILL);
                loop {
                    let mut status = 0;
                    if libc::waitpid(-1, &mut status, 0) >= 0 {
                        continue;
                    }
                    if *libc::__errno_location() == libc::EINTR {
                        continue;
                    }
                    if *libc::__errno_location() != libc::ECHILD {
                        libc::_exit(125);
                    }
                    break;
                }
                let bytes = primary_status.to_ne_bytes();
                let mut written = 0;
                while written < bytes.len() {
                    let count = libc::write(
                        outcome[1],
                        bytes[written..].as_ptr().cast(),
                        bytes.len() - written,
                    );
                    if count < 0 && *libc::__errno_location() == libc::EINTR {
                        continue;
                    }
                    if count <= 0 {
                        libc::_exit(125);
                    }
                    written += count as usize;
                }
                libc::close(outcome[1]);
                libc::_exit(0);
            }
            libc::close(outcome[1]);
            checked(libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0))?;
            if libc::getppid() != 1 {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            for (index, signal) in signals.into_iter().enumerate() {
                checked(libc::sigaction(
                    signal,
                    &previous_actions[index],
                    std::ptr::null_mut(),
                ))?;
            }
            checked(libc::sigaction(
                libc::SIGCHLD,
                &previous_child,
                std::ptr::null_mut(),
            ))?;
            checked(libc::sigprocmask(
                libc::SIG_SETMASK,
                &previous_mask,
                std::ptr::null_mut(),
            ))?;
            // CLOEXEC closes every inherited non-stdio descriptor only on
            // successful exec, preserving Command's error reporting channel.
            checked(libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 4u32) as i32)?;
            Ok(())
        });
    }
    command.status().context("enter embedded Linux rootfs (namespace permission and a complete ELF/runtime closure are required)")
}

#[cfg(not(target_os = "linux"))]
pub fn launch(_root: &Path, _image: &str) -> Result<ExitStatus> {
    bail!("embedded rootfs execution requires Linux")
}

/// Preserve the child's process outcome after the caller has dropped its
/// extraction guard. Signals remain signals rather than integer exit codes.
pub fn exit_like(status: ExitStatus) -> ! {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            // SAFETY: target is this process, with the observed child signal.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                let mut unblocked: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut unblocked);
                libc::sigaddset(&mut unblocked, signal);
                libc::sigprocmask(libc::SIG_UNBLOCK, &unblocked, std::ptr::null_mut());
                libc::raise(signal);
            }
            std::process::exit(128 + signal);
        }
    }
    std::process::exit(status.code().unwrap_or(125));
}
