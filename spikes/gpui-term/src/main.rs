//! Spike: a libghostty-vt terminal drawn by GPUI (through gpui-kit), its PTY child `zmx attach` under a private
//! ZMX_DIR. Throwaway code; the report is docs/spikes/20261014-gpui-term.md.
//!
//!   gpui-term-spike [--control SOCKET] [--width W] [--height H] [--zmx PATH --session NAME] [-- ARGV...]

mod control;
mod term;

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::*;
use smallvec::SmallVec;

use crate::term::{CursorShape, Frame, Input, KeyInput, Pane, Rgb, Row, Spawn, flags};

const FONT_FAMILY: &str = "DejaVu Sans Mono";
const FONT_SIZE: f32 = 14.0;
const PADDING: f32 = 4.0;

struct Args {
    control: Option<PathBuf>,
    width: f32,
    height: f32,
    zmx: Option<String>,
    session: String,
    argv: Vec<String>,
}

fn parse_args() -> Args {
    let mut args = Args {
        control: None,
        width: 1280.0,
        height: 800.0,
        zmx: None,
        session: "gpui-spike".into(),
        argv: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--control" => args.control = it.next().map(PathBuf::from),
            "--width" => args.width = it.next().and_then(|v| v.parse().ok()).unwrap_or(args.width),
            "--height" => args.height = it.next().and_then(|v| v.parse().ok()).unwrap_or(args.height),
            "--zmx" => args.zmx = it.next(),
            "--session" => args.session = it.next().unwrap_or(args.session),
            "--" => {
                args.argv = it.collect();
                break;
            }
            other => panic!("unknown argument {other}"),
        }
    }
    if args.argv.is_empty() {
        args.argv = vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())];
    }
    args
}

#[derive(Clone, Copy, Debug)]
struct Metrics {
    cell_w: Pixels,
    cell_h: Pixels,
}

/// One painted text batch: shaped once per row content, painted at its column.
struct Batch {
    col: usize,
    line: ShapedLine,
}

struct RowLayout {
    row: Arc<Row>,
    batches: Vec<Batch>,
}

struct TermView {
    focus: FocusHandle,
    pane: Pane,
    frame: Arc<Frame>,
    metrics: Option<Metrics>,
    grid: (u16, u16),
    preedit: Option<String>,
    painted: u64,
    layouts: Vec<Option<RowLayout>>,
    images: HashMap<(u32, u64), Arc<RenderImage>>,
    started: Instant,
    first_paint_ms: Option<u128>,
}

impl TermView {
    fn font(bold: bool, italic: bool) -> Font {
        let mut f = font(FONT_FAMILY);
        if bold {
            f.weight = FontWeight::BOLD;
        }
        if italic {
            f.style = FontStyle::Italic;
        }
        f
    }

