//! The pane core of the spike: a libghostty-vt `Terminal` owned by its own thread (the crate's types are `!Send`, so
//! the plan's "IO thread calls vt_write under the pane's mutex" cannot hold one), a PTY reader thread feeding it, and
//! frames in our own types handed to the UI thread. libghostty-vt types never leave this module.

use std::cell::{Cell as StdCell, RefCell};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result};
use libghostty_vt::kitty::graphics::{self, ImageFormat, Layer, PlacementIterator};
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{RgbColor, Underline};
use libghostty_vt::terminal::{
    ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, PrimaryDeviceAttributes, ScrollViewport,
    SecondaryDeviceAttributes, SizeReportSize,
};
use libghostty_vt::{RenderState, Terminal, TerminalOptions, key, mouse};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl From<RgbColor> for Rgb {
    fn from(c: RgbColor) -> Self {
        Self(c.r, c.g, c.b)
    }
}

pub mod flags {
    pub const BOLD: u16 = 1;
    pub const ITALIC: u16 = 2;
    pub const FAINT: u16 = 4;
    pub const UNDERLINE: u16 = 8;
    pub const STRIKE: u16 = 16;
    pub const INVISIBLE: u16 = 32;
    pub const WIDE: u16 = 64;
    pub const SPACER: u16 = 128;
    pub const UNDERCURL: u16 = 256;
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Cell {
    pub text: String,
    pub fg: Rgb,
    pub bg: Option<Rgb>,
    pub flags: u16,
}

#[derive(Clone, Debug, Default)]
pub struct Row {
    pub cells: Vec<Cell>,
}

impl Row {
    pub fn text(&self) -> String {
        let mut out = String::new();
        for cell in &self.cells {
            if cell.flags & flags::SPACER != 0 {
                continue;
            }
            if cell.text.is_empty() {
                out.push(' ');
            } else {
                out.push_str(&cell.text);
            }
        }
        out.trim_end().to_owned()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
    Hollow,
}

#[derive(Clone, Copy, Debug)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub shape: CursorShape,
    pub color: Option<Rgb>,
}

/// RGBA pixels of one kitty image, shared between frames until its generation changes.
#[derive(Debug)]
pub struct ImageData {
    pub id: u32,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Placement {
    pub image: Arc<ImageData>,
    pub col: i32,
    pub row: i32,
    pub cols: u32,
    pub rows: u32,
    pub x_offset: u32,
    pub y_offset: u32,
    pub source: (u32, u32, u32, u32),
    pub above_text: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub cols: u16,
    pub rows: Vec<Arc<Row>>,
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Option<Cursor>,
    pub images: Vec<Placement>,
    pub seq: u64,
    pub title: String,
}

/// What the UI thread hands the terminal thread.
pub enum Input {
    Output(Vec<u8>),
    Key(KeyInput),
    Text(String),
    Scroll(isize),
    Resize {
        cols: u16,
        rows: u16,
        cell_w: u32,
        cell_h: u32,
        width: u32,
        height: u32,
    },
    FrameWanted,
    Exited(Option<u32>),
    Shutdown,
}

#[derive(Clone, Debug)]
pub struct KeyInput {
    pub key: String,
    pub text: Option<String>,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub platform: bool,
}

/// State the two threads share: the latest frame and whether the UI wants a new one.
#[derive(Default)]
pub struct Shared {
    pub frame: Arc<Frame>,
    pub wanted: bool,
    pub exited: Option<Option<u32>>,
    pub bytes: u64,
    pub vt_nanos: u64,
    pub frames_built: u64,
    pub build_nanos: u64,
}

pub struct Pane {
    pub tx: Sender<Input>,
    pub shared: Arc<Mutex<Shared>>,
}

pub struct Spawn {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

impl Pane {
    /// Start the PTY child, its reader thread and the terminal thread. `wake` is called (from the terminal thread)
    /// whenever a new frame is published or the child exits.
    pub fn spawn(spawn: Spawn, wake: impl Fn() + Send + 'static) -> Result<Self> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: spawn.rows,
                cols: spawn.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;
        let mut command = CommandBuilder::new(&spawn.argv[0]);
        command.args(&spawn.argv[1..]);
        for (key, value) in &spawn.env {
            command.env(key, value);
        }
        if let Ok(dir) = std::env::current_dir() {
            command.cwd(dir);
        }
        let mut child = pair.slave.spawn_command(command).context("spawn")?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().context("reader")?;
        let writer = pair.master.take_writer().context("writer")?;
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared {
            wanted: true,
            ..Shared::default()
        }));

