//! Native Android/Termux host controller for Ostadix-lang.
//!
//! Read-only commands never probe a root provider. Privileged operations are
//! limited to applying Android's fixed top-app/foreground task profiles to a
//! validated Termux app process.

use std::collections::BTreeSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

const STATUS_SCHEMA: &str = "ostadix.device-status/v1";
const DOCTOR_SCHEMA: &str = "ostadix.device-doctor/v1";
const PRIME_SCHEMA: &str = "ostadix.device-prime/v1";
const TOP_APP_PROFILE: &str = "CPUSET_SP_TOP_APP";
const FOREGROUND_PROFILE: &str = "CPUSET_SP_FOREGROUND";
const BATTERY_TIMEOUT: Duration = Duration::from_secs(3);
const NATIVE_RUSTFLAGS: &str = "-C target-cpu=native -C linker=clang -C link-arg=-fuse-ld=lld";
const NATIVE_CFLAGS: &str = "-std=c17 -Wall -Wextra -Wpedantic -O3 -mcpu=native \
    -flto=thin -Iinclude -D_POSIX_C_SOURCE=200809L -D_XOPEN_SOURCE=700";
const NATIVE_LDFLAGS: &str = "-pthread -fuse-ld=lld -flto=thin";
const SCCACHE_IDLE_TIMEOUT_SECONDS: &str = "600";

#[derive(Debug, Parser)]
#[command(
    name = "ostadix-device",
    version,
    about = "Native Android host controller for Ostadix-lang",
    long_about = "Build, run, diagnose, and explicitly tune Ostadix-lang on Android. \
Read-only commands do not probe su. Prime mode temporarily applies Android's \
top-app task profile, returns the target to foreground afterward, and never \
changes thermal controls, SELinux, boot files, or governors."
)]
struct Cli {
    /// Ostadix-lang checkout (O_LANG_ROOT, then $HOME/Ostadix-lang by default).
    #[arg(long, global = true, value_name = "PATH")]
    root: Option<PathBuf>,

    /// Emit stable JSON for commands that support structured output.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: DeviceCommand,
}