    fn metrics(&mut self, window: &Window) -> Metrics {
        if let Some(metrics) = self.metrics {
            return metrics;
        }
        let text = window.text_system();
        let id = text.resolve_font(&Self::font(false, false));
        let size = px(FONT_SIZE);
        let advance = text.advance(id, size, 'M').map(|s| s.width).unwrap_or(px(8.0));
        let height = text.ascent(id, size) + text.descent(id, size).abs();
        let metrics = Metrics {
            cell_w: advance.ceil(),
            cell_h: (height * 1.15).ceil(),
        };
        self.metrics = Some(metrics);
        metrics
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = ks.modifiers;
        // Plain printable keys with no command modifier go to the input handler path when an IME composes;
        // without composition they are encoded here like any key, so the kitty keyboard protocol sees them.
        if self.preedit.is_some() {
            return;
        }
        let text = if m.control || m.platform {
            None
        } else {
            ks.key_char.clone()
        };
        let input = KeyInput {
            key: ks.key.clone(),
            text,
            shift: m.shift,
            ctrl: m.control,
            alt: m.alt,
            platform: m.platform,
        };
        self.pane.send(Input::Key(input));
        cx.stop_propagation();
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        let lines = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / f32::from(self.metrics.map(|m| m.cell_h).unwrap_or(px(16.0))),
        };
        let lines = lines.round() as isize;
        if lines != 0 {
            self.pane.send(Input::Scroll(lines));
        }
    }

    fn dump(&self) -> serde_json::Value {
        let text: Vec<String> = self.frame.rows.iter().map(|r| r.text()).collect();
        let shared = self.pane.shared.lock().unwrap();
        let cell = self
            .metrics
            .map(|m| [f32::from(m.cell_w), f32::from(m.cell_h)])
            .unwrap_or([0.0, 0.0]);
        serde_json::json!({
            "cols": self.frame.cols,
            "rows": self.frame.rows.len(),
            "cell": cell,
            "cursor": self.frame.cursor.map(|c| [c.x, c.y]),
            "text": text.join("\n"),
            "title": self.frame.title,
            "preedit": self.preedit,
            "frames": self.painted,
            "framesBuilt": shared.frames_built,
            "images": self.frame.images.len(),
            "bytes": shared.bytes,
            "vtSeconds": shared.vt_nanos as f64 / 1e9,
            "frameBuildSeconds": shared.build_nanos as f64 / 1e9,
            "firstPaintMs": self.first_paint_ms,
            "exited": shared.exited.is_some(),
        })
    }

    fn render_image(&mut self, placement: &term::Placement) -> Option<Arc<RenderImage>> {
        let data = &placement.image;
        let key = (data.id, data.generation);
        if let Some(image) = self.images.get(&key) {
            return Some(image.clone());
        }
        // GPUI's RenderImage holds BGRA.
        let mut bgra = data.rgba.clone();
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let buffer = image::RgbaImage::from_raw(data.width, data.height, bgra)?;
        let frames: SmallVec<[image::Frame; 1]> = SmallVec::from_elem(image::Frame::new(buffer), 1);
        let image = Arc::new(RenderImage::new(frames));
        self.images.insert(key, image.clone());
        Some(image)
    }
}

fn hsla(c: Rgb, alpha: f32) -> Hsla {
    Rgba {
        r: c.0 as f32 / 255.0,
        g: c.1 as f32 / 255.0,
        b: c.2 as f32 / 255.0,
        a: alpha,
    }
    .into()
}

/// Group a row's cells into batches of one style (wide cells alone) and shape each.
fn layout_row(row: &Arc<Row>, metrics: Metrics, window: &Window) -> RowLayout {
    let mut batches = Vec::new();
    let cells = &row.cells;
    let mut i = 0;
    while i < cells.len() {
        let cell = &cells[i];
        if cell.flags & flags::SPACER != 0 || cell.flags & flags::INVISIBLE != 0 {
            i += 1;
            continue;
        }
        let start = i;
        let wide = cell.flags & flags::WIDE != 0;
        let mut text = String::new();
        let style = (cell.fg, cell.flags & !(flags::WIDE | flags::SPACER));
        loop {
            let c = &cells[i];
            text.push_str(if c.text.is_empty() { " " } else { &c.text });
            i += 1;
            if wide || i >= cells.len() {
                break;
            }
            let n = &cells[i];
            if n.flags & (flags::WIDE | flags::SPACER) != 0 || (n.fg, n.flags) != style {
                break;
            }
        }
        let decorated = style.1 & (flags::UNDERLINE | flags::UNDERCURL | flags::STRIKE) != 0;
        if text.trim().is_empty() && !decorated {
            continue;
        }
        let (fg, f) = style;
        let color = hsla(fg, if f & flags::FAINT != 0 { 0.5 } else { 1.0 });
        let run = TextRun {
            len: text.len(),
            font: TermView::font(f & flags::BOLD != 0, f & flags::ITALIC != 0),
            color,
            background_color: None,
            underline: (f & (flags::UNDERLINE | flags::UNDERCURL) != 0).then_some(UnderlineStyle {
                thickness: px(1.0),
                color: Some(color),
                wavy: f & flags::UNDERCURL != 0,
            }),
            strikethrough: (f & flags::STRIKE != 0).then_some(StrikethroughStyle {
                thickness: px(1.0),
                color: Some(color),
            }),
        };
        let width = if wide { metrics.cell_w * 2.0 } else { metrics.cell_w };
        let line = window
            .text_system()
            .shape_line(text.into(), px(FONT_SIZE), &[run], Some(width));
        batches.push(Batch { col: start, line });
    }
    RowLayout {
        row: row.clone(),
        batches,
    }
}

