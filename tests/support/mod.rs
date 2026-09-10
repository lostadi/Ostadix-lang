//! Shared policy for integration tests that depend on hosted runtimes.
//!
//! Rust's built-in test harness has no runtime skip result. Developer runs may
//! therefore skip an unavailable optional prerequisite explicitly, while CI can
//! set `OSTADIX_TEST_RUNTIME_POLICY=required` so the same absence is a failure
//! rather than false-positive execution evidence.

// Each integration-test crate includes this module independently and uses a
// different subset of its shared helpers.
#![allow(dead_code)]

use serde::Deserialize;
use std::collections::HashMap;
use std::io;
use std::process::{Child, Command, Output};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

/// Normalize non-semantic permission bits from an assembled fixture, then
/// restore the fingerprint invariant. Production bundles intentionally preserve
/// all Unix mode bits; canonical goldens must not inherit the checkout umask.
pub(crate) fn normalize_project_fixture_modes(
    mut bundle: o_lang::project::ProjectBundle,
) -> o_lang::project::ProjectBundle {
    for file in &mut bundle.files {
        file.unix_mode = if file.is_symlink() {
            None
        } else if file.executable {
            Some(0o100755)
        } else {
            Some(0o100644)
        };
    }
    bundle.root_fingerprint = o_lang::project::bundle::fingerprint(&bundle.files);
    bundle
}

const POLICY_ENV: &str = "OSTADIX_TEST_RUNTIME_POLICY";
const RUNTIME_PROBE_CONTRACT: &str = include_str!("../../ci/test-suites.toml");

const MAX_EXECUTABLE_BUSY_RETRIES: usize = 10;

#[derive(Debug, Deserialize)]
struct RuntimeProbeManifest {
    schema: String,
    runtime_probes: HashMap<String, RuntimeProbe>,
}

#[derive(Debug, Deserialize)]
struct RuntimeProbe {
    executable: String,
    probe_args: Vec<String>,
}

fn runtime_probe(name: &str) -> &'static RuntimeProbe {
    static PROBES: OnceLock<HashMap<String, RuntimeProbe>> = OnceLock::new();
    let probes = PROBES.get_or_init(|| {
        let manifest: RuntimeProbeManifest = toml::from_str(RUNTIME_PROBE_CONTRACT)
            .expect("ci/test-suites.toml must contain valid runtime probe contracts");
        assert_eq!(
            manifest.schema, "ostadix.ci-test-suites/v2",
            "ci/test-suites.toml has an unsupported runtime probe schema"
        );
        manifest.runtime_probes
    });
    probes.get(name).unwrap_or_else(|| {
        panic!("runtime {name:?} has no authoritative probe in ci/test-suites.toml")
    })
}

fn retry_executable_busy<T>(mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    for attempt in 0..=MAX_EXECUTABLE_BUSY_RETRIES {
        match operation() {
            Ok(value) => return Ok(value),
            #[cfg(unix)]
            Err(error)
                if error.raw_os_error() == Some(libc::ETXTBSY)
                    && attempt < MAX_EXECUTABLE_BUSY_RETRIES =>
            {
                // Some Linux filesystems can briefly retain deny-write state
                // after a freshly copied executable's writer is closed. The
                // test path is private, so retry only this transient loader
                // condition; persistent writers and every other error fail.
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the bounded executable-busy loop always returns")
}

pub(crate) fn spawn_private_executable(command: &mut Command) -> io::Result<Child> {
    retry_executable_busy(|| command.spawn())
}

pub(crate) fn output_private_executable(command: &mut Command) -> io::Result<Output> {
    retry_executable_busy(|| command.output())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimePolicy {
    Optional,
    Required,
}

fn runtime_policy() -> RuntimePolicy {
    match std::env::var(POLICY_ENV).as_deref() {
        Ok("required") => RuntimePolicy::Required,
        Ok("optional") | Err(std::env::VarError::NotPresent) => RuntimePolicy::Optional,
        Ok(value) => panic!("invalid {POLICY_ENV}={value:?}; expected `optional` or `required`"),
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("{POLICY_ENV} must be valid UTF-8")
        }
    }
}

fn runtime_available(name: &str) -> bool {
    let probe = runtime_probe(name);
    Command::new(&probe.executable)
        .args(&probe.probe_args)
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Return true when every runtime is invocable. Missing prerequisites are
/// explicit optional skips for developer portability and hard failures under
/// the release-evidence CI policy.
pub(crate) fn require_runtimes(names: &[&str]) -> bool {
    let policy = runtime_policy();
    let missing = names
        .iter()
        .copied()
        .filter(|name| !runtime_available(name))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return true;
    }

    let test = std::thread::current()
        .name()
        .unwrap_or("unnamed-integration-test")
        .to_string();
    let missing = missing.join(",");
    match policy {
        RuntimePolicy::Optional => {
            eprintln!(
                "runtime-evidence status=skip-optional policy=optional test={test} missing={missing}"
            );
            false
        }
        RuntimePolicy::Required => panic!(
            "runtime-evidence status=missing-required policy=required test={test} missing={missing}"
        ),
    }
}

pub(crate) fn require_runtime(name: &str) -> bool {
    require_runtimes(&[name])
}
