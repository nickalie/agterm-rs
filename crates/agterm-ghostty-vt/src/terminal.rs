//! [`Terminal`]: an owned `GhosttyTerminal`.

use crate::effects::{CALLBACKS, EffectsSlot};
use crate::snapshot::PendingHistory;
use crate::{
    CursorStyle, Effects, Error, Handle, Mode, MouseShape, Rgb, ScrollbackLimits, TerminalColors, ffi, string_bytes,
};

/// A viewport scroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scroll {
    /// To the top of the scrollback.
    Top,
    /// To the active area.
    Bottom,
    /// By rows; negative is up.
    Rows(isize),
    /// By shell prompts (needs the shell's semantic prompt marks); negative is back.
    Prompts(isize),
    /// To the absolute row (0 is the top of the scrollback), which becomes the first visible row; clamped to the
    /// active area.
    Row(usize),
}

/// One VT: parser, screens and scrollback, owned and freed on drop.
///
/// Mutating calls take `&mut self`, which serializes them as the C API asks; the handle is `Send` and not `Sync`
/// (crate docs). Installed [`Effects`] run inside those calls.
#[derive(Debug)]
pub struct Terminal {
    // Fields drop in declaration order: a snapshot decoder still restoring history (it borrows the terminal) goes
    // first, then the terminal (which can call the effects), then the effects.
    pub(crate) history: Option<PendingHistory>,
    raw: Handle<ffi::GhosttyTerminalImpl>,
    effects: Option<EffectsSlot>,
}

// SAFETY: libghostty-vt creates no threads and keeps no thread-local state (`grep threadlocal src/terminal` is empty
// at LIBGHOSTTY_VT_REV; its process-global sys hooks are plain variables), and a terminal may be used from any thread
// as long as calls are serialized (`render.h`, `terminal.h`); the same holds for a snapshot decoder, which is reached
// only through the terminal it restores. Moving the owned handles to another thread is therefore sound. It is
// deliberately not `Sync`: `&Terminal` reads must not race the `&mut` calls of another thread.
unsafe impl Send for Terminal {}

impl Terminal {
    /// A terminal of `cols` x `rows` cells with the library's defaults (scrollback limits and the rest).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a zero dimension, [`Error::OutOfMemory`] when the allocation fails.
    pub fn new(cols: u16, rows: u16) -> Result<Self, Error> {
        // the process-global hooks must be final before any terminal can read them (`sys` docs)
        crate::sys::seal();
        let mut raw: ffi::GhosttyTerminal = std::ptr::null_mut();
        // SAFETY: a null allocator selects the library's default one; `raw` is a valid out-pointer that the call
        // fills on success only.
        let result = unsafe { ffi::ghostty_terminal_new(std::ptr::null(), &raw mut raw, cols, rows) };
        Error::check(result)?;
        Ok(Self {
            history: None,
            raw: Handle::new(raw, ffi::ghostty_terminal_free)?,
            effects: None,
        })
    }

    /// Install `effects`: every callback of [`Effects`] is registered, replacing any earlier handler (dropped after
    /// the switch).
    ///
    /// # Errors
    ///
    /// The library's result when an option is refused (a library without one of the bound options).
    pub fn set_effects(&mut self, effects: Box<dyn Effects>) -> Result<(), Error> {
        let slot = EffectsSlot::new(effects);
        // SAFETY: `self.raw` is live and `&mut self` serializes the call; the userdata is the slot's heap pointer,
        // which stays valid until the slot is replaced or dropped after the terminal (field order).
        let result =
            unsafe { ffi::ghostty_terminal_set(self.raw.ptr(), ffi::GHOSTTY_TERMINAL_OPT_USERDATA, slot.userdata()) };
        Error::check(result)?;
        let previous = self.effects.replace(slot);
        for &(option, callback) in CALLBACKS {
            // SAFETY: each option takes its callback's function pointer itself, and `CALLBACKS` pairs every option
            // with a trampoline of exactly that option's C signature.
            Error::check(unsafe { ffi::ghostty_terminal_set(self.raw.ptr(), option, callback) })?;
        }
        drop(previous);
        Ok(())
    }