struct TerminalElement {
    view: Entity<TermView>,
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, _cx| {
            let m = view.metrics(window);
            let cols = ((f32::from(bounds.size.width) - 2.0 * PADDING) / f32::from(m.cell_w))
                .floor()
                .max(1.0) as u16;
            let rows = ((f32::from(bounds.size.height) - 2.0 * PADDING) / f32::from(m.cell_h))
                .floor()
                .max(1.0) as u16;
            if (cols, rows) != view.grid {
                view.grid = (cols, rows);
                let scale = window.scale_factor();
                view.pane.send(Input::Resize {
                    cols,
                    rows,
                    cell_w: (f32::from(m.cell_w) * scale) as u32,
                    cell_h: (f32::from(m.cell_h) * scale) as u32,
                    width: (f32::from(m.cell_w) * cols as f32 * scale) as u32,
                    height: (f32::from(m.cell_h) * rows as f32 * scale) as u32,
                });
            }
            view.frame = view.pane.shared.lock().unwrap().frame.clone();
            let frame = view.frame.clone();
            view.layouts.resize_with(frame.rows.len(), || None);
            view.layouts.truncate(frame.rows.len());
            for (y, row) in frame.rows.iter().enumerate() {
                let fresh = view.layouts[y].as_ref().is_some_and(|l| Arc::ptr_eq(&l.row, row));
                if !fresh {
                    view.layouts[y] = Some(layout_row(row, m, window));
                }
            }
        });
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.view.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.view.clone()), cx);
        let focused = focus.is_focused(window);
        self.view.update(cx, |view, cx| {
            let m = view.metrics(window);
            let frame = view.frame.clone();
            let origin = bounds.origin + point(px(PADDING), px(PADDING));
            let cell = |col: f32, row: f32| origin + point(m.cell_w * col, m.cell_h * row);
            window.paint_quad(fill(bounds, hsla(frame.bg, 1.0)));
            // backgrounds, merged per run of one color
            for (y, row) in frame.rows.iter().enumerate() {
                let mut x = 0;
                while x < row.cells.len() {
                    let Some(bg) = row.cells[x].bg else {
                        x += 1;
                        continue;
                    };
                    let start = x;
                    while x < row.cells.len() && row.cells[x].bg == Some(bg) {
                        x += 1;
                    }
                    let b = Bounds::new(
                        cell(start as f32, y as f32),
                        size(m.cell_w * (x - start) as f32, m.cell_h),
                    );
                    window.paint_quad(fill(b, hsla(bg, 1.0)));
                }
            }
            let images: Vec<_> = frame
                .images
                .iter()
                .filter_map(|p| Some((p.clone(), view.render_image(p)?)))
                .collect();
            let paint_images = |above: bool, window: &mut Window| {
                for (p, image) in &images {
                    if p.above_text != above {
                        continue;
                    }
                    let at = cell(p.col as f32, p.row as f32) + point(px(p.x_offset as f32), px(p.y_offset as f32));
                    let b = Bounds::new(at, size(m.cell_w * p.cols as f32, m.cell_h * p.rows as f32));
                    let _ = window.paint_image(b, b, Corners::default(), image.clone(), 0, false);
                }
            };
            paint_images(false, window);
            for (y, layout) in view.layouts.iter().enumerate() {
                let Some(layout) = layout else { continue };
                for batch in &layout.batches {
                    let _ = batch.line.paint(
                        cell(batch.col as f32, y as f32),
                        m.cell_h,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
            }
            paint_images(true, window);
            if let Some(c) = frame.cursor {
                let color = hsla(c.color.unwrap_or(frame.fg), 1.0);
                let at = cell(c.x as f32, c.y as f32);
                let shape = if focused { c.shape } else { CursorShape::Hollow };
                match shape {
                    CursorShape::Block => {
                        window.paint_quad(fill(Bounds::new(at, size(m.cell_w, m.cell_h)), color));
                        if let Some(text) = frame
                            .rows
                            .get(c.y as usize)
                            .and_then(|r| r.cells.get(c.x as usize))
                            .map(|c| c.text.clone())
                            .filter(|t| !t.is_empty())
                        {
                            let run = TextRun {
                                len: text.len(),
                                font: TermView::font(false, false),
                                color: hsla(frame.bg, 1.0),
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            let line =
                                window
                                    .text_system()
                                    .shape_line(text.into(), px(FONT_SIZE), &[run], Some(m.cell_w));
                            let _ = line.paint(at, m.cell_h, TextAlign::Left, None, window, cx);
                        }
                    }
                    CursorShape::Bar => window.paint_quad(fill(Bounds::new(at, size(px(2.0), m.cell_h)), color)),
                    CursorShape::Underline => window.paint_quad(fill(
                        Bounds::new(at + point(px(0.0), m.cell_h - px(2.0)), size(m.cell_w, px(2.0))),
                        color,
                    )),
                    CursorShape::Hollow => window.paint_quad(outline(
                        Bounds::new(at, size(m.cell_w, m.cell_h)),
                        color,
                        BorderStyle::default(),
                    )),
                }
                // IME preedit, drawn over the cursor cell with an underline
                if let Some(preedit) = view.preedit.clone().filter(|p| !p.is_empty()) {
                    let fg = hsla(frame.fg, 1.0);
                    let run = TextRun {
                        len: preedit.len(),
                        font: TermView::font(false, false),
                        color: fg,
                        background_color: Some(hsla(frame.bg, 1.0)),
                        underline: Some(UnderlineStyle {
                            thickness: px(1.0),
                            color: Some(fg),
                            wavy: false,
                        }),
                        strikethrough: None,
                    };
                    let line = window
                        .text_system()
                        .shape_line(preedit.into(), px(FONT_SIZE), &[run], None);
                    let _ = line.paint(at, m.cell_h, TextAlign::Left, None, window, cx);
                }
            }
            view.painted += 1;
            if view.first_paint_ms.is_none() && !frame.rows.iter().all(|r| r.cells.iter().all(|c| c.text.is_empty())) {
                view.first_paint_ms = Some(view.started.elapsed().as_millis());
            }
            view.pane.send(Input::FrameWanted);
            let _ = cx;
        });
    }
}

impl EntityInputHandler for TermView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.preedit.as_ref().map(|p| 0..p.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.preedit = None;
        cx.notify();
    }

    fn replace_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        self.preedit = None;
        if !text.is_empty() {
            self.pane.send(Input::Text(text.to_owned()));
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preedit = (!text.is_empty()).then(|| text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let m = self.metrics?;
        let c = self.frame.cursor?;
        let at = element.origin + point(px(PADDING) + m.cell_w * c.x as f32, px(PADDING) + m.cell_h * c.y as f32);
        Some(Bounds::new(at, size(m.cell_w, m.cell_h)))
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
    }
}

impl Focusable for TermView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TermView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .child(TerminalElement { view: cx.entity() })
    }
}