        let out_tx = tx.clone();
        thread::Builder::new()
            .name("pty-read".into())
            .spawn(move || read_loop(reader, out_tx))?;
        let exit_tx = tx.clone();
        thread::Builder::new().name("pty-wait".into()).spawn(move || {
            let status = child.wait().ok().map(|status| status.exit_code());
            let _ = exit_tx.send(Input::Exited(status));
        })?;
        let thread_shared = shared.clone();
        let (cols, rows) = (spawn.cols, spawn.rows);
        thread::Builder::new().name("terminal".into()).spawn(move || {
            if let Err(error) = terminal_loop(cols, rows, pair.master, writer, rx, thread_shared, wake) {
                eprintln!("terminal thread: {error:#}");
            }
        })?;
        Ok(Self { tx, shared })
    }

    pub fn send(&self, input: Input) {
        let _ = self.tx.send(input);
    }
}

fn read_loop(mut reader: Box<dyn Read + Send>, tx: Sender<Input>) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if tx.send(Input::Output(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
        }
    }
}

struct Renderer {
    state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    placements: PlacementIterator<'static>,
    images: HashMap<u32, Arc<ImageData>>,
    seq: u64,
}

fn terminal_loop(
    cols: u16,
    rows: u16,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    rx: Receiver<Input>,
    shared: Arc<Mutex<Shared>>,
    wake: impl Fn(),
) -> Result<()> {
    // The PNG decoder is thread-local in libghostty-vt: it must be installed on the terminal's own thread.
    graphics::set_png_decoder(Some(Box::new(PngDecoder)))?;
    let trace = std::env::var_os("SPIKE_TRACE").is_some();
    let writer = Rc::new(RefCell::new(writer));
    let size = Rc::new(StdCell::new(SizeReportSize {
        rows,
        columns: cols,
        cell_width: 8,
        cell_height: 16,
    }));
    let title_changed = Rc::new(StdCell::new(false));

    let mut terminal: Terminal<'static, 'static> = Terminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback: 10_000,
    })?;
    terminal.set_kitty_image_storage_limit(64 * 1024 * 1024)?;
    {
        let writer = writer.clone();
        terminal.on_pty_write(move |_, data| {
            if trace {
                eprintln!("vt reply< {}", data.escape_ascii());
            }
            let mut writer = writer.borrow_mut();
            let _ = writer.write_all(data);
            let _ = writer.flush();
        })?;
    }
    {
        let size = size.clone();
        terminal.on_size(move |_| Some(size.get()))?;
    }
    {
        let title_changed = title_changed.clone();
        terminal.on_title_changed(move |_| title_changed.set(true))?;
    }
    terminal.on_xtversion(|_| Some("agterm-gpui-spike"))?;
    terminal.on_device_attributes(|_| {
        Some(DeviceAttributes {
            primary: PrimaryDeviceAttributes::new(
                ConformanceLevel::VT220,
                &[
                    DeviceAttributeFeature::COLUMNS_132,
                    DeviceAttributeFeature::SELECTIVE_ERASE,
                    DeviceAttributeFeature::ANSI_COLOR,
                ],
            ),
            secondary: SecondaryDeviceAttributes {
                device_type: DeviceType::VT220,
                firmware_version: 1,
                rom_cartridge: 0,
            },
            tertiary: Default::default(),
        })
    })?;

    let mut key_encoder = key::Encoder::new()?;
    let mut key_event = key::Event::new()?;
    let mut mouse_encoder = mouse::Encoder::new()?;
    let mut mouse_event = mouse::Event::new()?;
    let mut renderer = Renderer {
        state: RenderState::new()?,
        rows: RowIterator::new()?,
        cells: CellIterator::new()?,
        placements: PlacementIterator::new()?,
        images: HashMap::new(),
        seq: 0,
    };
    let mut dirty = true;
    let mut title = String::new();
    let mut pixel = (cols as u32 * 8, rows as u32 * 16);
    let mut pty_size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };
    // `zmx attach` paints the session only after a SIGWINCH it receives once attached, and a resize that lands
    // before its handshake is lost; the PTY owner sees no handshake. So wiggle the size when the client first
    // speaks and again on a short schedule, until the session sends more than its greeting.
    let mut outputs = 0u32;
    let mut output_bytes = 0usize;
    let mut nudges: Vec<Instant> = Vec::new();
    let wiggle = |size: PtySize| {
        let mut smaller = size;
        smaller.rows = smaller.rows.saturating_sub(1).max(1);
        let _ = master.resize(smaller);
        // the client must observe the smaller size: both resizes inside one SIGWINCH look like no change
        thread::sleep(std::time::Duration::from_millis(50));
        let _ = master.resize(size);
        if trace {
            eprintln!("wiggled {}x{}", size.cols, size.rows);
        }
    };

    loop {
        let first = match nudges.first() {
            Some(due) => match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                Ok(input) => Some(input),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            },
            None => match rx.recv() {
                Ok(input) => Some(input),
                Err(_) => return Ok(()),
            },
        };
        if nudges.first().is_some_and(|due| *due <= Instant::now()) {
            nudges.remove(0);
            // the greeting (`session "…" created` and a clear) is about 32 bytes; a prompt makes it more
            if output_bytes < 64 {
                wiggle(pty_size);
            } else {
                nudges.clear();
            }
        }
        let Some(first) = first else { continue };
        let mut next = Some(first);
        let mut wanted = false;
        // Drain everything queued before publishing: a frame per batch, not per read.
        while let Some(input) = next.take() {
            match input {
                Input::Output(bytes) => {
                    outputs += 1;
                    output_bytes += bytes.len();
                    if outputs == 1 {
                        wiggle(pty_size);
                        let now = Instant::now();
                        nudges = [100, 300, 800, 1500]
                            .map(|ms| now + std::time::Duration::from_millis(ms))
                            .to_vec();
                    }
                    if trace {
                        eprintln!("pty> {}", bytes.escape_ascii());
                    }
                    let start = Instant::now();
                    terminal.vt_write(&bytes);
                    let mut shared = shared.lock().unwrap();
                    shared.bytes += bytes.len() as u64;
                    shared.vt_nanos += start.elapsed().as_nanos() as u64;
                    dirty = true;
                }
                Input::Key(input) => {
                    let mut out = Vec::new();
                    encode_key(&terminal, &mut key_encoder, &mut key_event, &input, &mut out)?;
                    if !out.is_empty() {
                        terminal.scroll_viewport(ScrollViewport::Bottom);
                        let mut writer = writer.borrow_mut();
                        writer.write_all(&out)?;
                        writer.flush()?;
                        dirty = true;
                    }
                }
                Input::Text(text) => {
                    terminal.scroll_viewport(ScrollViewport::Bottom);
                    let mut writer = writer.borrow_mut();
                    writer.write_all(text.as_bytes())?;
                    writer.flush()?;
                    dirty = true;
                }
                Input::Scroll(lines) => {
                    if terminal.is_mouse_tracking()? {
                        let mut out = Vec::new();
                        let (w, h) = pixel;
                        let s = size.get();
                        mouse_encoder
                            .set_options_from_terminal(&terminal)
                            .set_size(mouse::EncoderSize {
                                screen_width: w,
                                screen_height: h,
                                cell_width: s.cell_width.max(1),
                                cell_height: s.cell_height.max(1),
                                padding_top: 0,
                                padding_bottom: 0,
                                padding_left: 0,
                                padding_right: 0,
                            });
                        let button = if lines > 0 {
                            mouse::Button::Four
                        } else {
                            mouse::Button::Five
                        };
                        for _ in 0..lines.unsigned_abs() {
                            mouse_event
                                .set_position(mouse::Position {
                                    x: w as f32 / 2.0,
                                    y: h as f32 / 2.0,
                                })
                                .set_button(Some(button))
                                .set_action(mouse::Action::Press);
                            mouse_encoder.encode_to_vec(&mouse_event, &mut out)?;
                        }
                        writer.borrow_mut().write_all(&out)?;
                    } else {
                        // Positive lines scroll back into history (up), as a wheel turned away from the user.
                        terminal.scroll_viewport(ScrollViewport::Delta(-lines));
                        dirty = true;
                    }
                }
                Input::Resize {
                    cols,
                    rows,
                    cell_w,
                    cell_h,
                    width,
                    height,
                } => {
                    terminal.resize(cols, rows, cell_w, cell_h)?;
                    size.set(SizeReportSize {
                        rows,
                        columns: cols,
                        cell_width: cell_w,
                        cell_height: cell_h,
                    });
                    pixel = (width, height);
                    pty_size = PtySize {
                        rows,
                        cols,
                        pixel_width: width.min(u16::MAX as u32) as u16,
                        pixel_height: height.min(u16::MAX as u32) as u16,
                    };
                    let _ = master.resize(pty_size);
                    dirty = true;
                }
                Input::FrameWanted => wanted = true,
                Input::Exited(status) => {
                    shared.lock().unwrap().exited = Some(status);
                    wake();
                }
                Input::Shutdown => return Ok(()),
            }
            next = rx.try_recv().ok();
        }
        if title_changed.replace(false) {
            title = terminal.title().unwrap_or_default().to_owned();
            dirty = true;
        }
        let publish = {
            let mut shared = shared.lock().unwrap();
            if wanted {
                shared.wanted = true;
            }
            dirty && shared.wanted
        };
        if publish {
            let start = Instant::now();
            let previous = shared.lock().unwrap().frame.clone();
            let frame = renderer.build(&terminal, &previous, &title)?;
            {
                let mut shared = shared.lock().unwrap();
                shared.build_nanos += start.elapsed().as_nanos() as u64;
                shared.frame = Arc::new(frame);
                shared.wanted = false;
                shared.frames_built += 1;
            }
            dirty = false;
            wake();
        }
    }
}

