//! The Android adapter: cpal's AAudio backend asks `ndk_context` for the app's JavaVM and
//! Context (device names, the default device), which an app not built on `android-activity`
//! must hand over itself. The app keeps its own JNI export (named after its Java package) and
//! calls [`init`] from it once, before any audio starts:
//!
//! ```ignore
//! #[no_mangle]
//! pub unsafe extern "system" fn Java_com_example_app_NativeInit_init(
//!     env: *mut jni_sys::JNIEnv, _class: jni_sys::jclass, context: jni_sys::jobject,
//! ) {
//!     djrust::android::init(env, context);
//! }
//! ```
#![allow(unsafe_code)]

use jni_sys::{jobject, JNIEnv, JavaVM, JNI_OK};
use std::sync::Once;

static INIT: Once = Once::new();

/// Hand the JavaVM and a global ref to `context` to `ndk_context`. Only the first call does
/// anything.
///
/// # Safety
/// Called on a JVM thread with a valid `env` and a live Context.
pub unsafe fn init(env: *mut JNIEnv, context: jobject) {
    INIT.call_once(|| {
        let f = &**env;
        let mut vm: *mut JavaVM = std::ptr::null_mut();
        if (f.GetJavaVM.unwrap())(env, &mut vm) != JNI_OK || vm.is_null() {
            return;
        }
        // a global ref: the Context outlives this call (kept for the process's life)
        let ctx = (f.NewGlobalRef.unwrap())(env, context);
        ndk_context::initialize_android_context(vm.cast(), ctx.cast());
    });
}
