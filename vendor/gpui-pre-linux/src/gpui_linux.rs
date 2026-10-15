#![cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux;

pub use linux::{
    ClipboardHtml, HTML_MIME_TYPE, NativeInput, NativeKey, current_native_input, current_platform, linux_platform,
    with_clipboard_html,
};