impl Renderer {
    fn build(&mut self, terminal: &Terminal<'static, 'static>, previous: &Frame, title: &str) -> Result<Frame> {
        let snapshot = self.state.update(terminal)?;
        let full = snapshot.dirty()? == Dirty::Full;
        let colors = snapshot.colors()?;
        let (fg, bg) = (Rgb::from(colors.foreground), Rgb::from(colors.background));
        let cols = snapshot.cols()?;
        let cursor = if snapshot.cursor_visible()? {
            snapshot.cursor_viewport()?.map(|vp| Cursor {
                x: vp.x,
                y: vp.y,
                shape: match snapshot.cursor_visual_style() {
                    Ok(CursorVisualStyle::Bar) => CursorShape::Bar,
                    Ok(CursorVisualStyle::Underline) => CursorShape::Underline,
                    Ok(CursorVisualStyle::BlockHollow) => CursorShape::Hollow,
                    _ => CursorShape::Block,
                },
                color: colors.cursor.map(Rgb::from),
            })
        } else {
            None
        };
        let mut rows = Vec::with_capacity(previous.rows.len());
        let mut iteration = self.rows.update(&snapshot)?;
        let mut y = 0usize;
        let mut text = String::new();
        while let Some(row) = iteration.next() {
            let reuse = !full && !row.dirty()? && previous.rows.len() > y && previous.cols == cols;
            if reuse {
                rows.push(previous.rows[y].clone());
            } else {
                let selection = row.selection()?.map(|s| (s.start_x, s.end_x));
                let mut cells = Vec::with_capacity(cols as usize);
                let mut iter = self.cells.update(row)?;
                let mut x: u16 = 0;
                while let Some(cell) = iter.next() {
                    text.clear();
                    if cell.graphemes_len()? > 0 {
                        cell.graphemes_utf8(&mut text)?;
                    }
                    let mut cell_fg = cell.fg_color()?.map(Rgb::from).unwrap_or(fg);
                    let mut cell_bg = cell.bg_color()?.map(Rgb::from);
                    let mut f = 0u16;
                    match cell.raw_cell()?.wide()? {
                        CellWide::Wide => f |= flags::WIDE,
                        CellWide::SpacerTail | CellWide::SpacerHead => f |= flags::SPACER,
                        _ => {}
                    }
                    if cell.has_styling()? {
                        let style = cell.style()?;
                        if style.inverse {
                            let old_bg = cell_bg.unwrap_or(bg);
                            cell_bg = Some(cell_fg);
                            cell_fg = old_bg;
                        }
                        if style.bold {
                            f |= flags::BOLD;
                        }
                        if style.italic {
                            f |= flags::ITALIC;
                        }
                        if style.faint {
                            f |= flags::FAINT;
                        }
                        if style.strikethrough {
                            f |= flags::STRIKE;
                        }
                        if style.invisible {
                            f |= flags::INVISIBLE;
                        }
                        match style.underline {
                            Underline::None => {}
                            Underline::Curly => f |= flags::UNDERCURL,
                            _ => f |= flags::UNDERLINE,
                        }
                    }
                    if selection.is_some_and(|(a, b)| x >= a && x <= b) {
                        let old_bg = cell_bg.unwrap_or(bg);
                        cell_bg = Some(cell_fg);
                        cell_fg = old_bg;
                    }
                    cells.push(Cell {
                        text: text.clone(),
                        fg: cell_fg,
                        bg: cell_bg,
                        flags: f,
                    });
                    x += 1;
                }
                rows.push(Arc::new(Row { cells }));
            }
            row.set_dirty(false)?;
            y += 1;
        }
        snapshot.set_dirty(Dirty::Clean)?;
        drop(iteration);
        drop(snapshot);
        let images = self.placements(terminal)?;
        self.seq += 1;
        Ok(Frame {
            cols,
            rows,
            fg,
            bg,
            cursor,
            images,
            seq: self.seq,
            title: title.to_owned(),
        })
    }