#[derive(Debug, Subcommand)]
enum DeviceCommand {
    /// Show live device, scheduler, toolchain, battery, and Ostadix paths.
    Status,
    /// Check whether this Android host is ready to build and run Ostadix.
    Doctor,
    /// Run the optimized Rust evaluator with Android-safe loader settings.
    Run {
        /// Request Android's top-app profile and pin execution to the prime CPU.
        #[arg(long)]
        prime: bool,
        /// Do not hold a Termux wake lock while O runs.
        #[arg(long)]
        no_wake_lock: bool,
        /// Arguments passed verbatim to O. Use `--` before ambiguous arguments.
        #[arg(
            value_name = "O_ARG",
            trailing_var_arg = true,
            allow_hyphen_values = true
        )]
        args: Vec<OsString>,
    },
    /// Build native release binaries.
    Build {
        #[arg(value_enum, default_value_t = BuildTarget::Rust)]
        target: BuildTarget,
        #[arg(short, long)]
        jobs: Option<usize>,
        /// Request top-app scheduling for the build without running Cargo as root.
        #[arg(long)]
        prime: bool,
        #[arg(long)]
        no_wake_lock: bool,
    },
    /// Compatibility command for the optimized C17 runtime build.
    #[command(name = "build-c17")]
    BuildC17 {
        #[arg(short, long)]
        jobs: Option<usize>,
        #[arg(long)]
        prime: bool,
        #[arg(long)]
        no_wake_lock: bool,
    },
    /// Check the Rust workspace with the device-local toolchain.
    Check {
        #[arg(short, long)]
        jobs: Option<usize>,
        #[arg(long)]
        prime: bool,
        #[arg(long)]
        no_wake_lock: bool,
    },
    /// Hold or release the Termux wake lock explicitly.
    Performance {
        #[arg(value_enum)]
        action: PerformanceAction,
    },
    /// Inspect or change Android task-profile placement for one Termux process.
    Prime {
        #[command(subcommand)]
        command: PrimeCommand,
    },
    /// Enter the root manager or run an explicit command as root.
    Root {
        #[command(subcommand)]
        command: RootCommand,
    },
    /// Show compiler-cache statistics.
    Cache,
    /// Stop sccache and enforce the configured ccache size limit.
    #[command(name = "cache-trim")]
    CacheTrim,
    /// Root-only implementation reached through an explicit prime request.
    #[command(name = "__prime-apply", hide = true)]
    PrimeApply {
        #[arg(value_enum)]
        action: PrimeAction,
        pid: u32,
        expected_uid: u32,
        expected_start_time: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum BuildTarget {
    Rust,
    C17,
    All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PerformanceAction {
    On,
    Off,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PrimeAction {
    Attach,
    Release,
}

#[derive(Debug, Subcommand)]
enum PrimeCommand {
    /// Report the live cpuset and scheduler affinity without invoking su.
    Status {
        /// Defaults to this CLI process.
        pid: Option<u32>,
    },
    /// Apply Android's top-app profile. Elevates through su when necessary.
    Attach { pid: u32 },
    /// Restore Android's foreground profile. Elevates through su when necessary.
    Release { pid: u32 },
}

#[derive(Debug, Subcommand)]
enum RootCommand {
    /// Ask the installed root provider to prove the granted identity.
    Status,
    /// Enter the root provider's interactive shell.
    Shell,
    /// Run one argv-shaped command through the root provider.
    Run {
        #[arg(
            required = true,
            value_name = "COMMAND",
            trailing_var_arg = true,
            allow_hyphen_values = true
        )]
        command: Vec<OsString>,
    },
}

#[derive(Debug, Serialize)]
struct DeviceStatus {
    schema: &'static str,
    device: DeviceIdentity,
    android: AndroidIdentity,
    runtime: RuntimeStatus,
    cpu: CpuStatus,
    memory: MemoryStatus,
    battery: Option<BatteryStatus>,
    toolchain: ToolchainStatus,
    paths: PathStatus,
    privileges: PrivilegeStatus,
}

#[derive(Debug, Serialize)]
struct DeviceIdentity {
    model: Option<String>,
    product: Option<String>,
    soc: Option<String>,
    architecture: String,
}

#[derive(Debug, Serialize)]
struct AndroidIdentity {
    detected: bool,
    release: Option<String>,
    api: Option<String>,
    termux: bool,
}

#[derive(Debug, Serialize)]
struct RuntimeStatus {
    cpuset: Option<String>,
    cgroups: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CpuStatus {
    present: String,
    present_count: usize,
    allowed: String,
    allowed_count: usize,
    prime_core: Option<u32>,
    prime_available: Option<bool>,
}

#[derive(Debug, Default, Serialize)]
struct MemoryStatus {
    total_mib: Option<u64>,
    available_mib: Option<u64>,
}

#[derive(Debug, Serialize)]
struct BatteryStatus {
    percentage: Option<f64>,
    temperature_c: Option<f64>,
    health: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Serialize)]
struct ToolchainStatus {
    rustc: Option<String>,
    cargo: Option<String>,
    clang: Option<String>,
    linker: Option<PathBuf>,
    sccache: bool,
    ccache: bool,
    taskset: bool,
}

#[derive(Debug, Serialize)]
struct PathStatus {
    project_root: PathBuf,
    target_dir: PathBuf,
    backends_dir: PathBuf,
    evaluator: Option<PathBuf>,
    private_app_storage: bool,
}

#[derive(Debug, Serialize)]
struct PrivilegeStatus {
    effective_uid: Option<u32>,
    root_session: bool,
    su_probed: bool,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    schema: &'static str,
    ready: bool,
    checks: Vec<DoctorCheck>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum CheckLevel {
    Pass,
    Info,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    name: &'static str,
    level: CheckLevel,
    detail: String,
}

#[derive(Debug, Serialize)]
struct PrimeStatus {
    schema: &'static str,
    pid: u32,
    uid: u32,
    cpuset: Option<String>,
    allowed: String,
    prime_core: Option<u32>,
    prime_available: Option<bool>,
}

#[derive(Clone, Debug)]
struct ProcessIdentity {
    pid: u32,
    uid: u32,
    start_time: u64,
}

fn main() -> ExitCode {
    match dispatch(Cli::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("ostadix-device: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cli: Cli) -> Result<u8> {
    let root = resolve_project_root(cli.root.as_deref())?;
    match cli.command {
        DeviceCommand::Status => {
            let status = collect_status(&root);
            print_status(&status, cli.json)?;
            Ok(0)
        }
        DeviceCommand::Doctor => {
            let report = doctor(&collect_status(&root));
            print_doctor(&report, cli.json)?;
            Ok(if report.ready { 0 } else { 1 })
        }
        DeviceCommand::Run {
            prime,
            no_wake_lock,
            args,
        } => {
            let _prime = PrimeGuard::acquire(prime, PrimeAffinity::Pinned)?;
            let _wake = WakeLockGuard::acquire(!no_wake_lock)?;
            let status = run_evaluator(&root, &args)?;
            Ok(exit_status_code(status))
        }
        DeviceCommand::Build {
            target,
            jobs,
            prime,
            no_wake_lock,
        } => {
            validate_jobs(jobs)?;
            let _prime = PrimeGuard::acquire(prime, PrimeAffinity::Available)?;
            let _wake = WakeLockGuard::acquire(!no_wake_lock)?;
            build(&root, target, jobs)
        }
        DeviceCommand::BuildC17 {
            jobs,
            prime,
            no_wake_lock,
        } => {
            validate_jobs(jobs)?;
            let _prime = PrimeGuard::acquire(prime, PrimeAffinity::Available)?;
            let _wake = WakeLockGuard::acquire(!no_wake_lock)?;
            build_c17(&root, jobs.unwrap_or_else(c17_jobs))
        }
        DeviceCommand::Check {
            jobs,
            prime,
            no_wake_lock,
        } => {
            validate_jobs(jobs)?;
            let _prime = PrimeGuard::acquire(prime, PrimeAffinity::Available)?;
            let _wake = WakeLockGuard::acquire(!no_wake_lock)?;
            check_workspace(&root, jobs.unwrap_or_else(check_jobs))
        }
        DeviceCommand::Performance { action } => performance(action),
        DeviceCommand::Prime { command } => prime_command(command, cli.json),
        DeviceCommand::Root { command } => root_command(command),
        DeviceCommand::Cache => cache_stats(),
        DeviceCommand::CacheTrim => cache_trim(),
        DeviceCommand::PrimeApply {
            action,
            pid,
            expected_uid,
            expected_start_time,
        } => {
            apply_prime_root(action, pid, expected_uid, expected_start_time)?;
            Ok(0)
        }
    }
}

fn resolve_project_root(explicit: Option<&Path>) -> Result<PathBuf> {
    let selected = explicit
        .map(Path::to_path_buf)
        .or_else(|| nonempty_env_path("O_LANG_ROOT"))
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join("Ostadix-lang")))
        .unwrap_or_else(|| PathBuf::from("Ostadix-lang"));
    if selected.is_absolute() {
        Ok(selected)
    } else {
        Ok(env::current_dir()
            .context("could not determine the current directory")?
            .join(selected))
    }
}

fn nonempty_env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn collect_status(root: &Path) -> DeviceStatus {
    // Capture the CLI's own live scheduler placement before spawning helpers.
    let allowed_cpus = affinity_for_pid(0)
        .or_else(|| affinity_from_proc_status("/proc/self/status"))
        .unwrap_or_default();
    let cpuset = read_trimmed("/proc/self/cpuset");
    let cgroups = read_lines("/proc/self/cgroup");
    let present_cpus = present_cpus().unwrap_or_else(|| allowed_cpus.clone());
    let prime_core = present_cpus.iter().copied().max();
    let prime_available = prime_core.map(|prime| allowed_cpus.contains(&prime));

    let backends_dir = nonempty_env_path("O_BACKENDS_DIR").unwrap_or_else(|| root.join("backends"));
    let evaluator = evaluator_path(root);

    DeviceStatus {
        schema: STATUS_SCHEMA,
        device: DeviceIdentity {
            model: getprop("ro.product.model"),
            product: getprop("ro.product.device"),
            soc: getprop("ro.soc.model"),
            architecture: env::consts::ARCH.to_string(),
        },
        android: AndroidIdentity {
            detected: Path::new("/system/bin/getprop").is_file(),
            release: getprop("ro.build.version.release"),
            api: getprop("ro.build.version.sdk"),
            termux: termux_detected(),
        },
        runtime: RuntimeStatus { cpuset, cgroups },
        cpu: CpuStatus {
            present: format_cpu_list(&present_cpus),
            present_count: present_cpus.len(),
            allowed: format_cpu_list(&allowed_cpus),
            allowed_count: allowed_cpus.len(),
            prime_core,
            prime_available,
        },
        memory: memory_status(),
        battery: battery_status(),
        toolchain: ToolchainStatus {
            rustc: command_first_line("rustc", &["--version"]),
            cargo: command_first_line("cargo", &["--version"]),
            clang: command_first_line("clang", &["--version"]),
            linker: which::which("ld.lld").ok(),
            sccache: which::which("sccache").is_ok(),
            ccache: which::which("ccache").is_ok(),
            taskset: which::which("taskset").is_ok(),
        },
        paths: PathStatus {
            project_root: root.to_path_buf(),
            target_dir: root.join("target"),
            backends_dir,
            evaluator,
            private_app_storage: is_private_termux_path(root),
        },
        privileges: PrivilegeStatus {
            effective_uid: effective_uid(),
            root_session: effective_uid() == Some(0),
            su_probed: false,
        },
    }
}

fn print_status(status: &DeviceStatus, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(status)?);
        return Ok(());
    }

    println!(
        "device:      {} / {}",
        show(&status.device.model),
        show(&status.device.soc)
    );
    println!(
        "android:     {} (API {}) / {}",
        show(&status.android.release),
        show(&status.android.api),
        status.device.architecture
    );
    println!(
        "scheduler:   {} / CPUs {} ({} available)",
        status.runtime.cpuset.as_deref().unwrap_or("unavailable"),
        empty_as_unavailable(&status.cpu.allowed),
        status.cpu.allowed_count
    );
    match (status.cpu.prime_core, status.cpu.prime_available) {
        (Some(prime), Some(true)) => println!("prime:       CPU {prime} available now"),
        (Some(prime), Some(false)) => {
            println!("prime:       CPU {prime} withheld; use --prime or `prime attach PID`")
        }
        _ => println!("prime:       unavailable"),
    }
    println!(
        "memory:      {} MiB total / {} MiB available",
        show_number(status.memory.total_mib),
        show_number(status.memory.available_mib)
    );
    if let Some(battery) = &status.battery {
        println!(
            "battery:     {}% / {} C / {} / {}",
            show_float(battery.percentage),
            show_float(battery.temperature_c),
            show(&battery.health),
            show(&battery.status)
        );
    }
    println!("rust:        {}", show(&status.toolchain.rustc));
    println!(
        "linker:      {}",
        status
            .toolchain
            .linker
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "unavailable".to_string())
    );
    println!(
        "O binary:    {}",
        status
            .paths
            .evaluator
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "unavailable".to_string())
    );
    println!("project:     {}", status.paths.project_root.display());
    println!("backends:    {}", status.paths.backends_dir.display());
    println!(
        "privilege:   uid {} (status did not probe su)",
        status
            .privileges
            .effective_uid
            .map(|uid| uid.to_string())
            .unwrap_or_else(|| "unavailable".to_string())
    );
    Ok(())
}

