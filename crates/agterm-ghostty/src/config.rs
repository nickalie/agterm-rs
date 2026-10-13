//! A libghostty configuration: load files, finalize, read typed values back, collect diagnostics.

use std::ffi::{CStr, CString, c_void};
use std::marker::PhantomData;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use agterm_ghostty_sys as sys;

use crate::Error;

/// A problem met while loading a configuration: a libghostty diagnostic (an unknown key, a bad value) or a file
/// this wrapper could not hand to libghostty at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The message as libghostty words it, or `<path>: <io error>` for a file that could not be read.
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// An RGB color, as libghostty's `ghostty_config_color_s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

mod sealed {
    /// Bytes `ghostty_config_get` may write into: larger than any value it writes (the biggest, the 256-color
    /// palette, is 768 bytes) and aligned for any of them, so a read is memory-safe whatever the key's type is.
    pub const SLOT_BYTES: usize = 4096;

    #[repr(C, align(16))]
    pub struct Slot(pub [u8; SLOT_BYTES]);

    impl Slot {
        pub fn head<const N: usize>(&self) -> [u8; N] {
            self.0.first_chunk::<N>().copied().unwrap_or([0; N])
        }
    }

    pub trait Sealed: Sized {
        fn decode(slot: &Slot) -> Self;
    }
}

use sealed::{SLOT_BYTES, Slot};

/// A type `ghostty_config_get` writes a key's value as. Decoding never reinterprets bytes as a pointer, so even a
/// key bound to the wrong type cannot cause undefined behavior, only a wrong value.
pub trait ConfigValue: sealed::Sealed {}

macro_rules! numeric_value {
    ($($ty:ty),*) => {$(
        impl ConfigValue for $ty {}
        impl sealed::Sealed for $ty {
            fn decode(slot: &Slot) -> Self {
                <$ty>::from_ne_bytes(slot.head())
            }
        }
    )*};
}

// `u8` and `u32` keys arrive as `c_uint`, `i16` ones as `c_short`.
numeric_value!(f32, f64, u32, i16);

impl ConfigValue for bool {}
impl sealed::Sealed for bool {
    fn decode(slot: &Slot) -> Self {
        slot.head::<1>()[0] != 0
    }
}

impl ConfigValue for Color {}
impl sealed::Sealed for Color {
    fn decode(slot: &Slot) -> Self {
        let [r, g, b] = slot.head();
        Self { r, g, b }
    }
}

/// The 256-color palette, as libghostty's `ghostty_config_palette_s`: the 16 ANSI colors first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette(pub [Color; 256]);

impl Palette {
    /// The 16 ANSI colors by slot.
    #[must_use]
    pub fn ansi(&self) -> &[Color] {
        &self.0[..16]
    }
}

impl ConfigValue for Palette {}
impl sealed::Sealed for Palette {
    fn decode(slot: &Slot) -> Self {
        let bytes: [u8; 768] = slot.head();
        Self(std::array::from_fn(|index| {
            let [r, g, b] = [bytes[index * 3], bytes[index * 3 + 1], bytes[index * 3 + 2]];
            Color { r, g, b }
        }))
    }
}

/// A configuration key bound to the type libghostty reports it as. The predefined ones are in [`keys`].
#[derive(Debug)]
pub struct ConfigKey<T> {
    name: &'static str,
    value: PhantomData<fn() -> T>,
}

// By hand: a derive would require `T: Copy`, which the key does not need.
impl<T> Clone for ConfigKey<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for ConfigKey<T> {}

impl<T: ConfigValue> ConfigKey<T> {
    /// Bind `name` to `T`. A wrong `T` reads a garbage value, never out of bounds (see [`ConfigValue`]); check
    /// the key's Zig type in ghostty's `src/config/Config.zig` and how `src/config/c_get.zig` writes it.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            value: PhantomData,
        }
    }

    /// The key as it appears in a config file.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

/// Keys agterm reads, typed after ghostty's `Config.zig` at `GHOSTTY_REV`.
pub mod keys {
    use super::{Color, ConfigKey};

    pub const FONT_SIZE: ConfigKey<f32> = ConfigKey::new("font-size");
    pub const BACKGROUND: ConfigKey<Color> = ConfigKey::new("background");
    pub const FOREGROUND: ConfigKey<Color> = ConfigKey::new("foreground");
    /// The 256-color palette; its first 16 entries are the theme's ANSI colors.
    pub const PALETTE: ConfigKey<super::Palette> = ConfigKey::new("palette");
    pub const CURSOR_OPACITY: ConfigKey<f64> = ConfigKey::new("cursor-opacity");
    /// Optional in ghostty: unset reads as `None`.
    pub const CURSOR_STYLE_BLINK: ConfigKey<bool> = ConfigKey::new("cursor-style-blink");
    pub const MOUSE_HIDE_WHILE_TYPING: ConfigKey<bool> = ConfigKey::new("mouse-hide-while-typing");
    /// A packed struct, read as its bits: `cursor`, `sudo`, `title`, `ssh-env`, `ssh-terminfo`, `path` from bit 0.
    pub const SHELL_INTEGRATION_FEATURES: ConfigKey<u32> = ConfigKey::new("shell-integration-features");
    /// The static window title; unset reads as `None`.
    pub const TITLE: super::StringKey = super::StringKey::new("title");
}