    /// Set the scrollback limits; lowering one prunes history right away.
    ///
    /// # Errors
    ///
    /// The library's result when a value is refused.
    pub fn set_scrollback_limits(&mut self, limits: ScrollbackLimits) -> Result<(), Error> {
        for (option, limit) in [
            (ffi::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, limits.max_bytes),
            (ffi::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES, limits.max_lines),
        ] {
            let value = limit
                .as_ref()
                .map_or(std::ptr::null(), |limit| std::ptr::from_ref(limit).cast());
            // SAFETY: `self.raw` is live; both options take a `size_t *`, or null to remove the limit.
            Error::check(unsafe { ffi::ghostty_terminal_set(self.raw.ptr(), option, value) })?;
        }
        Ok(())
    }

    /// Set the Kitty image storage limit in bytes on every screen; zero disables the Kitty graphics protocol and
    /// deletes every image.
    ///
    /// # Errors
    ///
    /// The library's result when the value is refused.
    pub fn set_kitty_image_storage_limit(&mut self, bytes: u64) -> Result<(), Error> {
        // SAFETY: `self.raw` is live; the option takes a `uint64_t *` read during the call.
        let result = unsafe {
            ffi::ghostty_terminal_set(
                self.raw.ptr(),
                ffi::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT,
                (&raw const bytes).cast(),
            )
        };
        Error::check(result)
    }

    /// The current scrollback limits.
    #[must_use]
    pub fn scrollback_limits(&self) -> ScrollbackLimits {
        let limit = |data| {
            let mut value: usize = 0;
            // SAFETY: `self.raw` is live; both keys write a `size_t`, and fail with `NO_VALUE` when unlimited.
            let result = unsafe { ffi::ghostty_terminal_get(self.raw.ptr(), data, (&raw mut value).cast()) };
            (result == ffi::GHOSTTY_SUCCESS).then_some(value)
        };
        ScrollbackLimits {
            max_bytes: limit(ffi::GHOSTTY_TERMINAL_DATA_SCROLLBACK_MAX_BYTES),
            max_lines: limit(ffi::GHOSTTY_TERMINAL_DATA_SCROLLBACK_MAX_LINES),
        }
    }

    /// Set or reset a mode directly (no render-hold callback runs for [`Mode::SYNC_OUTPUT`]).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a mode the library does not know.
    pub fn set_mode(&mut self, mode: Mode, value: bool) -> Result<(), Error> {
        let config = ffi::GhosttyTerminalModeConfig { mode: mode.0, value };
        // SAFETY: `self.raw` is live; `OPT_MODE` takes a `GhosttyTerminalModeConfig *` read during the call.
        let result = unsafe {
            ffi::ghostty_terminal_set(
                self.raw.ptr(),
                ffi::GHOSTTY_TERMINAL_OPT_MODE,
                (&raw const config).cast(),
            )
        };
        Error::check(result)
    }

    /// Set a mode's reset default: the current value and the one a reset (RIS; DECSTR for its subset) restores, as
    /// Ghostty's termio sets its `default_modes` from the config.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a mode the library does not know or whose default cannot be configured (one that
    /// mirrors other terminal state, as the alternate screen).
    pub fn set_mode_default(&mut self, mode: Mode, value: bool) -> Result<(), Error> {
        let config = ffi::GhosttyTerminalModeConfig { mode: mode.0, value };
        // SAFETY: `self.raw` is live; `OPT_MODE_DEFAULT` takes a `GhosttyTerminalModeConfig *` read during the call.
        let result = unsafe {
            ffi::ghostty_terminal_set(
                self.raw.ptr(),
                ffi::GHOSTTY_TERMINAL_OPT_MODE_DEFAULT,
                (&raw const config).cast(),
            )
        };
        Error::check(result)
    }

