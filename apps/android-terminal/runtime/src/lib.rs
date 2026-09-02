//! Supported JNI boundary for the standalone Ostadix Terminal APK.
//!
//! The Android app embeds the stable `ostadix_api::Runtime` instead of
//! executing a binary copied into writable app storage. Android 10 and newer
//! intentionally disallow that writable-code pattern.

use jni::objects::{JClass, JString};
use jni::sys::{jlong, jstring};
use jni::JNIEnv;
use ostadix_api::{OValue, Runtime};
use serde_json::json;
use std::ptr;
use std::sync::Mutex;

struct AndroidRuntime {
    inner: Mutex<Runtime>,
}

fn java_string(env: JNIEnv<'_>, value: String) -> jstring {
    match env.new_string(value) {
        Ok(text) => text.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

fn error_json(stage: &str, message: impl AsRef<str>) -> String {
    json!({
        "ok": false,
        "stage": stage,
        "message": message.as_ref(),
    })
    .to_string()
}

/// Create one app-owned evaluator. The handle is never shared with Java code
/// other than as an opaque token and is serialized by the Java wrapper.
#[no_mangle]
pub extern "system" fn Java_org_ostadix_terminal_OstadixRuntime_nativeCreate(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    shim_dir: JString<'_>,
) -> jlong {
    let shim_dir: String = match env.get_string(&shim_dir) {
        Ok(value) => value.into(),
        Err(_) => return 0,
    };
    let runtime = AndroidRuntime {
        // This private executor thread never changes Landlock/seccomp
        // authority between evaluations. Reuse is therefore safe here; the
        // core still rebuilds workers whenever Android affinity changes.
        inner: Mutex::new(Runtime::new(shim_dir).with_reusable_local_workers()),
    };
    Box::into_raw(Box::new(runtime)) as jlong
}

/// Evaluate a complete O document and return a small JSON envelope. JNI is
/// deliberately coarse-grained: terminal rendering never crosses this edge.
#[no_mangle]
pub extern "system" fn Java_org_ostadix_terminal_OstadixRuntime_nativeEvaluate(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    source: JString<'_>,
) -> jstring {
    if handle == 0 {
        return java_string(env, error_json("runtime", "runtime is closed"));
    }

    let source: String = match env.get_string(&source) {
        Ok(value) => value.into(),
        Err(error) => {
            return java_string(env, error_json("jni", format!("invalid source: {error}")))
        }
    };

    // SAFETY: `handle` is created by `nativeCreate`, access is serialized by
    // the Java wrapper and the allocation is released exactly once by
    // `nativeDestroy`.
    let runtime = unsafe { &*(handle as *mut AndroidRuntime) };
    let mut runtime = match runtime.inner.lock() {
        Ok(runtime) => runtime,
        Err(_) => return java_string(env, error_json("runtime", "runtime lock is poisoned")),
    };

    let response = match runtime.evaluate(&source) {
        Ok(value) => {
            let output = match &value {
                OValue::Text { v } => v.utf8.clone(),
                OValue::Html { v } => v.to_string(),
                other => other.to_string(),
            };
            json!({
                "ok": true,
                "type": value.type_name(),
                "output": output,
            })
            .to_string()
        }
        Err(error) => error_json(&error.stage().to_string(), error.message()),
    };
    java_string(env, response)
}

#[no_mangle]
pub extern "system" fn Java_org_ostadix_terminal_OstadixRuntime_nativeVersion(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
) -> jstring {
    java_string(env, env!("CARGO_PKG_VERSION").to_string())
}

#[no_mangle]
pub extern "system" fn Java_org_ostadix_terminal_OstadixRuntime_nativeDestroy(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    if handle != 0 {
        // SAFETY: ownership of the pointer is returned exactly once by the
        // Java wrapper's synchronized `close` method.
        unsafe { drop(Box::from_raw(handle as *mut AndroidRuntime)) };
    }
}