/// Run one control command on the UI thread.
fn command(line: &str, view: &Entity<TermView>, window: &mut Window, cx: &mut App) -> Result<String, String> {
    let (name, rest) = line.split_once(' ').unwrap_or((line, ""));
    let json = |rest: &str| serde_json::from_str::<String>(rest).map_err(|e| e.to_string());
    match name {
        "dump" => Ok(view.read(cx).dump().to_string()),
        "key" => {
            let ks = Keystroke::parse(rest).map_err(|e| format!("{e:?}"))?;
            window.dispatch_keystroke(ks, cx);
            Ok(String::new())
        }
        "type" => {
            for ch in json(rest)?.chars() {
                let ks = match ch {
                    '\n' => Keystroke::parse("enter").unwrap(),
                    ' ' => Keystroke {
                        modifiers: Modifiers::default(),
                        key: "space".into(),
                        key_char: Some(" ".into()),
                    },
                    c => Keystroke {
                        modifiers: Modifiers {
                            shift: c.is_ascii_uppercase(),
                            ..Modifiers::default()
                        },
                        key: c.to_ascii_lowercase().to_string(),
                        key_char: Some(c.to_string()),
                    },
                };
                window.dispatch_keystroke(ks, cx);
            }
            Ok(String::new())
        }
        "preedit" => {
            let text = json(rest)?;
            view.update(cx, |v, cx| {
                v.replace_and_mark_text_in_range(None, &text, None, window, cx)
            });
            Ok(String::new())
        }
        "commit" => {
            let text = json(rest)?;
            view.update(cx, |v, cx| v.replace_text_in_range(None, &text, window, cx));
            Ok(String::new())
        }
        "scroll" => {
            let lines: f32 = rest.trim().parse().map_err(|e| format!("{e}"))?;
            let center = point(window.viewport_size().width / 2.0, window.viewport_size().height / 2.0);
            window.dispatch_event(
                PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position: center,
                    delta: ScrollDelta::Lines(point(0.0, lines)),
                    modifiers: Modifiers::default(),
                    touch_phase: TouchPhase::Moved,
                }),
                cx,
            );
            Ok(String::new())
        }
        "resize" => {
            let mut parts = rest.split_whitespace().map(|v| v.parse::<f32>());
            let (Some(Ok(w)), Some(Ok(h))) = (parts.next(), parts.next()) else {
                return Err("resize W H".into());
            };
            window.resize(size(px(w), px(h)));
            Ok(String::new())
        }
        "quit" => {
            view.read(cx).pane.send(Input::Shutdown);
            cx.quit();
            Ok(String::new())
        }
        other => Err(format!("unknown command {other}")),
    }
}