    /// Whether a mode is set (`false` for a mode the library does not know).
    #[must_use]
    pub fn mode(&self, mode: Mode) -> bool {
        let mut config = ffi::GhosttyTerminalModeConfig {
            mode: mode.0,
            value: false,
        };
        // SAFETY: `self.raw` is live; `DATA_MODE` reads `mode` and writes `value` of the config.
        unsafe {
            ffi::ghostty_terminal_get(
                self.raw.ptr(),
                ffi::GHOSTTY_TERMINAL_DATA_MODE,
                (&raw mut config).cast(),
            )
        };
        config.value
    }

    /// The window title (OSC 0/2), empty when none was set.
    #[must_use]
    pub fn title(&self) -> String {
        self.get_string(ffi::GHOSTTY_TERMINAL_DATA_TITLE)
    }

    /// The working directory a program reported (OSC 7/9/1337), raw; empty when none.
    #[must_use]
    pub fn pwd(&self) -> String {
        self.get_string(ffi::GHOSTTY_TERMINAL_DATA_PWD)
    }

    /// The effective colors.
    #[must_use]
    pub fn colors(&self) -> TerminalColors {
        let color = |data| {
            let mut value = ffi::GhosttyColorRgb { r: 0, g: 0, b: 0 };
            // SAFETY: `self.raw` is live; the color keys write a `GhosttyColorRgb`, or fail when unset.
            let result = unsafe { ffi::ghostty_terminal_get(self.raw.ptr(), data, (&raw mut value).cast()) };
            (result == ffi::GHOSTTY_SUCCESS).then(|| Rgb::from(value))
        };
        let mut palette = [ffi::GhosttyColorRgb { r: 0, g: 0, b: 0 }; 256];
        // SAFETY: `self.raw` is live; `DATA_COLOR_PALETTE` writes 256 `GhosttyColorRgb` (it always has a value).
        unsafe {
            ffi::ghostty_terminal_get(
                self.raw.ptr(),
                ffi::GHOSTTY_TERMINAL_DATA_COLOR_PALETTE,
                palette.as_mut_ptr().cast(),
            )
        };
        TerminalColors {
            foreground: color(ffi::GHOSTTY_TERMINAL_DATA_COLOR_FOREGROUND),
            background: color(ffi::GHOSTTY_TERMINAL_DATA_COLOR_BACKGROUND),
            cursor: color(ffi::GHOSTTY_TERMINAL_DATA_COLOR_CURSOR),
            palette: Box::new(palette.map(Rgb::from)),
        }
    }

    /// The default colors, the overrides programs set (OSC 4/10/11/12) left out: the theme
    /// ([`Terminal::set_default_colors`], [`Terminal::set_default_palette`]).
    #[must_use]
    pub fn default_colors(&self) -> TerminalColors {
        let color = |data| {
            let mut value = ffi::GhosttyColorRgb { r: 0, g: 0, b: 0 };
            // SAFETY: `self.raw` is live; the default color keys write a `GhosttyColorRgb`, or fail when unset.
            let result = unsafe { ffi::ghostty_terminal_get(self.raw.ptr(), data, (&raw mut value).cast()) };
            (result == ffi::GHOSTTY_SUCCESS).then(|| Rgb::from(value))
        };
        TerminalColors {
            foreground: color(ffi::GHOSTTY_TERMINAL_DATA_COLOR_FOREGROUND_DEFAULT),
            background: color(ffi::GHOSTTY_TERMINAL_DATA_COLOR_BACKGROUND_DEFAULT),
            cursor: color(ffi::GHOSTTY_TERMINAL_DATA_COLOR_CURSOR_DEFAULT),
            palette: self.default_palette(),
        }
    }

