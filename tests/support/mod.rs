//! Shared policy for integration tests that depend on hosted runtimes.
//!
//! Rust's built-in test harness has no runtime skip result. Developer runs may
//! therefore skip an unavailable optional prerequisite explicitly, while CI can
//! set `OSTADIX_TEST_RUNTIME_POLICY=required` so the same absence is a failure
//! rather than false-positive execution evidence.

// Each integration-test crate includes this module independently and uses a
// different subset of its shared helpers.
#![allow(dead_code)]

use std::io;
use std::process::{Child, Command, Output};
use std::thread;
use std::time::Duration;

const POLICY_ENV: &str = "OSTADIX_TEST_RUNTIME_POLICY";

const MAX_EXECUTABLE_BUSY_RETRIES: usize = 10;

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
    Command::new(name)
        .arg("--version")
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