fn doctor(status: &DeviceStatus) -> DoctorReport {
    let mut checks = Vec::new();
    checks.push(check(
        "android",
        status.android.detected,
        "Android properties are available",
        "not running in an Android userspace",
        true,
    ));
    checks.push(check(
        "termux",
        status.android.termux,
        "Termux environment detected",
        "Termux environment was not detected",
        true,
    ));
    checks.push(check(
        "architecture",
        status.device.architecture == "aarch64",
        "native aarch64 userspace",
        &format!("expected aarch64, found {}", status.device.architecture),
        true,
    ));
    checks.push(check(
        "project",
        status.paths.project_root.join("Cargo.toml").is_file(),
        &format!(
            "{} is an Ostadix checkout",
            status.paths.project_root.display()
        ),
        &format!("missing {}/Cargo.toml", status.paths.project_root.display()),
        true,
    ));
    checks.push(check(
        "private-storage",
        status.paths.private_app_storage,
        "project is in Termux private storage",
        "project path is not recognized as Termux private storage",
        false,
    ));
    checks.push(check(
        "evaluator",
        status.paths.evaluator.is_some(),
        "optimized O evaluator found",
        "optimized O evaluator is not built or on PATH",
        true,
    ));
    checks.push(check(
        "backends",
        status.paths.backends_dir.is_dir(),
        "backend directory found",
        &format!("missing {}", status.paths.backends_dir.display()),
        true,
    ));
    checks.push(check(
        "rust-toolchain",
        status.toolchain.rustc.is_some() && status.toolchain.cargo.is_some(),
        "rustc and Cargo found",
        "rustc or Cargo is unavailable",
        true,
    ));
    checks.push(check(
        "native-toolchain",
        status.toolchain.clang.is_some() && status.toolchain.linker.is_some(),
        "Clang and LLD found",
        "Clang or LLD is unavailable; C17/AOT builds may fail",
        false,
    ));
    checks.push(DoctorCheck {
        name: "compiler-cache",
        level: if status.toolchain.sccache && status.toolchain.ccache {
            CheckLevel::Pass
        } else {
            CheckLevel::Warn
        },
        detail: format!(
            "sccache={}, ccache={}",
            status.toolchain.sccache, status.toolchain.ccache
        ),
    });
    checks.push(DoctorCheck {
        name: "prime-core",
        level: match status.cpu.prime_available {
            Some(true) => CheckLevel::Pass,
            Some(false) => CheckLevel::Info,
            None => CheckLevel::Warn,
        },
        detail: match (status.cpu.prime_core, status.cpu.prime_available) {
            (Some(cpu), Some(true)) => format!("CPU {cpu} is in the live affinity mask"),
            (Some(cpu), Some(false)) => format!(
                "CPU {cpu} is currently withheld by {}; explicit prime mode can request top-app",
                status.runtime.cpuset.as_deref().unwrap_or("the scheduler")
            ),
            _ => "could not identify a prime CPU".to_string(),
        },
    });
    checks.push(DoctorCheck {
        name: "root-surface",
        level: CheckLevel::Info,
        detail: "doctor does not probe su; only an explicit prime mutation may invoke it"
            .to_string(),
    });
    checks.push(DoctorCheck {
        name: "thermal-safety",
        level: CheckLevel::Info,
        detail: "no governor, thermal, SELinux, boot, module, or root-hiding changes".to_string(),
    });

    DoctorReport {
        schema: DOCTOR_SCHEMA,
        ready: !checks
            .iter()
            .any(|item| matches!(item.level, CheckLevel::Fail)),
        checks,
    }
}

fn check(
    name: &'static str,
    passed: bool,
    pass_detail: &str,
    fail_detail: &str,
    required: bool,
) -> DoctorCheck {
    DoctorCheck {
        name,
        level: if passed {
            CheckLevel::Pass
        } else if required {
            CheckLevel::Fail
        } else {
            CheckLevel::Warn
        },
        detail: if passed { pass_detail } else { fail_detail }.to_string(),
    }
}

fn print_doctor(report: &DoctorReport, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    for item in &report.checks {
        let label = match item.level {
            CheckLevel::Pass => "ok",
            CheckLevel::Info => "info",
            CheckLevel::Warn => "warn",
            CheckLevel::Fail => "fail",
        };
        println!("[{label:4}] {:18} {}", item.name, item.detail);
    }
    println!(
        "doctor: {}",
        if report.ready { "ready" } else { "not ready" }
    );
    Ok(())
}

fn run_evaluator(root: &Path, args: &[OsString]) -> Result<ExitStatus> {
    let evaluator = evaluator_path(root).with_context(|| {
        format!(
            "no optimized O evaluator found under {} or on PATH",
            root.display()
        )
    })?;
    let mut command = ProcessCommand::new(&evaluator);
    command.args(args);
    command.env("TERMUX_EXEC__EXECVE_CALL__INTERCEPT", "disable");
    command.env("O_LANG_ROOT", root);
    command.env(
        "O_BACKENDS_DIR",
        nonempty_env_path("O_BACKENDS_DIR").unwrap_or_else(|| root.join("backends")),
    );
    if let Some(loader_path) = termux_loader_path() {
        command.env("LD_LIBRARY_PATH", loader_path);
    }
    run_child(&mut command).with_context(|| format!("could not run {}", evaluator.display()))
}