    /// Set the default foreground, background and cursor colors (the theme; programs override them with OSC 10/11/12).
    /// `None` unsets one. A render state reports its default colors only while both foreground and background are set.
    ///
    /// # Errors
    ///
    /// The library's result when a value is refused.
    pub fn set_default_colors(
        &mut self,
        foreground: Option<Rgb>,
        background: Option<Rgb>,
        cursor: Option<Rgb>,
    ) -> Result<(), Error> {
        for (option, color) in [
            (ffi::GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND, foreground),
            (ffi::GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND, background),
            (ffi::GHOSTTY_TERMINAL_OPT_COLOR_CURSOR, cursor),
        ] {
            let color = color.map(|color| ffi::GhosttyColorRgb {
                r: color.r,
                g: color.g,
                b: color.b,
            });
            let value = color
                .as_ref()
                .map_or(std::ptr::null(), |color| std::ptr::from_ref(color).cast());
            // SAFETY: `self.raw` is live; the color options take a `GhosttyColorRgb *` read during the call, or null.
            Error::check(unsafe { ffi::ghostty_terminal_set(self.raw.ptr(), option, value) })?;
        }
        Ok(())
    }

    /// Set the default 256-color palette, keeping the slots programs overrode (OSC 4); `None` restores the library's
    /// built-in palette.
    ///
    /// # Errors
    ///
    /// The library's result when the value is refused.
    pub fn set_default_palette(&mut self, palette: Option<&[Rgb; 256]>) -> Result<(), Error> {
        let raw = palette.map(|palette| {
            palette.map(|color| ffi::GhosttyColorRgb {
                r: color.r,
                g: color.g,
                b: color.b,
            })
        });
        let value = raw.as_ref().map_or(std::ptr::null(), |raw| raw.as_ptr().cast());
        // SAFETY: `self.raw` is live; `OPT_COLOR_PALETTE` takes a pointer to 256 `GhosttyColorRgb` read during the
        // call, or null.
        let result =
            unsafe { ffi::ghostty_terminal_set(self.raw.ptr(), ffi::GHOSTTY_TERMINAL_OPT_COLOR_PALETTE, value) };
        Error::check(result)
    }

    /// Set the cursor the terminal starts with and returns to on `CSI 0 q` (Ghostty's `cursor-style` and
    /// `cursor-style-blink`, which Ghostty's termio applies to every terminal); the current cursor takes it too while
    /// no program chose one. The library's own default is a steady block.
    ///
    /// # Errors
    ///
    /// The library's result when a value is refused.
    pub fn set_default_cursor(&mut self, style: CursorStyle, blink: bool) -> Result<(), Error> {
        let style: ffi::GhosttyTerminalCursorStyle = match style {
            CursorStyle::Bar => ffi::GHOSTTY_TERMINAL_CURSOR_STYLE_BAR,
            CursorStyle::Block => ffi::GHOSTTY_TERMINAL_CURSOR_STYLE_BLOCK,
            CursorStyle::Underline => ffi::GHOSTTY_TERMINAL_CURSOR_STYLE_UNDERLINE,
            CursorStyle::BlockHollow => ffi::GHOSTTY_TERMINAL_CURSOR_STYLE_BLOCK_HOLLOW,
        };
        // SAFETY: `self.raw` is live; `OPT_DEFAULT_CURSOR_STYLE` takes a `GhosttyTerminalCursorStyle *` read during the
        // call.
        let result = unsafe {
            ffi::ghostty_terminal_set(
                self.raw.ptr(),
                ffi::GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_STYLE,
                (&raw const style).cast(),
            )
        };
        Error::check(result)?;
        // SAFETY: `self.raw` is live; `OPT_DEFAULT_CURSOR_BLINK` takes a `bool *` read during the call.
        let result = unsafe {
            ffi::ghostty_terminal_set(
                self.raw.ptr(),
                ffi::GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_BLINK,
                (&raw const blink).cast(),
            )
        };
        Error::check(result)
    }

    /// The default palette (the built-in one unless [`Terminal::set_default_palette`] replaced it), OSC 4 overrides
    /// ignored.
    #[must_use]
    pub fn default_palette(&self) -> Box<[Rgb; 256]> {
        let mut palette = [ffi::GhosttyColorRgb { r: 0, g: 0, b: 0 }; 256];
        // SAFETY: `self.raw` is live; `DATA_COLOR_PALETTE_DEFAULT` writes 256 `GhosttyColorRgb` (it always has one).
        unsafe {
            ffi::ghostty_terminal_get(
                self.raw.ptr(),
                ffi::GHOSTTY_TERMINAL_DATA_COLOR_PALETTE_DEFAULT,
                palette.as_mut_ptr().cast(),
            )
        };
        Box::new(palette.map(Rgb::from))
    }

