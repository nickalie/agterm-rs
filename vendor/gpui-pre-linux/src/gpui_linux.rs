#![cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux;

pub use linux::{NativeInput, NativeKey, current_native_input, current_platform, linux_platform};