fn build(root: &Path, target: BuildTarget, jobs: Option<usize>) -> Result<u8> {
    validate_checkout(root)?;
    if matches!(target, BuildTarget::Rust | BuildTarget::All) {
        let code = build_rust(root, jobs.unwrap_or_else(release_jobs))?;
        if code != 0 {
            return Ok(code);
        }
    }
    if matches!(target, BuildTarget::C17 | BuildTarget::All) {
        return build_c17(root, jobs.unwrap_or_else(c17_jobs));
    }
    Ok(0)
}

fn build_rust(root: &Path, jobs: usize) -> Result<u8> {
    let cargo = which::which("cargo").context("Cargo is not available")?;
    let sccache = which::which("sccache").ok();
    println!("Rust tuning: target-cpu=native, clang + LLD, fat-LTO release profile");
    match &sccache {
        Some(path) => println!(
            "Rust cache:  sccache {} ({}s idle timeout)",
            path.display(),
            SCCACHE_IDLE_TIMEOUT_SECONDS
        ),
        None => println!("Rust cache:  disabled (sccache is not installed)"),
    }
    let mut command = ProcessCommand::new(cargo);
    command
        .current_dir(root)
        .args(["build", "--workspace", "--release", "--locked", "--bins"])
        // Do not let a calling shell silently replace the device build policy.
        // CARGO_ENCODED_RUSTFLAGS has higher precedence than RUSTFLAGS.
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_BUILD_JOBS", jobs.to_string())
        .env("RUSTFLAGS", NATIVE_RUSTFLAGS);
    if let Some(wrapper) = &sccache {
        command
            .env("RUSTC_WRAPPER", wrapper)
            .env("SCCACHE_IDLE_TIMEOUT", SCCACHE_IDLE_TIMEOUT_SECONDS);
    }
    Ok(exit_status_code(
        run_child(&mut command).context("could not start the Rust release build")?,
    ))
}

fn build_c17(root: &Path, jobs: usize) -> Result<u8> {
    validate_checkout(root)?;
    let make = which::which("make").context("make is not available")?;
    let ccache = which::which("ccache").ok();
    let compiler = match &ccache {
        Some(path) => format!("{} clang", path.display()),
        None => "clang".to_string(),
    };
    println!("C17 tuning:  -mcpu=native, ThinLTO, clang + LLD");
    match &ccache {
        Some(path) => println!("C17 cache:   ccache {}", path.display()),
        None => println!("C17 cache:   disabled (ccache is not installed)"),
    }
    let mut command = ProcessCommand::new(make);
    command
        .current_dir(root)
        .env("CCACHE_BASEDIR", root)
        .env("CCACHE_COMPILERCHECK", "content")
        .env("CCACHE_NOHASHDIR", "true")
        .arg("-C")
        .arg("c_cpp")
        .arg("-B")
        .arg(format!("-j{jobs}"))
        .arg(format!("CC={compiler}"))
        .arg(format!("CFLAGS={NATIVE_CFLAGS}"))
        .arg(format!("LDFLAGS={NATIVE_LDFLAGS}"))
        .arg("all");
    Ok(exit_status_code(
        run_child(&mut command).context("could not start the C17 release build")?,
    ))
}

fn check_workspace(root: &Path, jobs: usize) -> Result<u8> {
    validate_checkout(root)?;
    let cargo = which::which("cargo").context("Cargo is not available")?;
    let mut command = ProcessCommand::new(cargo);
    command
        .current_dir(root)
        .args(["check", "--workspace", "--locked"])
        .env("CARGO_BUILD_JOBS", jobs.to_string());
    Ok(exit_status_code(
        run_child(&mut command).context("could not start cargo check")?,
    ))
}

fn validate_checkout(root: &Path) -> Result<()> {
    if !root.join("Cargo.toml").is_file() {
        bail!("{} is not an Ostadix checkout", root.display());
    }
    Ok(())
}

fn validate_jobs(jobs: Option<usize>) -> Result<()> {
    if jobs == Some(0) {
        bail!("--jobs must be at least 1");
    }
    Ok(())
}

fn release_jobs() -> usize {
    positive_env_usize("OSTADIX_RELEASE_JOBS").unwrap_or(5)
}

fn c17_jobs() -> usize {
    positive_env_usize("OSTADIX_C_BUILD_JOBS").unwrap_or(6)
}

fn check_jobs() -> usize {
    positive_env_usize("CARGO_BUILD_JOBS").unwrap_or(6)
}

fn positive_env_usize(name: &str) -> Option<usize> {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}

fn performance(action: PerformanceAction) -> Result<u8> {
    let command = match action {
        PerformanceAction::On => "termux-wake-lock",
        PerformanceAction::Off => "termux-wake-unlock",
    };
    let path = which::which(command).with_context(|| format!("{command} is not available"))?;
    let status = ProcessCommand::new(path)
        .status()
        .with_context(|| format!("could not run {command}"))?;
    if status.success() {
        println!(
            "performance wake lock: {}",
            if matches!(action, PerformanceAction::On) {
                "on"
            } else {
                "off"
            }
        );
    }
    Ok(exit_status_code(status))
}

fn cache_stats() -> Result<u8> {
    let mut found = false;
    for (name, arg) in [("sccache", "--show-stats"), ("ccache", "--show-stats")] {
        if let Ok(path) = which::which(name) {
            found = true;
            println!("== {name} ==");
            let _ = ProcessCommand::new(path).arg(arg).status();
        }
    }
    if !found {
        println!("no compiler caches found");
    }
    Ok(0)
}