    /// The mouse pointer shape the program asked for (OSC 22).
    #[must_use]
    pub fn mouse_shape(&self) -> MouseShape {
        let mut value: ffi::GhosttyMouseShape = ffi::GHOSTTY_MOUSE_SHAPE_DEFAULT;
        // SAFETY: `self.raw` is live; `DATA_MOUSE_SHAPE` writes a `GhosttyMouseShape`.
        unsafe {
            ffi::ghostty_terminal_get(
                self.raw.ptr(),
                ffi::GHOSTTY_TERMINAL_DATA_MOUSE_SHAPE,
                (&raw mut value).cast(),
            )
        };
        MouseShape::from_raw(value)
    }

    /// Full reset (RIS) from the embedder's side. Only the render-hold callback runs (when a hold was on): unlike a
    /// program's RIS, this reports no [`Effects::reset`], progress removal or status clear, so the embedder reports the
    /// cleared title, pwd, progress and status itself.
    pub fn reset(&mut self) {
        // SAFETY: `self.raw` is live and `&mut self` serializes the call.
        unsafe { ffi::ghostty_terminal_reset(self.raw.ptr()) };
    }

    /// A `GhosttyString` datum, copied (lossy UTF-8).
    fn get_string(&self, data: ffi::GhosttyTerminalData) -> String {
        let mut value = ffi::GhosttyString {
            ptr: std::ptr::null(),
            len: 0,
        };
        // SAFETY: `self.raw` is live; the keys passed here write a `GhosttyString`. On failure it stays empty.
        unsafe { ffi::ghostty_terminal_get(self.raw.ptr(), data, (&raw mut value).cast()) };
        // SAFETY: the bytes stay valid until the next mutating call, and `&self` excludes one during this copy.
        String::from_utf8_lossy(unsafe { string_bytes(value) }).into_owned()
    }

    /// Feed bytes from the PTY through the VT parser. Malformed input never fails: the library logs it and keeps the
    /// terminal consistent.
    pub fn vt_write(&mut self, bytes: &[u8]) {
        // SAFETY: `self.raw` is a live terminal owned by `self`, `&mut self` makes this the only call on it, and the
        // pointer and length describe the borrowed slice for the duration of the call.
        unsafe { ffi::ghostty_terminal_vt_write(self.raw.ptr(), bytes.as_ptr(), bytes.len()) };
    }

    /// Feed only the shortest prefix of `bytes` that brings the parser to ground (no sequence or UTF-8 character
    /// in progress), the point where out-of-band VT sequences can go in safely; the bytes consumed, or `None` when all
    /// of them were and the parser is still not at ground. Already at ground, nothing is consumed (`Some(0)`).
    pub fn vt_write_until_ground(&mut self, bytes: &[u8]) -> Option<usize> {
        let mut consumed = 0;
        // SAFETY: `self.raw` is live and `&mut self` serializes the call; the pointer and length describe the borrowed
        // slice, and `consumed` is a valid out-pointer.
        let result = unsafe {
            ffi::ghostty_terminal_vt_write_until_ground(self.raw.ptr(), bytes.as_ptr(), bytes.len(), &raw mut consumed)
        };
        (result == ffi::GHOSTTY_SUCCESS).then_some(consumed)
    }

    /// Whether the parser is at ground (see [`Terminal::vt_write_until_ground`]).
    #[must_use]
    pub fn at_ground(&self) -> bool {
        self.get_bool(ffi::GHOSTTY_TERMINAL_DATA_VT_GROUND, true)
    }

    /// Whether the cursor is at a shell prompt or its input (semantic prompt marks, OSC 133); false on the alternate
    /// screen or without marks.
    #[must_use]
    pub fn cursor_at_prompt(&self) -> bool {
        self.get_bool(ffi::GHOSTTY_TERMINAL_DATA_CURSOR_AT_PROMPT, false)
    }