    fn placements(&mut self, terminal: &Terminal<'static, 'static>) -> Result<Vec<Placement>> {
        let Ok(storage) = terminal.kitty_graphics() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut iteration = self.placements.update(&storage)?;
        iteration.set_layer(Layer::All)?;
        while let Some(placement) = iteration.next() {
            let id = placement.image_id()?;
            let Some(image) = storage.image(id) else { continue };
            let info = placement.placement_render_info(&image, terminal)?;
            if !info.viewport_visible || info.grid_cols == 0 || info.grid_rows == 0 {
                continue;
            }
            if image.format()? != ImageFormat::Rgba {
                continue;
            }
            let generation = image.generation()?;
            let data = match self.images.get(&id) {
                Some(data) if data.generation == generation => data.clone(),
                _ => {
                    let data = Arc::new(ImageData {
                        id,
                        generation,
                        width: image.width()?,
                        height: image.height()?,
                        rgba: image.data()?.to_vec(),
                    });
                    self.images.insert(id, data.clone());
                    data
                }
            };
            out.push(Placement {
                image: data,
                col: info.viewport_col,
                row: info.viewport_row,
                cols: info.grid_cols,
                rows: info.grid_rows,
                x_offset: placement.x_offset()?,
                y_offset: placement.y_offset()?,
                source: (info.source_x, info.source_y, info.source_width, info.source_height),
                above_text: placement.z()? >= 0,
            });
        }
        Ok(out)
    }
}