fn cache_trim() -> Result<u8> {
    if let Ok(path) = which::which("sccache") {
        let _ = ProcessCommand::new(path)
            .arg("--stop-server")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    if let Ok(path) = which::which("ccache") {
        return Ok(exit_status_code(
            ProcessCommand::new(path)
                .arg("--cleanup")
                .status()
                .context("could not trim ccache")?,
        ));
    }
    bail!("ccache is not available")
}

struct WakeLockGuard {
    unlock: Option<PathBuf>,
}

impl WakeLockGuard {
    fn acquire(enabled: bool) -> Result<Self> {
        if !enabled {
            return Ok(Self { unlock: None });
        }
        let lock = which::which("termux-wake-lock")
            .context("termux-wake-lock is unavailable; use --no-wake-lock to continue")?;
        let unlock = which::which("termux-wake-unlock")
            .context("termux-wake-unlock is unavailable; use --no-wake-lock to continue")?;
        let status = ProcessCommand::new(lock)
            .status()
            .context("could not acquire the Termux wake lock")?;
        if !status.success() {
            bail!("termux-wake-lock exited with {status}");
        }
        Ok(Self {
            unlock: Some(unlock),
        })
    }
}

impl Drop for WakeLockGuard {
    fn drop(&mut self) {
        if let Some(unlock) = self.unlock.take() {
            if !ProcessCommand::new(unlock)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
            {
                eprintln!("ostadix-device: warning: could not release the Termux wake lock");
            }
        }
    }
}

struct PrimeGuard {
    identity: Option<ProcessIdentity>,
    original_affinity: Option<Vec<u32>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrimeAffinity {
    /// Make every top-app CPU available for parallel work.
    Available,
    /// Guarantee that the evaluator and its inherited threads start on prime.
    Pinned,
}

impl PrimeGuard {
    fn acquire(enabled: bool, affinity: PrimeAffinity) -> Result<Self> {
        if !enabled {
            return Ok(Self {
                identity: None,
                original_affinity: None,
            });
        }
        let identity = validated_prime_identity(std::process::id(), caller_termux_uid()?, None)?;
        let original_affinity = matches!(affinity, PrimeAffinity::Pinned)
            .then(|| affinity_for_pid(0).context("could not capture the original CPU affinity"))
            .transpose()?;
        invoke_prime(PrimeAction::Attach, &identity)?;
        if matches!(affinity, PrimeAffinity::Pinned) {
            if let Err(error) = pin_current_to_prime() {
                if let Some(original) = &original_affinity {
                    let _ = set_affinity_for_pid(0, original);
                }
                let _ = invoke_prime(PrimeAction::Release, &identity);
                return Err(error.context("could not pin prime execution"));
            }
        }
        Ok(Self {
            identity: Some(identity),
            original_affinity,
        })
    }
}

impl Drop for PrimeGuard {
    fn drop(&mut self) {
        if let Some(identity) = self.identity.take() {
            let original = self.original_affinity.take();
            let first_restore_error = original
                .as_deref()
                .and_then(|affinity| set_affinity_for_pid(0, affinity).err());
            if let Err(error) = invoke_prime(PrimeAction::Release, &identity) {
                eprintln!(
                    "ostadix-device: warning: could not restore foreground profile: {error:#}"
                );
            }
            if let (Some(original), Some(first_error)) = (original, first_restore_error) {
                if let Err(second_error) = set_affinity_for_pid(0, &original) {
                    eprintln!(
                        "ostadix-device: warning: could not restore original CPU affinity before or after foreground release: {first_error:#}; retry: {second_error:#}"
                    );
                }
            }
        }
    }
}

fn pin_current_to_prime() -> Result<u32> {
    let allowed = affinity_for_pid(0).context("could not read top-app CPU affinity")?;
    let present = present_cpus().unwrap_or_else(|| allowed.clone());
    let prime = select_prime_cpu(&present, &allowed)?;
    set_affinity_for_pid(0, &[prime])?;
    let pinned = affinity_for_pid(0).context("could not verify prime CPU affinity")?;
    if pinned != [prime] {
        bail!(
            "requested CPU {prime}, but the live affinity is {}",
            format_cpu_list(&pinned)
        );
    }
    println!("ostadix-device: execution pinned to CPU {prime}");
    Ok(prime)
}

fn select_prime_cpu(present: &[u32], allowed: &[u32]) -> Result<u32> {
    let prime = present
        .iter()
        .copied()
        .max()
        .context("could not identify the prime CPU")?;
    if !allowed.contains(&prime) {
        bail!(
            "prime CPU {prime} is outside the live affinity ({})",
            format_cpu_list(allowed)
        );
    }
    Ok(prime)
}

fn prime_command(command: PrimeCommand, json: bool) -> Result<u8> {
    match command {
        PrimeCommand::Status { pid } => {
            let status = collect_prime_status(pid.unwrap_or_else(std::process::id))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("pid:         {} (uid {})", status.pid, status.uid);
                println!(
                    "cpuset:      {}",
                    status.cpuset.as_deref().unwrap_or("unavailable")
                );
                println!("allowed:     {}", empty_as_unavailable(&status.allowed));
                match (status.prime_core, status.prime_available) {
                    (Some(cpu), Some(true)) => println!("prime:       CPU {cpu} available"),
                    (Some(cpu), Some(false)) => println!("prime:       CPU {cpu} withheld"),
                    _ => println!("prime:       unavailable"),
                }
            }
            Ok(0)
        }
        PrimeCommand::Attach { pid } => mutate_prime(PrimeAction::Attach, pid),
        PrimeCommand::Release { pid } => mutate_prime(PrimeAction::Release, pid),
    }
}

fn root_command(command: RootCommand) -> Result<u8> {
    // Root lookup is deliberately confined to this explicit command surface.
    let su = which::which("su").context(
        "su is unavailable; install or grant an Android root provider for this Termux app",
    )?;
    let mut process = ProcessCommand::new(su);
    match command {
        RootCommand::Status => {
            process.arg("-c").arg("exec /system/bin/id");
        }
        RootCommand::Shell => {}
        RootCommand::Run { command } => {
            let command = command
                .iter()
                .map(|argument| shell_quote_os(argument))
                .collect::<Result<Vec<_>>>()?
                .join(" ");
            process.arg("-c").arg(format!("exec {command}"));
        }
    }
    let status = run_child(&mut process).context("could not invoke the Android root provider")?;
    Ok(exit_status_code(status))
}

fn mutate_prime(action: PrimeAction, pid: u32) -> Result<u8> {
    let identity = validated_prime_identity(pid, caller_termux_uid()?, None)?;
    invoke_prime(action, &identity)?;
    Ok(0)
}

fn invoke_prime(action: PrimeAction, identity: &ProcessIdentity) -> Result<()> {
    if effective_uid() == Some(0) {
        return apply_prime_root(action, identity.pid, identity.uid, identity.start_time);
    }

    // This lookup and invocation occur only after an explicit mutating request.
    let su = which::which("su").context(
        "su is unavailable; grant this Termux app root access, then retry the explicit prime command",
    )?;
    let helper = format!("/proc/{}/exe", std::process::id());
    let action_name = match action {
        PrimeAction::Attach => "attach",
        PrimeAction::Release => "release",
    };
    let root_command = format!(
        "exec {} __prime-apply {} {} {} {}",
        shell_quote(&helper),
        action_name,
        identity.pid,
        identity.uid,
        identity.start_time
    );
    let mut command = ProcessCommand::new(su);
    command.arg("-c").arg(root_command);
    let status = run_child(&mut command).context("could not invoke su for prime mode")?;
    if !status.success() {
        bail!(
            "su could not apply prime mode (exit {}); grant the Termux app in your root manager and retry",
            exit_status_code(status)
        );
    }
    Ok(())
}

fn apply_prime_root(
    action: PrimeAction,
    pid: u32,
    expected_uid: u32,
    expected_start_time: u64,
) -> Result<()> {
    if effective_uid() != Some(0) {
        bail!("internal prime helper requires an existing root session");
    }
    if !is_android_app_uid(expected_uid) {
        bail!("refusing non-app uid {expected_uid}");
    }
    if !termux_owner_uids().contains(&expected_uid) {
        bail!("uid {expected_uid} is not the owner of this Termux installation");
    }
    let identity = validated_prime_identity(pid, expected_uid, Some(expected_start_time))?;
    let profile = match action {
        PrimeAction::Attach => TOP_APP_PROFILE,
        PrimeAction::Release => FOREGROUND_PROFILE,
    };

    let status = run_profile_transaction(
        action,
        || apply_profile_and_collect(action, identity.pid, profile),
        || restore_foreground_profile(identity.pid),
    )?;
    let action_name = match action {
        PrimeAction::Attach => "top-app",
        PrimeAction::Release => "foreground",
    };
    println!(
        "ostadix-device: PID {} -> {} (cpuset {}, CPUs {})",
        identity.pid,
        action_name,
        status.cpuset.as_deref().unwrap_or("unavailable"),
        empty_as_unavailable(&status.allowed)
    );
    Ok(())
}

fn apply_profile_and_collect(action: PrimeAction, pid: u32, profile: &str) -> Result<PrimeStatus> {
    let mut applied = false;
    if Path::new("/system/bin/settaskprofile").is_file() {
        applied = apply_task_profile(pid, profile)?;
    }
    if !applied {
        apply_cpuset_fallback(action, pid)?;
    }
    let status = collect_prime_status(pid)?;
    if matches!(action, PrimeAction::Attach) && status.prime_available == Some(false) {
        bail!(
            "Android applied top-app, but CPU {} is still outside PID {pid} affinity ({})",
            status.prime_core.unwrap_or(0),
            status.allowed
        );
    }
    Ok(status)
}

fn restore_foreground_profile(pid: u32) -> Result<()> {
    if Path::new("/system/bin/settaskprofile").is_file()
        && apply_task_profile(pid, FOREGROUND_PROFILE).unwrap_or(false)
    {
        return Ok(());
    }
    apply_cpuset_fallback(PrimeAction::Release, pid)
}

fn run_profile_transaction<T>(
    action: PrimeAction,
    apply: impl FnOnce() -> Result<T>,
    rollback: impl FnOnce() -> Result<()>,
) -> Result<T> {
    match apply() {
        Ok(value) => Ok(value),
        Err(error) if matches!(action, PrimeAction::Attach) => match rollback() {
            Ok(()) => Err(error.context("top-app failed; foreground profile was restored")),
            Err(rollback_error) => Err(error.context(format!(
                "top-app failed and foreground rollback also failed: {rollback_error:#}"
            ))),
        },
        Err(error) => Err(error),
    }
}

fn apply_task_profile(pid: u32, profile: &str) -> Result<bool> {
    let mut tids = fs::read_dir(format!("/proc/{pid}/task"))
        .with_context(|| format!("could not enumerate threads for PID {pid}"))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .collect::<Vec<_>>();
    tids.sort_unstable();
    if tids.is_empty() {
        bail!("PID {pid} has no visible threads");
    }
    for tid in tids {
        let status = ProcessCommand::new("/system/bin/settaskprofile")
            .arg(tid.to_string())
            .arg(profile)
            .status()
            .with_context(|| format!("could not apply Android task profile {profile}"))?;
        if !status.success() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn apply_cpuset_fallback(action: PrimeAction, pid: u32) -> Result<()> {
    let base = match action {
        PrimeAction::Attach => "/dev/cpuset/top-app",
        PrimeAction::Release => "/dev/cpuset/foreground",
    };
    let process_path = Path::new(base).join("cgroup.procs");
    let thread_path = Path::new(base).join("tasks");
    let destination = if process_path.is_file() {
        process_path
    } else {
        thread_path
    };
    write_existing_control_file(&destination, format!("{pid}\n").as_bytes()).with_context(|| {
        format!(
            "could not write fixed cpuset target {}",
            destination.display()
        )
    })
}

fn write_existing_control_file(path: &Path, value: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let mut file = options
        .open(path)
        .with_context(|| format!("could not open existing {}", path.display()))?;
    file.write_all(value)
        .with_context(|| format!("could not write {}", path.display()))
}

fn collect_prime_status(pid: u32) -> Result<PrimeStatus> {
    let expected_uid = process_uids(pid)?[0];
    let identity = validated_prime_identity(pid, expected_uid, None)?;
    let allowed = affinity_for_pid(pid as i32)
        .or_else(|| affinity_from_proc_status(format!("/proc/{pid}/status")))
        .unwrap_or_default();
    let present = present_cpus().unwrap_or_else(|| allowed.clone());
    let prime = present.iter().copied().max();
    Ok(PrimeStatus {
        schema: PRIME_SCHEMA,
        pid,
        uid: identity.uid,
        cpuset: read_trimmed(format!("/proc/{pid}/cpuset")),
        allowed: format_cpu_list(&allowed),
        prime_core: prime,
        prime_available: prime.map(|cpu| allowed.contains(&cpu)),
    })
}

fn caller_termux_uid() -> Result<u32> {
    let euid = effective_uid().context("could not determine the effective uid")?;
    if euid != 0 {
        if !is_android_app_uid(euid) {
            bail!("uid {euid} is not an Android app uid");
        }
        if !termux_owner_uids().contains(&euid) {
            bail!("uid {euid} is not the owner of this Termux installation");
        }
        return Ok(euid);
    }
    termux_owner_uids()
        .into_iter()
        .next()
        .context("could not identify the Termux app uid from owned paths")
}

fn validated_prime_identity(
    pid: u32,
    expected_uid: u32,
    expected_start_time: Option<u64>,
) -> Result<ProcessIdentity> {
    if pid == 0 {
        bail!("PID must be positive");
    }
    let uids = process_uids(pid)?;
    if !uids.iter().all(|uid| *uid == uids[0]) {
        bail!("refusing PID {pid}: real/effective/saved/filesystem UIDs differ");
    }
    let uid = uids[0];
    if uid != expected_uid {
        bail!("refusing PID {pid}: uid {uid} does not match Termux uid {expected_uid}");
    }
    if !is_android_app_uid(uid) {
        bail!("refusing PID {pid}: uid {uid} is not an Android app uid");
    }
    let start_time = process_start_time(pid)?;
    if expected_start_time.is_some_and(|expected| expected != start_time) {
        bail!("refusing PID {pid}: process start time changed (possible PID reuse)");
    }
    Ok(ProcessIdentity {
        pid,
        uid,
        start_time,
    })
}

fn process_uids(pid: u32) -> Result<[u32; 4]> {
    let status = fs::read_to_string(format!("/proc/{pid}/status"))
        .with_context(|| format!("PID {pid} does not exist or is not readable"))?;
    parse_uids(&status).with_context(|| format!("PID {pid} has no valid Uid field"))
}

fn parse_uids(status: &str) -> Result<[u32; 4]> {
    let line = status
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .context("missing Uid field")?;
    let values = line
        .split_whitespace()
        .skip(1)
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("invalid Uid field")?;
    if values.len() != 4 {
        bail!("Uid field must contain four values");
    }
    Ok([values[0], values[1], values[2], values[3]])
}

fn process_start_time(pid: u32) -> Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))
        .with_context(|| format!("could not read stat for PID {pid}"))?;
    parse_process_start_time(&stat)
}