    /// Resize to `cols` x `rows` cells of `cell_width_px` x `cell_height_px` pixels: the primary screen reflows,
    /// synchronized output ends, and an in-band size report goes out when mode 2048 is on.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a zero dimension, [`Error::OutOfMemory`] when reflow cannot allocate.
    pub fn resize(&mut self, cols: u16, rows: u16, cell_width_px: u32, cell_height_px: u32) -> Result<(), Error> {
        // SAFETY: `self.raw` is a live terminal owned by `self` and `&mut self` makes this the only call on it.
        let result = unsafe { ffi::ghostty_terminal_resize(self.raw.ptr(), cols, rows, cell_width_px, cell_height_px) };
        Error::check(result)
    }

    /// The width in cells.
    #[must_use]
    pub fn cols(&self) -> u16 {
        self.get_u16(ffi::GHOSTTY_TERMINAL_DATA_COLS)
    }

    /// The height in cells.
    #[must_use]
    pub fn rows(&self) -> u16 {
        self.get_u16(ffi::GHOSTTY_TERMINAL_DATA_ROWS)
    }

    /// The cursor's column and row in the active area, zero-based.
    #[must_use]
    pub fn cursor(&self) -> (u16, u16) {
        (
            self.get_u16(ffi::GHOSTTY_TERMINAL_DATA_CURSOR_X),
            self.get_u16(ffi::GHOSTTY_TERMINAL_DATA_CURSOR_Y),
        )
    }

    /// The total rows of the active screen, scrollback included.
    #[must_use]
    pub fn total_rows(&self) -> usize {
        let mut value: usize = 0;
        // SAFETY: the terminal is live; `DATA_TOTAL_ROWS` writes a `size_t`.
        unsafe {
            ffi::ghostty_terminal_get(
                self.raw.ptr(),
                ffi::GHOSTTY_TERMINAL_DATA_TOTAL_ROWS,
                (&raw mut value).cast(),
            )
        };
        value
    }

    /// Whether the viewport follows the active area (not scrolled back).
    #[must_use]
    pub fn viewport_at_bottom(&self) -> bool {
        self.get_bool(ffi::GHOSTTY_TERMINAL_DATA_VIEWPORT_ACTIVE, true)
    }

    /// Whether the alternate screen is active (a full-screen program such as `less` or `vim`).
    #[must_use]
    pub fn alternate_screen(&self) -> bool {
        let mut screen: ffi::GhosttyTerminalScreen = ffi::GHOSTTY_TERMINAL_SCREEN_PRIMARY;
        // SAFETY: the terminal is live; `DATA_ACTIVE_SCREEN` writes a `GhosttyTerminalScreen`.
        unsafe {
            ffi::ghostty_terminal_get(
                self.raw.ptr(),
                ffi::GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN,
                (&raw mut screen).cast(),
            )
        };
        screen == ffi::GHOSTTY_TERMINAL_SCREEN_ALTERNATE
    }

    /// Whether a program turned on mouse reporting (any tracking mode).
    #[must_use]
    pub fn mouse_tracking(&self) -> bool {
        self.get_bool(ffi::GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING, false)
    }

    /// Scroll the viewport.
    pub fn scroll_viewport(&mut self, scroll: Scroll) {
        let (tag, value) = match scroll {
            Scroll::Top => (
                ffi::GHOSTTY_SCROLL_VIEWPORT_TOP,
                ffi::GhosttyTerminalScrollViewportValue { delta: 0 },
            ),
            Scroll::Bottom => (
                ffi::GHOSTTY_SCROLL_VIEWPORT_BOTTOM,
                ffi::GhosttyTerminalScrollViewportValue { delta: 0 },
            ),
            Scroll::Rows(delta) => (
                ffi::GHOSTTY_SCROLL_VIEWPORT_DELTA,
                ffi::GhosttyTerminalScrollViewportValue { delta },
            ),
            Scroll::Prompts(delta) => (
                ffi::GHOSTTY_SCROLL_VIEWPORT_DELTA_PROMPT,
                ffi::GhosttyTerminalScrollViewportValue { delta_prompt: delta },
            ),
            Scroll::Row(row) => (
                ffi::GHOSTTY_SCROLL_VIEWPORT_ROW,
                ffi::GhosttyTerminalScrollViewportValue { row },
            ),
        };
        let behavior = ffi::GhosttyTerminalScrollViewport { tag, value };
        // SAFETY: the terminal is live and `&mut self` serializes the call; the tag selects the union member set.
        unsafe { ffi::ghostty_terminal_scroll_viewport(self.raw.ptr(), behavior) };
    }