/// A key whose value libghostty hands out as a NUL-terminated string. Its own type, with only the predefined
/// [`keys`], because reading one dereferences what libghostty wrote: a key of another type would make that pointer up.
#[derive(Debug, Clone, Copy)]
pub struct StringKey {
    name: &'static str,
}

impl StringKey {
    const fn new(name: &'static str) -> Self {
        Self { name }
    }

    /// The key as it appears in a config file.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

/// An owned `ghostty_config_t`. Not `Send`: it lives on the thread that owns the libghostty app.
#[derive(Debug)]
pub struct Config {
    raw: NonNull<c_void>,
    /// Files that never reached libghostty, which reports nothing for them (it only logs).
    load_errors: Vec<Diagnostic>,
}

impl Config {
    /// An empty configuration (libghostty's defaults), initialising libghostty first if needed.
    ///
    /// # Errors
    ///
    /// [`Error::Init`] when libghostty cannot be initialised, [`Error::Null`] when it cannot allocate.
    pub fn new() -> Result<Self, Error> {
        crate::init()?;
        // SAFETY: libghostty is initialised; ghostty_config_new has no other precondition.
        let raw = unsafe { sys::ghostty_config_new() };
        let raw = NonNull::new(raw).ok_or(Error::Null("ghostty_config_new"))?;
        Ok(Self {
            raw,
            load_errors: Vec::new(),
        })
    }

    /// The raw handle, still owned by `self`, for the libghostty calls that take a config.
    #[must_use]
    pub fn as_ptr(&self) -> sys::ghostty_config_t {
        self.raw.as_ptr()
    }

    /// Load one config file. A path that cannot be opened or is not a regular file is recorded as a [`Diagnostic`]
    /// instead of being passed on: libghostty only logs it, and asserts on a relative path, so the path is made
    /// absolute here first.
    pub fn load_file(&mut self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        let absolute = match loadable_path(path) {
            Ok(absolute) => absolute,
            Err(err) => return self.note(format!("{}: {err}", path.display())),
        };
        let Ok(c_path) = CString::new(absolute.as_os_str().as_bytes()) else {
            return self.note(format!("{}: path contains a NUL byte", path.display()));
        };
        // SAFETY: raw is a live config owned by self; c_path is an absolute, NUL-terminated path that outlives
        // the call.
        unsafe { sys::ghostty_config_load_file(self.raw.as_ptr(), c_path.as_ptr()) };
    }

    /// Load config lines from memory. libghostty only reads files, so the text goes through a private temp file that
    /// is removed once loaded; a failure to write it is recorded as a [`Diagnostic`].
    pub fn load_text(&mut self, text: &str) {
        let written = tempfile::Builder::new()
            .prefix("agterm-ghostty-")
            .suffix(".conf")
            .tempfile()
            .and_then(|mut file| {
                std::io::Write::write_all(&mut file, text.as_bytes())?;
                Ok(file)
            });
        match written {
            Ok(file) => self.load_file(file.path()),
            Err(err) => self.note(format!("config text: {err}")),
        }
    }

    /// Load ghostty's own default files (`$XDG_CONFIG_HOME/ghostty/config` and friends).
    pub fn load_default_files(&mut self) {
        // SAFETY: raw is a live config owned by self.
        unsafe { sys::ghostty_config_load_default_files(self.raw.as_ptr()) };
    }

    /// Load the `config-file` entries the loaded files named, recursively up to libghostty's limit.
    pub fn load_recursive_files(&mut self) {
        // SAFETY: raw is a live config owned by self.
        unsafe { sys::ghostty_config_load_recursive_files(self.raw.as_ptr()) };
    }

    /// Resolve derived values; required before the config is handed to an app or a surface.
    pub fn finalize(&mut self) {
        // SAFETY: raw is a live config owned by self.
        unsafe { sys::ghostty_config_finalize(self.raw.as_ptr()) };
    }