fn parse_process_start_time(stat: &str) -> Result<u64> {
    let end = stat
        .rfind(')')
        .context("invalid process stat command field")?;
    let fields = stat[end + 1..].split_whitespace().collect::<Vec<_>>();
    let value = fields
        .get(19)
        .context("process stat is missing starttime")?;
    value.parse::<u64>().context("invalid process starttime")
}

fn is_android_app_uid(uid: u32) -> bool {
    let app_id = uid % 100_000;
    (10_000..=19_999).contains(&app_id)
}

#[cfg(unix)]
fn termux_owner_uids() -> BTreeSet<u32> {
    let mut candidates = Vec::new();
    for name in ["TERMUX__HOME", "HOME"] {
        if let Some(path) = nonempty_env_path(name) {
            candidates.push(path);
        }
    }
    candidates.push(PathBuf::from("/data/data/com.termux/files/home"));
    candidates.push(PathBuf::from("/home"));
    if let Ok(executable) = env::current_exe() {
        candidates.push(executable);
    }
    candidates
        .into_iter()
        .filter_map(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.uid())
        .filter(|uid| is_android_app_uid(*uid))
        .collect()
}

#[cfg(not(unix))]
fn termux_owner_uids() -> BTreeSet<u32> {
    BTreeSet::new()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn shell_quote_os(value: &OsStr) -> Result<String> {
    let value = value
        .to_str()
        .context("root command arguments must be valid UTF-8")?;
    Ok(shell_quote(value))
}

fn evaluator_path(root: &Path) -> Option<PathBuf> {
    let built = root.join("target/release/O");
    if built.is_file() {
        return Some(built);
    }
    which::which("O").ok()
}

fn termux_loader_path() -> Option<OsString> {
    let prefix = nonempty_env_path("PREFIX")?;
    let library = prefix.join("lib");
    let mut paths = vec![library];
    if let Some(existing) = env::var_os("LD_LIBRARY_PATH") {
        paths.extend(env::split_paths(&existing));
    }
    let mut seen = BTreeSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    env::join_paths(paths).ok()
}

fn termux_detected() -> bool {
    env::var_os("TERMUX_VERSION").is_some()
        || nonempty_env_path("PREFIX")
            .is_some_and(|prefix| prefix.to_string_lossy().contains("com.termux"))
        || Path::new("/data/data/com.termux/files/usr").is_dir()
}

fn is_private_termux_path(path: &Path) -> bool {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match env::current_dir() {
            Ok(current) => current.join(path),
            Err(_) => return false,
        }
    };
    let mut roots = vec![
        PathBuf::from("/home"),
        PathBuf::from("/data/data/com.termux/files/home"),
        PathBuf::from("/data/user/0/com.termux/files/home"),
    ];
    for name in ["HOME", "TERMUX__HOME"] {
        if let Some(root) = nonempty_env_path(name) {
            roots.push(root);
        }
    }
    roots.into_iter().any(|root| absolute.starts_with(root))
}