    /// Allow or forbid loading Kitty images from files, from temporary files under `temp_dir` (`None` forbids) and
    /// from shared memory; Ghostty allows all three, temporary files in the system's temporary directory.
    ///
    /// # Errors
    ///
    /// The library's result when an option is refused.
    pub fn set_kitty_image_media(
        &mut self,
        file: bool,
        temp_dir: Option<&str>,
        shared_memory: bool,
    ) -> Result<(), Error> {
        for (option, value) in [
            (ffi::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_FILE, file),
            (ffi::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_SHARED_MEM, shared_memory),
        ] {
            // SAFETY: the terminal is live; both options read a `bool` during the call.
            Error::check(unsafe { ffi::ghostty_terminal_set(self.raw.ptr(), option, (&raw const value).cast()) })?;
        }
        let dir = temp_dir.map(|dir| ffi::GhosttyString {
            ptr: dir.as_ptr(),
            len: dir.len(),
        });
        let value = dir
            .as_ref()
            .map_or(std::ptr::null(), |dir| std::ptr::from_ref(dir).cast());
        // SAFETY: the terminal is live; `MEDIUM_TEMP_FILE` copies the string during the call, null disables.
        Error::check(unsafe {
            ffi::ghostty_terminal_set(
                self.raw.ptr(),
                ffi::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_TEMP_FILE,
                value,
            )
        })
    }

    /// Set the terminfo entry this terminal runs as (`TERM`, e.g. `xterm-ghostty`), the answer to an XTGETTCAP query
    /// for `TN`; `None` clears it, and a terminal without one leaves that query unanswered (the library's default).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a name longer than 128 bytes.
    pub fn set_terminfo_name(&mut self, name: Option<&str>) -> Result<(), Error> {
        let name = name.map(|name| ffi::GhosttyString {
            ptr: name.as_ptr(),
            len: name.len(),
        });
        let value = name
            .as_ref()
            .map_or(std::ptr::null(), |name| std::ptr::from_ref(name).cast());
        // SAFETY: the terminal is live and `&mut self` serializes the call; `OPT_TERMINFO_NAME` copies the string
        // during the call, and null clears it.
        Error::check(unsafe {
            ffi::ghostty_terminal_set(self.raw.ptr(), ffi::GHOSTTY_TERMINAL_OPT_TERMINFO_NAME, value)
        })
    }

    /// Own a terminal the library created (a decoded snapshot), with no effects and no pending history. The caller
    /// sealed the process-global hooks before the library created it, as [`Terminal::new`] does.
    pub(crate) fn from_raw(raw: ffi::GhosttyTerminal) -> Result<Self, Error> {
        Ok(Self {
            history: None,
            raw: Handle::new(raw, ffi::ghostty_terminal_free)?,
            effects: None,
        })
    }

    /// The raw handle, for the wrapper's other types; it stays owned by `self`.
    pub(crate) fn as_raw(&mut self) -> ffi::GhosttyTerminal {
        self.raw.ptr()
    }

    /// The raw handle for a call that only reads the terminal (`&self` keeps mutating calls out meanwhile).
    pub(crate) fn raw_shared(&self) -> ffi::GhosttyTerminal {
        self.raw.ptr()
    }

