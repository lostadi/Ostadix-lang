//! Shared policy for integration tests that depend on hosted runtimes.
//!
//! Rust's built-in test harness has no runtime skip result. Developer runs may
//! therefore skip an unavailable optional prerequisite explicitly, while CI can
//! set `OSTADIX_TEST_RUNTIME_POLICY=required` so the same absence is a failure
//! rather than false-positive execution evidence.

use std::process::Command;

const POLICY_ENV: &str = "OSTADIX_TEST_RUNTIME_POLICY";

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