fn getprop(name: &str) -> Option<String> {
    let command = if Path::new("/system/bin/getprop").is_file() {
        PathBuf::from("/system/bin/getprop")
    } else {
        which::which("getprop").ok()?
    };
    let output = ProcessCommand::new(command).arg(name).output().ok()?;
    if !output.status.success() {
        return None;
    }
    nonempty_string(String::from_utf8_lossy(&output.stdout).trim())
}

fn command_first_line(command: &str, args: &[&str]) -> Option<String> {
    let path = which::which(command).ok()?;
    let output = ProcessCommand::new(path).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    output
        .stdout
        .split(|byte| *byte == b'\n')
        .next()
        .and_then(|line| nonempty_string(String::from_utf8_lossy(line).trim()))
}

fn nonempty_string(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn battery_status() -> Option<BatteryStatus> {
    let command = which::which("termux-battery-status").ok()?;
    let mut child = ProcessCommand::new(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + BATTERY_TIMEOUT;
    let status = loop {
        match child.try_wait().ok()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => thread::sleep(Duration::from_millis(25)),
        }
    };
    if !status.success() {
        return None;
    }
    let mut bytes = Vec::new();
    child.stdout.take()?.read_to_end(&mut bytes).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    Some(BatteryStatus {
        percentage: json_number(&value, "percentage"),
        temperature_c: json_number(&value, "temperature"),
        health: json_string(&value, "health"),
        status: json_string(&value, "status"),
    })
}

fn json_number(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key).and_then(|item| {
        item.as_f64()
            .or_else(|| item.as_str().and_then(|text| text.parse().ok()))
    })
}

fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_string)
}

fn memory_status() -> MemoryStatus {
    let Some(meminfo) = fs::read_to_string("/proc/meminfo").ok() else {
        return MemoryStatus::default();
    };
    MemoryStatus {
        total_mib: meminfo_kib(&meminfo, "MemTotal").map(|kib| kib / 1024),
        available_mib: meminfo_kib(&meminfo, "MemAvailable").map(|kib| kib / 1024),
    }
}

fn meminfo_kib(meminfo: &str, key: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name != key {
            return None;
        }
        value.split_whitespace().next()?.parse().ok()
    })
}