    /// A `bool` datum, or `default` when the library has none.
    fn get_bool(&self, data: ffi::GhosttyTerminalData, default: bool) -> bool {
        let mut value = default;
        // SAFETY: the terminal is live; the keys passed here write a `bool`.
        unsafe { ffi::ghostty_terminal_get(self.raw.ptr(), data, (&raw mut value).cast()) };
        value
    }

    /// A `uint16_t` datum (`terminal.h` documents the output type of each key read through here).
    fn get_u16(&self, data: ffi::GhosttyTerminalData) -> u16 {
        let mut value: u16 = 0;
        // SAFETY: `self.raw` is a live terminal owned by `self`; every key passed here has `uint16_t *` as its output
        // type, so `value` is a valid, correctly sized out-pointer. The keys are always set, so the result is ignored
        // and a failure would leave the zero in place.
        unsafe { ffi::ghostty_terminal_get(self.raw.ptr(), data, (&raw mut value).cast()) };
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_dimension_is_refused() {
        assert_eq!(Terminal::new(0, 24).err(), Some(Error::InvalidValue));
        assert_eq!(Terminal::new(80, 0).err(), Some(Error::InvalidValue));
    }

    #[test]
    fn a_new_terminal_reports_its_size_and_a_home_cursor() {
        let terminal = Terminal::new(80, 24).unwrap();
        assert_eq!((terminal.cols(), terminal.rows()), (80, 24));
        assert_eq!(terminal.cursor(), (0, 0));
    }

    #[test]
    fn the_alternate_screen_is_reported() {
        let mut terminal = Terminal::new(80, 24).unwrap();
        assert!(!terminal.alternate_screen());
        terminal.vt_write(b"\x1b[?1049h");
        assert!(terminal.alternate_screen());
        terminal.vt_write(b"\x1b[?1049l");
        assert!(!terminal.alternate_screen());
    }

    #[test]
    fn writes_move_the_cursor_and_resize_changes_the_size() {
        let mut terminal = Terminal::new(80, 24).unwrap();
        terminal.vt_write(b"abc\r\nde");
        assert_eq!(terminal.cursor(), (2, 1));
        terminal.resize(100, 30, 8, 16).unwrap();
        assert_eq!((terminal.cols(), terminal.rows()), (100, 30));
        assert_eq!(terminal.resize(0, 30, 8, 16), Err(Error::InvalidValue));
    }

    /// The family emoji: man, ZWJ, woman, ZWJ, girl; one grapheme of width 2.
    const FAMILY: &[u8] = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}".as_bytes();

    #[test]
    fn a_mode_default_holds_across_a_reset_and_a_plain_set_does_not() {
        let mut terminal = Terminal::new(80, 24).unwrap();
        assert!(
            !terminal.mode(Mode::GRAPHEME_CLUSTER),
            "the library's own default is off"
        );
        terminal.vt_write(FAMILY);
        assert_eq!(terminal.cursor(), (6, 0), "each codepoint counted: three wide emoji");

        terminal.set_mode(Mode::GRAPHEME_CLUSTER, true).unwrap();
        terminal.vt_write(b"\x1bc");
        assert!(!terminal.mode(Mode::GRAPHEME_CLUSTER), "a reset restores the default");

        terminal.set_mode_default(Mode::GRAPHEME_CLUSTER, true).unwrap();
        assert!(terminal.mode(Mode::GRAPHEME_CLUSTER), "the current value too");
        terminal.vt_write(b"\x1bc");
        assert!(
            terminal.mode(Mode::GRAPHEME_CLUSTER),
            "a reset keeps the configured default"
        );
        terminal.vt_write(FAMILY);
        assert_eq!(terminal.cursor(), (2, 0), "one cluster, two cells");
        // a program may still turn it off for itself
        terminal.vt_write(b"\x1b[?2027l");
        assert!(!terminal.mode(Mode::GRAPHEME_CLUSTER));
        // a mode mirroring other state has no configurable default
        assert_eq!(
            terminal.set_mode_default(Mode::dec(1049), true),
            Err(Error::InvalidValue)
        );
    }
}