    /// Read a key's current value. `None` when the key is unknown to libghostty, its type has no C
    /// representation, or it is an unset optional.
    #[must_use]
    pub fn get<T: ConfigValue>(&self, key: ConfigKey<T>) -> Option<T> {
        let mut slot = Slot([0; SLOT_BYTES]);
        let name = key.name();
        // SAFETY: raw is a live config; slot is writable, aligned and larger than any value libghostty writes;
        // name is valid for name.len() bytes and need not be NUL-terminated.
        let found = unsafe {
            sys::ghostty_config_get(
                self.raw.as_ptr(),
                std::ptr::from_mut(&mut slot).cast(),
                name.as_ptr().cast(),
                name.len(),
            )
        };
        found.then(|| <T as sealed::Sealed>::decode(&slot))
    }

    /// Read a string key's current value. `None` when it is unset.
    #[must_use]
    pub fn get_string(&self, key: StringKey) -> Option<String> {
        let mut value: *const std::ffi::c_char = std::ptr::null();
        let name = key.name();
        // SAFETY: raw is a live config; every StringKey names a `?[:0]const u8` key, which libghostty writes as one
        // `const char *`, so `value` is the right size and alignment; name is valid for name.len() bytes.
        let found = unsafe {
            sys::ghostty_config_get(
                self.raw.as_ptr(),
                std::ptr::from_mut(&mut value).cast(),
                name.as_ptr().cast(),
                name.len(),
            )
        };
        if !found {
            return None;
        }
        // SAFETY: a written value is null or a NUL-terminated string owned by the config, which outlives this copy.
        unsafe { crate::action::owned(value) }
    }

    /// Take a copy of a config libghostty owns (one an action carries).
    ///
    /// # Safety
    ///
    /// `raw` is null or a live config for the duration of the call.
    pub(crate) unsafe fn clone_raw(raw: sys::ghostty_config_t) -> Option<Self> {
        if raw.is_null() {
            return None;
        }
        // SAFETY: a live config per the caller's contract.
        let copy = unsafe { sys::ghostty_config_clone(raw) };
        NonNull::new(copy).map(|raw| Self {
            raw,
            load_errors: Vec::new(),
        })
    }

    /// Every problem met so far: files that could not be read, then libghostty's own diagnostics.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        // SAFETY: raw is a live config owned by self.
        let count = unsafe { sys::ghostty_config_diagnostics_count(self.raw.as_ptr()) };
        let ghostty = (0..count).map(|index| {
            // SAFETY: raw is a live config and index is below its diagnostics count.
            let diagnostic = unsafe { sys::ghostty_config_get_diagnostic(self.raw.as_ptr(), index) };
            let message = if diagnostic.message.is_null() {
                String::new()
            } else {
                // SAFETY: a non-null message is a NUL-terminated string owned by the config, alive until the
                // config changes or is freed, and copied out here before either can happen.
                unsafe { CStr::from_ptr(diagnostic.message) }
                    .to_string_lossy()
                    .into_owned()
            };
            Diagnostic { message }
        });
        self.load_errors.iter().cloned().chain(ghostty).collect()
    }

    /// An independent copy, diagnostics included.
    ///
    /// # Errors
    ///
    /// [`Error::Null`] when libghostty cannot allocate the copy.
    pub fn try_clone(&self) -> Result<Self, Error> {
        // SAFETY: raw is a live config owned by self.
        let raw = unsafe { sys::ghostty_config_clone(self.raw.as_ptr()) };
        let raw = NonNull::new(raw).ok_or(Error::Null("ghostty_config_clone"))?;
        Ok(Self {
            raw,
            load_errors: self.load_errors.clone(),
        })
    }

    fn note(&mut self, message: String) {
        self.load_errors.push(Diagnostic { message });
    }
}

impl Drop for Config {
    fn drop(&mut self) {
        // SAFETY: raw was returned by ghostty_config_new or ghostty_config_clone, is owned by self alone and is
        // freed exactly once, here.
        unsafe { sys::ghostty_config_free(self.raw.as_ptr()) };
    }
}

/// The absolute form of `path` when it names a readable regular file. libghostty asserts on a relative path (UB in
/// its `ReleaseFast` build, so nothing traps) and skips anything but a regular file with only a log line.
fn loadable_path(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    // the type is checked before the open, which would block on a FIFO without a writer; the open then only proves
    // the file is readable
    if !std::fs::metadata(&absolute)?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    std::fs::File::open(&absolute)?;
    Ok(absolute)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::loadable_path;

    #[test]
    fn a_relative_path_is_made_absolute() -> std::io::Result<()> {
        // unit tests run with the package root as the working directory
        let relative = Path::new("tests/relative.conf");
        let loadable = loadable_path(relative)?;
        assert!(loadable.is_absolute(), "{}", loadable.display());
        assert_eq!(loadable, std::env::current_dir()?.join(relative));
        Ok(())
    }
}