fn encode_key(
    terminal: &Terminal<'static, 'static>,
    encoder: &mut key::Encoder<'static>,
    event: &mut key::Event<'static>,
    input: &KeyInput,
    out: &mut Vec<u8>,
) -> Result<()> {
    let (code, unshifted) = key_code(&input.key);
    let mut mods = key::Mods::empty();
    if input.shift {
        mods |= key::Mods::SHIFT;
    }
    if input.ctrl {
        mods |= key::Mods::CTRL;
    }
    if input.alt {
        mods |= key::Mods::ALT;
    }
    if input.platform {
        mods |= key::Mods::SUPER;
    }
    let mut consumed = key::Mods::empty();
    if input.text.is_some() && input.shift {
        consumed |= key::Mods::SHIFT;
    }
    event
        .set_action(key::Action::Press)
        .set_key(code)
        .set_mods(mods)
        .set_consumed_mods(consumed)
        .set_unshifted_codepoint(unshifted)
        .set_utf8(input.text.clone());
    encoder.set_options_from_terminal(terminal).encode_to_vec(event, out)?;
    if out.is_empty()
        && let Some(text) = &input.text
    {
        out.extend_from_slice(text.as_bytes());
    }
    Ok(())
}

/// GPUI's key names (`Keystroke::key`) to libghostty-vt's physical key and its unshifted codepoint.
fn key_code(name: &str) -> (key::Key, char) {
    use key::Key as K;
    let named = match name {
        "enter" => K::Enter,
        "escape" => K::Escape,
        "backspace" => K::Backspace,
        "tab" => K::Tab,
        "space" => return (K::Space, ' '),
        "delete" => K::Delete,
        "insert" => K::Insert,
        "home" => K::Home,
        "end" => K::End,
        "pageup" => K::PageUp,
        "pagedown" => K::PageDown,
        "up" => K::ArrowUp,
        "down" => K::ArrowDown,
        "left" => K::ArrowLeft,
        "right" => K::ArrowRight,
        "f1" => K::F1,
        "f2" => K::F2,
        "f3" => K::F3,
        "f4" => K::F4,
        "f5" => K::F5,
        "f6" => K::F6,
        "f7" => K::F7,
        "f8" => K::F8,
        "f9" => K::F9,
        "f10" => K::F10,
        "f11" => K::F11,
        "f12" => K::F12,
        _ => K::Unidentified,
    };
    if named != K::Unidentified {
        return (named, '\0');
    }
    let mut chars = name.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return (K::Unidentified, '\0');
    };
    let c = c.to_ascii_lowercase();
    let code = match c {
        'a'..='z' => {
            const LETTERS: [key::Key; 26] = [
                K::A,
                K::B,
                K::C,
                K::D,
                K::E,
                K::F,
                K::G,
                K::H,
                K::I,
                K::J,
                K::K,
                K::L,
                K::M,
                K::N,
                K::O,
                K::P,
                K::Q,
                K::R,
                K::S,
                K::T,
                K::U,
                K::V,
                K::W,
                K::X,
                K::Y,
                K::Z,
            ];
            LETTERS[(c as u8 - b'a') as usize]
        }
        '0'..='9' => {
            const DIGITS: [key::Key; 10] = [
                K::Digit0,
                K::Digit1,
                K::Digit2,
                K::Digit3,
                K::Digit4,
                K::Digit5,
                K::Digit6,
                K::Digit7,
                K::Digit8,
                K::Digit9,
            ];
            DIGITS[(c as u8 - b'0') as usize]
        }
        '-' => K::Minus,
        '=' => K::Equal,
        '[' => K::BracketLeft,
        ']' => K::BracketRight,
        '\\' => K::Backslash,
        ';' => K::Semicolon,
        '\'' => K::Quote,
        ',' => K::Comma,
        '.' => K::Period,
        '/' => K::Slash,
        '`' => K::Backquote,
        _ => K::Unidentified,
    };
    (code, c)
}