fn present_cpus() -> Option<Vec<u32>> {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok()?;
    let cpus = cpuinfo
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name.trim() == "processor")
                .then(|| value.trim().parse::<u32>().ok())
                .flatten()
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    (!cpus.is_empty()).then_some(cpus)
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn affinity_for_pid(pid: i32) -> Option<Vec<u32>> {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::sched_getaffinity(
            pid,
            std::mem::size_of::<libc::cpu_set_t>(),
            &mut set as *mut libc::cpu_set_t,
        )
    };
    if result != 0 {
        return None;
    }
    let cpus = (0..libc::CPU_SETSIZE)
        .filter(|cpu| unsafe { libc::CPU_ISSET(*cpu, &set) })
        .map(|cpu| cpu as u32)
        .collect::<Vec<_>>();
    Some(cpus)
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn set_affinity_for_pid(pid: i32, cpus: &[u32]) -> Result<()> {
    if cpus.is_empty() {
        bail!("refusing an empty CPU affinity");
    }
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    for cpu in cpus {
        let cpu = usize::try_from(*cpu).context("CPU index does not fit usize")?;
        if cpu >= libc::CPU_SETSIZE {
            bail!("CPU {cpu} exceeds the platform affinity limit");
        }
        unsafe { libc::CPU_SET(cpu, &mut set) };
    }
    let result = unsafe {
        libc::sched_setaffinity(
            pid,
            std::mem::size_of::<libc::cpu_set_t>(),
            &set as *const libc::cpu_set_t,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("sched_setaffinity failed");
    }
    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn affinity_for_pid(_pid: i32) -> Option<Vec<u32>> {
    None
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn set_affinity_for_pid(_pid: i32, _cpus: &[u32]) -> Result<()> {
    bail!("CPU affinity control is unsupported on this host")
}

fn parse_cpu_list(value: &str) -> Result<Vec<u32>> {
    let mut cpus = BTreeSet::new();
    for component in value.trim().split(',').filter(|part| !part.is_empty()) {
        if let Some((start, end)) = component.split_once('-') {
            let start = start.trim().parse::<u32>().context("invalid CPU range")?;
            let end = end.trim().parse::<u32>().context("invalid CPU range")?;
            if start > end || end - start > 4096 {
                bail!("invalid CPU range {component}");
            }
            cpus.extend(start..=end);
        } else {
            cpus.insert(component.trim().parse().context("invalid CPU number")?);
        }
    }
    Ok(cpus.into_iter().collect())
}

fn affinity_from_proc_status(path: impl AsRef<Path>) -> Option<Vec<u32>> {
    let status = fs::read_to_string(path).ok()?;
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:").map(str::trim))?;
    parse_cpu_list(value).ok()
}

fn format_cpu_list(cpus: &[u32]) -> String {
    if cpus.is_empty() {
        return String::new();
    }
    let mut sorted = cpus.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut ranges = Vec::new();
    let mut start = sorted[0];
    let mut end = start;
    for cpu in sorted.into_iter().skip(1) {
        if cpu == end + 1 {
            end = cpu;
        } else {
            push_cpu_range(&mut ranges, start, end);
            start = cpu;
            end = cpu;
        }
    }
    push_cpu_range(&mut ranges, start, end);
    ranges.join(",")
}

fn push_cpu_range(ranges: &mut Vec<String>, start: u32, end: u32) {
    if start == end {
        ranges.push(start.to_string());
    } else {
        ranges.push(format!("{start}-{end}"));
    }
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .and_then(|value| nonempty_string(value.trim()))
}

fn read_lines(path: impl AsRef<Path>) -> Vec<String> {
    fs::read_to_string(path)
        .map(|value| {
            value
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(unix)]
fn effective_uid() -> Option<u32> {
    Some(unsafe { libc::geteuid() })
}

#[cfg(not(unix))]
fn effective_uid() -> Option<u32> {
    None
}

#[cfg(unix)]
fn run_child(command: &mut ProcessCommand) -> Result<ExitStatus> {
    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = Signals::new([SIGHUP, SIGINT, SIGTERM])
        .context("could not install child signal forwarding")?;
    let handle = signals.handle();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            handle.close();
            return Err(error.into());
        }
    };
    let child_pid = child.id() as i32;
    let forwarder = thread::spawn(move || {
        for signal in signals.forever() {
            let _ = unsafe { libc::kill(child_pid, signal) };
        }
    });
    let status = child.wait();
    handle.close();
    let _ = forwarder.join();
    status.map_err(Into::into)
}

#[cfg(not(unix))]
fn run_child(command: &mut ProcessCommand) -> Result<ExitStatus> {
    command.status().map_err(Into::into)
}

fn exit_status_code(status: ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return u8::try_from(code).unwrap_or(1);
    }
    #[cfg(unix)]
    if let Some(signal) = status.signal() {
        return u8::try_from(128 + signal).unwrap_or(1);
    }
    1
}

fn show(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("unavailable")
}

fn show_number(value: Option<u64>) -> String {
    value
        .map(|number| number.to_string())
        .unwrap_or_else(|| "unavailable".to_string())
}

fn show_float(value: Option<f64>) -> String {
    value
        .map(|number| format!("{number:.1}"))
        .unwrap_or_else(|| "unavailable".to_string())
}

fn empty_as_unavailable(value: &str) -> &str {
    if value.is_empty() {
        "unavailable"
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_lists_parse_sort_and_format_ranges() {
        assert_eq!(
            parse_cpu_list("0-3,7,4-6").unwrap(),
            (0..=7).collect::<Vec<_>>()
        );
        assert_eq!(format_cpu_list(&[7, 0, 2, 1, 6]), "0-2,6-7");
        assert!(parse_cpu_list("7-3").is_err());
    }

    #[test]
    fn prime_selection_requires_the_highest_present_cpu_to_be_allowed() {
        assert_eq!(
            select_prime_cpu(&(0..=7).collect::<Vec<_>>(), &(0..=7).collect::<Vec<_>>()).unwrap(),
            7
        );
        let error = select_prime_cpu(&(0..=7).collect::<Vec<_>>(), &(0..=6).collect::<Vec<_>>())
            .unwrap_err()
            .to_string();
        assert!(error.contains("prime CPU 7"));
    }

    #[test]
    fn attach_transaction_rolls_back_every_application_error() {
        let events = std::cell::RefCell::new(Vec::new());
        let result = run_profile_transaction::<()>(
            PrimeAction::Attach,
            || {
                events.borrow_mut().push("attach");
                bail!("verification failed")
            },
            || {
                events.borrow_mut().push("foreground");
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(*events.borrow(), ["attach", "foreground"]);
    }

    #[test]
    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn affinity_syscall_pins_and_restores_the_current_thread() {
        let original = affinity_for_pid(0).expect("current affinity");
        let selected = *original.first().expect("at least one allowed CPU");
        let pin_result = set_affinity_for_pid(0, &[selected]);
        let pinned = affinity_for_pid(0);
        let restore_result = set_affinity_for_pid(0, &original);
        let restored = affinity_for_pid(0);

        pin_result.expect("pin one allowed CPU");
        assert_eq!(pinned, Some(vec![selected]));
        restore_result.expect("restore original affinity");
        assert_eq!(restored, Some(original));
    }

    #[test]
    fn parses_all_four_process_uids() {
        assert_eq!(
            parse_uids("Name:\tO\nUid:\t10402\t10402\t10402\t10402\n").unwrap(),
            [10402; 4]
        );
        assert!(parse_uids("Uid:\t10402\t10402\n").is_err());
    }

    #[test]
    fn parses_start_time_after_parenthesized_command() {
        let stat = "42 (O worker (fast)) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 123456 20";
        assert_eq!(parse_process_start_time(stat).unwrap(), 123456);
    }

    #[test]
    fn recognizes_android_multiuser_app_ids() {
        assert!(is_android_app_uid(10402));
        assert!(is_android_app_uid(110402));
        assert!(!is_android_app_uid(0));
        assert!(!is_android_app_uid(2000));
    }

    #[test]
    fn shell_quote_does_not_expose_metacharacters() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }

    #[test]
    fn meminfo_parser_uses_named_kib_fields() {
        let input = "MemTotal:       16384000 kB\nMemAvailable:    8192000 kB\n";
        assert_eq!(meminfo_kib(input, "MemTotal"), Some(16_384_000));
        assert_eq!(meminfo_kib(input, "SwapTotal"), None);
    }
}