fn main() {
    let args = parse_args();
    let mut argv = args.argv.clone();
    if let Some(zmx) = &args.zmx {
        argv = [vec![zmx.clone(), "attach".into(), args.session.clone()], argv].concat();
    }
    let started = Instant::now();
    gpui_kit::application().run(move |cx: &mut App| {
        gpui_kit::init(cx);
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(args.width), px(args.height)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some("io.github.melonamin.agterm.gpui-spike".into()),
            ..Default::default()
        };
        let (wake_tx, wake_rx) = async_channel::unbounded::<()>();
        let (window, view) = gpui_kit::open_window(options, cx, |window, cx| {
            let view = cx.new(|cx| {
                let wake = wake_tx.clone();
                let pane = Pane::spawn(
                    Spawn {
                        argv: argv.clone(),
                        env: vec![
                            ("TERM".into(), "xterm-256color".into()),
                            ("COLORTERM".into(), "truecolor".into()),
                        ],
                        cols: 80,
                        rows: 24,
                    },
                    move || {
                        let _ = wake.try_send(());
                    },
                )
                .expect("spawn pane");
                TermView {
                    focus: cx.focus_handle(),
                    pane,
                    frame: Arc::new(Frame::default()),
                    metrics: None,
                    grid: (0, 0),
                    preedit: None,
                    painted: 0,
                    layouts: Vec::new(),
                    images: HashMap::new(),
                    started,
                    first_paint_ms: None,
                }
            });
            window.focus(&view.read(cx).focus.clone(), cx);
            view
        })
        .expect("open window");

        // Frames from the terminal thread wake the view; bursts collapse into one notify.
        let wake_view = view.clone();
        cx.spawn(async move |cx| {
            while wake_rx.recv().await.is_ok() {
                while wake_rx.try_recv().is_ok() {}
                let exited = cx.update(|cx| {
                    wake_view.update(cx, |_, cx| cx.notify());
                    wake_view.read(cx).pane.shared.lock().unwrap().exited.is_some()
                });
                if exited {
                    let _ = cx.update(|cx| cx.quit());
                    break;
                }
            }
        })
        .detach();

        if let Some(path) = &args.control {
            let (tx, rx) = async_channel::unbounded::<control::Request>();
            control::serve(path, tx).expect("control socket");
            let control_view = view.clone();
            cx.spawn(async move |cx| {
                while let Ok(request) = rx.recv().await {
                    let result = window
                        .update(cx, |_, window, cx| command(&request.line, &control_view, window, cx))
                        .unwrap_or_else(|e| Err(e.to_string()));
                    let _ = request.reply.send(result);
                }
            })
            .detach();
        }
        cx.activate(true);
    });
}