/// RGBA8 PNG decoding for kitty graphics. libghostty-vt 0.2.2's `RustPngDecoder` has no public constructor (its doc
/// example calls a `new()` that does not exist) and decodes into an empty buffer, so the spike brings its own.
struct PngDecoder;

impl graphics::DecodePng for PngDecoder {
    fn decode_png<'alloc>(
        &mut self,
        alloc: &'alloc libghostty_vt::alloc::Allocator<'_>,
        data: &[u8],
    ) -> Option<graphics::DecodedImage<'alloc>> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
        decoder.set_transformations(png::Transformations::ALPHA | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().ok()?;
        let mut buf = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut buf).ok()?;
        let rgba: Vec<u8> = match info.color_type {
            png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
            png::ColorType::GrayscaleAlpha => buf[..info.buffer_size()]
                .chunks(2)
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
            _ => return None,
        };
        let mut bytes = libghostty_vt::alloc::Bytes::new_with_alloc(alloc, rgba.len()).ok()?;
        bytes.copy_from_slice(&rgba);
        Some(graphics::DecodedImage {
            width: info.width,
            height: info.height,
            data: bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_rows_follow_vt_output() {
        let mut terminal: Terminal<'static, 'static> = Terminal::new(TerminalOptions {
            cols: 20,
            rows: 4,
            max_scrollback: 100,
        })
        .unwrap();
        terminal.vt_write(b"hello\r\n\x1b[1;31mred\x1b[0m");
        let mut renderer = Renderer {
            state: RenderState::new().unwrap(),
            rows: RowIterator::new().unwrap(),
            cells: CellIterator::new().unwrap(),
            placements: PlacementIterator::new().unwrap(),
            images: HashMap::new(),
            seq: 0,
        };
        let frame = renderer.build(&terminal, &Frame::default(), "").unwrap();
        assert_eq!(frame.rows.len(), 4);
        assert_eq!(frame.rows[0].text(), "hello");
        assert_eq!(frame.rows[1].text(), "red");
        assert_ne!(frame.rows[1].cells[0].fg, frame.fg);
        assert_eq!(frame.rows[1].cells[0].flags & flags::BOLD, flags::BOLD);
        let cursor = frame.cursor.unwrap();
        assert_eq!((cursor.x, cursor.y), (3, 1));
        // An unchanged row is shared with the previous frame.
        terminal.vt_write(b"\r\n!");
        let next = renderer.build(&terminal, &frame, "").unwrap();
        assert!(Arc::ptr_eq(&frame.rows[0], &next.rows[0]));
        assert_eq!(next.rows[2].text(), "!");
    }
}
