//! A markdown HUD message laid out for the painter (upstream's `HudMarkdown`): [`lines`] walks the parsed blocks
//! into prefixed logical rows of styled runs, [`rows`] wraps them, [`fitted`] clips them to the panel's grid and
//! [`sgr`] encodes each finished row.
//!
//! Upstream parses with Foundation's `AttributedString(markdown:)` (`.full` syntax); this port walks
//! `pulldown-cmark`'s events with tables and strikethrough on and reproduces the rows Foundation's runs produce.

use std::collections::HashSet;

use bitflags::bitflags;
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};
use unicode_normalization::UnicodeNormalization;

use super::layout::{MAX_COLUMNS, cell_count};
use crate::link_policy::{self, LinkDisposition};

bitflags! {
    /// How a run is painted (upstream's `HudMarkdown.Style`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Style: u8 {
        const BOLD = 1;
        const ITALIC = 2;
        const STRIKETHROUGH = 4;
        const DIM = 8;
        const UNDERLINE = 16;
    }
}

/// A styled stretch of text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Run {
    pub text: String,
    pub style: Style,
    /// The target a click on this run opens, set only for a URL [`link_target`] accepts.
    pub link: Option<String>,
}

impl Run {
    #[must_use]
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            link: None,
        }
    }

    #[must_use]
    pub fn linked(text: impl Into<String>, style: Style, link: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style,
            link: Some(link.into()),
        }
    }
}

/// How a [`Line`] wraps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineKind {
    /// Wraps at word boundaries.
    #[default]
    Text,
    /// Wraps at the width, keeping every space.
    Code,
    /// Never wraps; a row wider than the panel is clipped.
    Table,
    /// A thematic break, drawn across whatever width the row is laid out at.
    Rule,
}

/// One logical row. `lead` prefixes its first wrapped row (indent, quote bars, list marker) and `hang` every
/// continuation, so wrapped list text hangs under the item rather than under the marker.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Line {
    pub lead: String,
    pub hang: String,
    pub runs: Vec<Run>,
    pub kind: LineKind,
}

impl Line {
    /// The empty row separating two blocks.
    #[must_use]
    pub fn blank() -> Self {
        Self::default()
    }
}

pub const CODE_INDENT: &str = "  ";
pub const QUOTE_BAR: &str = "│ ";
pub const BULLET: &str = "• ";
pub const MIN_RULE_WIDTH: usize = 3;
pub const TAB_WIDTH: usize = 4;
pub const ELLIPSIS: &str = "…";
pub const RULE_GLYPH: &str = "─";

/// Parses `source` as standard markdown (precomposed first). Blocks are separated by one blank row, except blocks
/// sharing a list: upstream's parser does not say whether a list was tight or loose, so both render alike.
#[must_use]
pub fn lines(source: &str) -> Vec<Line> {
    let text: String = source.nfc().collect();
    let mut walker = Walker::default();
    for event in Parser::new_ext(&text, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH) {
        walker.event(event);
    }
    walker.finish()
}

/// Whether `source` lays out to at least one non-space cell.
#[must_use]
pub fn renders_visible_text(source: &str) -> bool {
    rows(&lines(source), MAX_COLUMNS)
        .iter()
        .flatten()
        .any(|run| run.text.chars().any(|scalar| !scalar.is_whitespace()))
}

/// Replaces the control characters the parser decoded from entities (`&#27;`, `&#10;`) so none can reach the
/// terminal or split a row. A tab in running text becomes a space; code blocks expand theirs before this runs.
#[must_use]
pub fn sanitized(text: &str) -> String {
    text.chars()
        .map(|scalar| match scalar {
            '\t' => ' ',
            '\0'..='\u{1f}' | '\u{7f}' => '\u{FFFD}',
            other => other,
        })
        .collect()
}

/// The URL a click may act on, or `None` when the link policy would ignore it. Only printable ASCII passes, so the
/// target can ride inside an OSC 8 sequence without ending it early.
#[must_use]
pub fn link_target(raw: &str) -> Option<String> {
    let printable = raw.chars().all(|scalar| ('\u{21}'..='\u{7e}').contains(&scalar));
    (printable && link_policy::disposition(raw) != LinkDisposition::Ignore).then(|| raw.to_owned())
}

/// Replaces tabs with spaces up to the next [`TAB_WIDTH`] stop, counted from the line start.
#[must_use]
pub fn expand_tabs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut column = 0;
    for scalar in line.chars() {
        if scalar == '\t' {
            let pad = TAB_WIDTH - column % TAB_WIDTH;
            out.extend(std::iter::repeat_n(' ', pad));
            column += pad;
        } else {
            out.push(scalar);
            column += 1;
        }
    }
    out
}

bitflags! {
    /// The inline intents a segment carries (Foundation's `InlinePresentationIntent`, as far as the HUD reads it).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    struct Inline: u8 {
        const STRONG = 1;
        const EMPHASIS = 2;
        const STRIKETHROUGH = 4;
        const CODE = 8;
        const IMAGE = 16;
        const SOFT_BREAK = 32;
        const LINE_BREAK = 64;
    }
}

/// One Foundation run's worth of inline text: adjacent text sharing intents and link is one segment.
#[derive(Debug, Clone)]
struct Segment {
    text: String,
    inline: Inline,
    link: Option<String>,
}

impl Segment {
    fn joins(&self, next: &Self) -> bool {
        let breaks = Inline::SOFT_BREAK | Inline::LINE_BREAK;
        self.inline == next.inline && self.link == next.link && !self.inline.intersects(breaks)
    }
}

/// A block container around the block being walked, outermost first on the walker's stack.
#[derive(Debug, Clone)]
enum Container {
    Quote,
    /// `next` is the next item's ordinal for an ordered list, `None` for a bulleted one.
    List {
        id: usize,
        next: Option<u64>,
    },
    Item {
        id: usize,
        marker: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Paragraph,
    Heading,
    Code,
    /// A raw HTML block: its source, one row per line.
    Html,
}

#[derive(Debug)]
struct Block {
    kind: BlockKind,
    containers: Vec<Container>,
    segments: Vec<Segment>,
    /// The verbatim text of a code or HTML block.
    raw: String,
}

/// A table collected cell by cell, flushed at its end.
#[derive(Debug, Default)]
struct PendingTable {
    containers: Vec<Container>,
    columns: usize,
    header: Vec<Vec<Run>>,
    body: Vec<Vec<Vec<Run>>>,
    row: Vec<Vec<Run>>,
    cell: Vec<Segment>,
    in_head: bool,
}

impl PendingTable {
    /// The rows Foundation reports: a header only when a header cell holds text, and no trailing body row whose
    /// cells are all empty (it emits no run for those). Every row is padded to the column count.
    fn grid(&self) -> (bool, Vec<Vec<Vec<Run>>>) {
        let has_header = !is_empty_row(&self.header);
        let kept = self.body.len() - self.body.iter().rev().take_while(|row| is_empty_row(row)).count();
        let rows = has_header
            .then_some(&self.header)
            .into_iter()
            .chain(&self.body[..kept])
            .map(|row| {
                (0..self.columns)
                    .map(|index| row.get(index).cloned().unwrap_or_default())
                    .collect()
            })
            .collect();
        (has_header, rows)
    }
}

fn is_empty_row(row: &[Vec<Run>]) -> bool {
    row.iter().flatten().all(|run| run.text.is_empty())
}

#[derive(Debug, Default)]
struct Walker {
    out: Vec<Line>,
    containers: Vec<Container>,
    next_id: usize,
    seen_items: HashSet<usize>,
    last_list_ids: Option<Vec<usize>>,
    block: Option<Block>,
    table: Option<PendingTable>,
    styles: Vec<Inline>,
    links: Vec<String>,
}

impl Walker {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => self.text(&text),
            Event::Code(text) => self.segment(&text, Inline::CODE),
            Event::SoftBreak => self.segment("\n", Inline::SOFT_BREAK),
            Event::HardBreak => self.segment("\n", Inline::LINE_BREAK),
            Event::Rule => self.rule(),
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Line> {
        self.flush_block();
        self.flush_table();
        self.out
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.open(BlockKind::Paragraph),
            Tag::Heading { .. } => self.open(BlockKind::Heading),
            Tag::CodeBlock(_) => self.open(BlockKind::Code),
            Tag::HtmlBlock => self.open(BlockKind::Html),
            Tag::BlockQuote(_) => self.push_container(Container::Quote),
            Tag::List(start) => {
                let id = self.fresh_id();
                self.push_container(Container::List { id, next: start });
            }
            Tag::Item => {
                let id = self.fresh_id();
                let marker = self.next_marker();
                self.push_container(Container::Item { id, marker });
            }
            Tag::Table(alignments) => {
                self.flush_block();
                self.table = Some(PendingTable {
                    containers: self.containers.clone(),
                    columns: alignments.len(),
                    ..PendingTable::default()
                });
            }
            Tag::TableHead => self.with_table(|table| table.in_head = true),
            Tag::Emphasis => self.styles.push(Inline::EMPHASIS),
            Tag::Strong => self.styles.push(Inline::STRONG),
            Tag::Strikethrough => self.styles.push(Inline::STRIKETHROUGH),
            Tag::Image { .. } => self.styles.push(Inline::IMAGE),
            Tag::Link {
                link_type, dest_url, ..
            } => self.links.push(match link_type {
                LinkType::Email => format!("mailto:{dest_url}"),
                _ => dest_url.into_string(),
            }),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock | TagEnd::HtmlBlock => self.flush_block(),
            TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item => {
                self.flush_block();
                self.containers.pop();
            }
            TagEnd::Table => self.flush_table(),
            TagEnd::TableHead => self.with_table(|table| {
                table.header = std::mem::take(&mut table.row);
                table.in_head = false;
            }),
            TagEnd::TableRow => self.with_table(|table| {
                if !table.in_head {
                    let row = std::mem::take(&mut table.row);
                    table.body.push(row);
                }
            }),
            TagEnd::TableCell => self.with_table(|table| {
                let base = if table.in_head { Style::BOLD } else { Style::empty() };
                let runs = inline_rows(&std::mem::take(&mut table.cell), base).concat();
                table.row.push(runs);
            }),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Image => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.links.pop();
            }
            _ => {}
        }
    }

    fn with_table(&mut self, change: impl FnOnce(&mut PendingTable)) {
        if let Some(table) = self.table.as_mut() {
            change(table);
        }
    }

    fn fresh_id(&mut self) -> usize {
        self.next_id += 1;
        self.next_id
    }

    /// The marker of the item opening in the innermost list: its ordinal for an ordered list, else the bullet.
    fn next_marker(&mut self) -> String {
        match self.containers.last_mut() {
            Some(Container::List {
                next: Some(ordinal), ..
            }) => {
                let marker = format!("{ordinal}. ");
                *ordinal += 1;
                marker
            }
            _ => BULLET.to_owned(),
        }
    }

    fn push_container(&mut self, container: Container) {
        self.flush_block();
        self.containers.push(container);
    }

    fn open(&mut self, kind: BlockKind) {
        self.flush_block();
        self.block = Some(Block {
            kind,
            containers: self.containers.clone(),
            segments: Vec::new(),
            raw: String::new(),
        });
    }

    fn text(&mut self, text: &str) {
        match self.block.as_mut() {
            Some(block) if matches!(block.kind, BlockKind::Code | BlockKind::Html) => block.raw.push_str(text),
            _ => self.segment(text, Inline::empty()),
        }
    }

    /// Adds inline text to the open table cell, else to the open block, opening a paragraph for the bare text of a
    /// tight list item.
    fn segment(&mut self, text: &str, extra: Inline) {
        let segment = Segment {
            text: text.to_owned(),
            inline: self.styles.iter().fold(extra, |all, style| all | *style),
            link: self.links.last().cloned(),
        };
        let segments = if let Some(table) = self.table.as_mut() {
            &mut table.cell
        } else {
            let containers = &self.containers;
            let block = self.block.get_or_insert_with(|| Block {
                kind: BlockKind::Paragraph,
                containers: containers.clone(),
                segments: Vec::new(),
                raw: String::new(),
            });
            &mut block.segments
        };
        match segments.last_mut() {
            Some(last) if last.joins(&segment) => last.text.push_str(&segment.text),
            _ => segments.push(segment),
        }
    }

    fn rule(&mut self) {
        self.flush_block();
        let containers = self.containers.clone();
        let (lead, hang) = self.prefixes(&containers);
        let rule = Line {
            lead,
            hang,
            runs: Vec::new(),
            kind: LineKind::Rule,
        };
        self.emit(vec![rule], &containers);
    }

    fn flush_block(&mut self) {
        let Some(block) = self.block.take() else {
            return;
        };
        let lines = self.render(&block);
        self.emit(lines, &block.containers);
    }

    fn render(&mut self, block: &Block) -> Vec<Line> {
        match block.kind {
            BlockKind::Code => self.code_lines(block),
            BlockKind::Html => {
                let mut body: Vec<&str> = block.raw.split('\n').collect();
                while body.last().is_some_and(|line| line.is_empty()) {
                    body.pop();
                }
                let (lead, hang) = self.prefixes(&block.containers);
                let rows = body.iter().map(|line| vec![Run::new(sanitized(line), Style::empty())]);
                text_lines(rows, &lead, &hang)
            }
            BlockKind::Heading | BlockKind::Paragraph if block.segments.is_empty() => Vec::new(),
            BlockKind::Heading | BlockKind::Paragraph => {
                let base = if block.kind == BlockKind::Heading {
                    Style::BOLD
                } else {
                    Style::empty()
                };
                let (lead, hang) = self.prefixes(&block.containers);
                text_lines(inline_rows(&block.segments, base), &lead, &hang)
            }
        }
    }

    fn code_lines(&mut self, block: &Block) -> Vec<Line> {
        let mut body: Vec<&str> = block.raw.split('\n').collect();
        if body.last().is_some_and(|line| line.is_empty()) {
            body.pop();
        }
        if body.is_empty() {
            return Vec::new();
        }
        let (lead, hang) = self.prefixes(&block.containers);
        body.iter()
            .enumerate()
            .map(|(index, raw)| Line {
                lead: format!("{}{CODE_INDENT}", if index == 0 { &lead } else { &hang }),
                hang: format!("{hang}{CODE_INDENT}"),
                runs: vec![Run::new(sanitized(&expand_tabs(raw)), Style::empty())],
                kind: LineKind::Code,
            })
            .collect()
    }

    /// Frames the table in box-drawing borders, with a rule under the header when it has one. The top border takes
    /// the container's lead, so a table opening a list item carries its marker once.
    fn flush_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        let (has_header, grid) = table.grid();
        if grid.is_empty() {
            return;
        }
        let mut widths = vec![0; table.columns];
        for row in &grid {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(width(cell));
            }
        }
        let mut rows = vec![border(&widths, ["┌", "┬", "┐"])];
        for (offset, row) in grid.iter().enumerate() {
            rows.push(framed(row, &widths));
            if offset == 0 && has_header {
                rows.push(border(&widths, ["├", "┼", "┤"]));
            }
        }
        rows.push(border(&widths, ["└", "┴", "┘"]));
        let (lead, hang) = self.prefixes(&table.containers);
        let lines = rows
            .into_iter()
            .enumerate()
            .map(|(offset, runs)| Line {
                lead: if offset == 0 { lead.clone() } else { hang.clone() },
                hang: hang.clone(),
                runs,
                kind: LineKind::Table,
            })
            .collect();
        self.emit(lines, &table.containers);
    }

    /// Appends `lines`, after a blank row unless the previous block shares a list with this one.
    fn emit(&mut self, lines: Vec<Line>, containers: &[Container]) {
        if lines.is_empty() {
            return;
        }
        let list_ids: Vec<usize> = containers
            .iter()
            .filter_map(|container| match container {
                Container::List { id, .. } => Some(*id),
                _ => None,
            })
            .collect();
        if let Some(last) = &self.last_list_ids
            && (list_ids.is_empty() || !last.iter().any(|id| list_ids.contains(id)))
        {
            self.out.push(Line::blank());
        }
        self.last_list_ids = Some(list_ids);
        self.out.extend(lines);
    }

    /// Walks the containers outermost first: a quote adds a bar to both prefixes, and a list item adds its marker to
    /// `lead` only for the item's first block, spaces of the same width otherwise.
    fn prefixes(&mut self, containers: &[Container]) -> (String, String) {
        let mut lead = String::new();
        let mut hang = String::new();
        for container in containers {
            match container {
                Container::Quote => {
                    lead.push_str(QUOTE_BAR);
                    hang.push_str(QUOTE_BAR);
                }
                Container::List { .. } => {}
                Container::Item { id, marker } => {
                    let pad = " ".repeat(cell_count(marker));
                    lead.push_str(if self.seen_items.insert(*id) { marker } else { &pad });
                    hang.push_str(&pad);
                }
            }
        }
        (lead, hang)
    }
}

fn text_lines(rows: impl IntoIterator<Item = Vec<Run>>, lead: &str, hang: &str) -> Vec<Line> {
    rows.into_iter()
        .enumerate()
        .map(|(offset, runs)| Line {
            lead: if offset == 0 { lead } else { hang }.to_owned(),
            hang: hang.to_owned(),
            runs,
            kind: LineKind::Text,
        })
        .collect()
}

/// Maps inline intents onto styles and splits at hard breaks; a soft break is a space. A link is its label,
/// underlined when a click can open it.
fn inline_rows(segments: &[Segment], base: Style) -> Vec<Vec<Run>> {
    let mut rows: Vec<Vec<Run>> = vec![Vec::new()];
    for segment in segments {
        if segment.inline.contains(Inline::LINE_BREAK) {
            rows.push(Vec::new());
            continue;
        }
        let link = segment.link.as_deref().and_then(link_target);
        let mut style = base;
        for (intent, flag) in [
            (Inline::STRONG, Style::BOLD),
            (Inline::EMPHASIS, Style::ITALIC),
            (Inline::STRIKETHROUGH, Style::STRIKETHROUGH),
        ] {
            if segment.inline.contains(intent) {
                style.insert(flag);
            }
        }
        if link.is_some() {
            style.insert(Style::UNDERLINE);
        }
        let text = if segment.inline.contains(Inline::SOFT_BREAK) {
            " ".to_owned()
        } else {
            sanitized(&segment.text)
        };
        if let Some(row) = rows.last_mut() {
            row.push(Run { text, style, link });
        }
    }
    rows
}

fn border(widths: &[usize], [left, join, right]: [&str; 3]) -> Vec<Run> {
    let segments: Vec<String> = widths.iter().map(|width| RULE_GLYPH.repeat(width + 2)).collect();
    vec![Run::new(
        format!("{left}{}{right}", segments.join(join)),
        Style::empty(),
    )]
}

fn framed(row: &[Vec<Run>], widths: &[usize]) -> Vec<Run> {
    let mut runs = vec![Run::new("│ ", Style::empty())];
    for (index, cell) in row.iter().enumerate() {
        if index > 0 {
            runs.push(Run::new(" ", Style::empty()));
        }
        runs.extend(cell.iter().cloned());
        let pad = " ".repeat(widths[index] - width(cell) + 1);
        runs.push(Run::new(format!("{pad}│"), Style::empty()));
    }
    runs
}

/// One painted cell: a scalar with its run's style and link.
#[derive(Debug, Clone, Copy)]
struct Cell<'a> {
    scalar: char,
    style: Style,
    link: Option<&'a str>,
}

/// The first row's and every continuation's cell budget.
#[derive(Debug, Clone, Copy)]
struct Limits {
    first: usize,
    rest: usize,
}

impl Limits {
    fn at(self, rows: usize) -> usize {
        if rows == 0 { self.first } else { self.rest }
    }
}

/// Wraps `lines` at `width` cells. Every row starts with its line's lead (first row) or hang (continuations); a
/// blank line is one empty row. A rule spans the widest other row, and its glyphs number at least
/// [`MIN_RULE_WIDTH`] after any container prefix.
#[must_use]
pub fn rows(lines: &[Line], width: usize) -> Vec<Vec<Run>> {
    let widest = lines
        .iter()
        .filter(|line| line.kind != LineKind::Rule)
        .flat_map(|line| wrapped(line, width))
        .map(|row| self::width(&row))
        .max()
        .unwrap_or(0);
    let rule_width = widest.max(MIN_RULE_WIDTH).min(width);
    lines
        .iter()
        .flat_map(|line| wrapped(line, if line.kind == LineKind::Rule { rule_width } else { width }))
        .collect()
}

/// Clips `rows` to a `columns` x `limit` budget. A row too wide keeps `columns - 1` cells and ends in `…`; too many
/// rows keep `limit - 1` and end in a dimmed `… N more`, N counting every hidden row.
#[must_use]
pub fn fitted(rows: &[Vec<Run>], columns: usize, limit: usize) -> Vec<Vec<Run>> {
    if limit == 0 || columns == 0 {
        return Vec::new();
    }
    let mut kept = rows.to_vec();
    if rows.len() > limit {
        kept.truncate(limit - 1);
        let hidden = rows.len() - kept.len();
        kept.push(vec![Run::new(format!("{ELLIPSIS} {hidden} more"), Style::DIM)]);
    }
    kept.iter().map(|row| clipped(row, columns)).collect()
}

/// `row` cut to `columns` cells, ending in `…` when it had to be cut.
#[must_use]
pub fn clipped(row: &[Run], columns: usize) -> Vec<Run> {
    if width(row) <= columns {
        return row.to_vec();
    }
    if columns == 0 {
        return Vec::new();
    }
    let mut out = merged(cells(row).into_iter().take(columns - 1));
    out.push(Run::new(ELLIPSIS, Style::empty()));
    out
}

/// The cells `row` paints, in [`cell_count`]'s unit.
#[must_use]
pub fn width(row: &[Run]) -> usize {
    row.iter().map(|run| cell_count(&run.text)).sum()
}

/// Encodes `row` for the painter. A style change emits only the codes it needs, and every style opened is closed
/// with its own reset (22, 23, 24, 29), never SGR 0, so the header's text color holds. A linked run is wrapped in an
/// OSC 8 hyperlink, closed by the end of the row.
#[must_use]
pub fn sgr(row: &[Run]) -> String {
    let mut out = String::new();
    let mut current = Style::empty();
    let mut link: Option<&str> = None;
    for run in row.iter().filter(|run| !run.text.is_empty()) {
        if run.link.as_deref() != link {
            out.push_str(&hyperlink(run.link.as_deref()));
        }
        out.push_str(&transition(current, run.style));
        out.push_str(&run.text);
        current = run.style;
        link = run.link.as_deref();
    }
    out.push_str(&transition(current, Style::empty()));
    if link.is_some() {
        out.push_str(&hyperlink(None));
    }
    out
}

/// Opens an OSC 8 link to `target`, or closes the open one when it is `None`.
fn hyperlink(target: Option<&str>) -> String {
    format!("\u{1B}]8;;{}\u{1B}\\", target.unwrap_or_default())
}

fn transition(current: Style, next: Style) -> String {
    let mut codes: Vec<&str> = Vec::new();
    let mut open = current;
    // 22 is the only reset for both bold and dim, so dropping either drops both and reopens the survivor
    let dropped = |flag: Style| open.contains(flag) && !next.contains(flag);
    if dropped(Style::BOLD) || dropped(Style::DIM) {
        codes.push("22");
        open.remove(Style::BOLD | Style::DIM);
    }
    for (flag, code) in [
        (Style::ITALIC, "23"),
        (Style::UNDERLINE, "24"),
        (Style::STRIKETHROUGH, "29"),
    ] {
        if open.contains(flag) && !next.contains(flag) {
            codes.push(code);
            open.remove(flag);
        }
    }
    let on = [
        (Style::BOLD, "1"),
        (Style::DIM, "2"),
        (Style::ITALIC, "3"),
        (Style::UNDERLINE, "4"),
        (Style::STRIKETHROUGH, "9"),
    ];
    codes.extend(
        on.into_iter()
            .filter(|(flag, _)| next.contains(*flag) && !open.contains(*flag))
            .map(|(_, code)| code),
    );
    if codes.is_empty() {
        String::new()
    } else {
        format!("\u{1B}[{}m", codes.join(";"))
    }
}

fn wrapped(line: &Line, width: usize) -> Vec<Vec<Run>> {
    let lead_width = cell_count(&line.lead);
    match line.kind {
        LineKind::Rule => {
            let glyphs = RULE_GLYPH.repeat(width.saturating_sub(lead_width).max(MIN_RULE_WIDTH));
            vec![prefixed(&line.lead, vec![Run::new(glyphs, Style::empty())])]
        }
        LineKind::Table => vec![prefixed(&line.lead, line.runs.clone())],
        LineKind::Code | LineKind::Text => {
            let limits = Limits {
                first: width.saturating_sub(lead_width).max(1),
                rest: width.saturating_sub(cell_count(&line.hang)).max(1),
            };
            let cells = cells(&line.runs);
            let broken = if line.kind == LineKind::Code {
                hard_wrapped(&cells, limits)
            } else {
                word_wrapped(&cells, limits)
            };
            broken
                .into_iter()
                .enumerate()
                .map(|(offset, row)| prefixed(if offset == 0 { &line.lead } else { &line.hang }, merged(row)))
                .collect()
        }
    }
}

fn hard_wrapped<'a>(cells: &[Cell<'a>], limits: Limits) -> Vec<Vec<Cell<'a>>> {
    let mut rows: Vec<Vec<Cell<'a>>> = Vec::new();
    let mut rest = cells;
    loop {
        let (row, tail) = rest.split_at(limits.at(rows.len()).min(rest.len()));
        rows.push(row.to_vec());
        rest = tail;
        if rest.is_empty() {
            return rows;
        }
    }
}

/// Breaks at spaces, carrying each word's styles across the break, and splits a word longer than the row. Spaces are
/// kept as written, the line's leading ones included, since code spans and raw HTML carry their spacing as content;
/// they are dropped at a row break and at the line's end.
fn word_wrapped<'a>(cells: &[Cell<'a>], limits: Limits) -> Vec<Vec<Cell<'a>>> {
    let mut rows: Vec<Vec<Cell<'a>>> = Vec::new();
    let mut current: Vec<Cell<'a>> = Vec::new();
    for (index, (separator, word)) in words(cells).into_iter().enumerate() {
        let mut word: &[Cell<'a>] = if index == 0 {
            &cells[separator.start..word.end]
        } else {
            &cells[word]
        };
        let separator = &cells[separator];
        if !current.is_empty() {
            if current.len() + separator.len() + word.len() <= limits.at(rows.len()) {
                current.extend_from_slice(separator);
                current.extend_from_slice(word);
                continue;
            }
            rows.push(std::mem::take(&mut current));
        }
        while word.len() > limits.at(rows.len()) {
            let (row, tail) = word.split_at(limits.at(rows.len()));
            rows.push(row.to_vec());
            word = tail;
        }
        current = word.to_vec();
    }
    if !current.is_empty() || rows.is_empty() {
        rows.push(current);
    }
    rows
}

/// Splits `cells` at spaces: each word's range paired with the range of the spaces before it.
fn words(cells: &[Cell<'_>]) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let mut out = Vec::new();
    let mut separator_start = 0;
    let mut word_start: Option<usize> = None;
    for (index, cell) in cells.iter().enumerate() {
        match (cell.scalar == ' ', word_start) {
            (true, Some(start)) => {
                out.push((separator_start..start, start..index));
                word_start = None;
                separator_start = index;
            }
            (false, None) => word_start = Some(index),
            _ => {}
        }
    }
    if let Some(start) = word_start {
        out.push((separator_start..start, start..cells.len()));
    }
    out
}

fn cells(runs: &[Run]) -> Vec<Cell<'_>> {
    runs.iter()
        .flat_map(|run| {
            run.text.chars().map(|scalar| Cell {
                scalar,
                style: run.style,
                link: run.link.as_deref(),
            })
        })
        .collect()
}

fn merged<'a>(cells: impl IntoIterator<Item = Cell<'a>>) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for cell in cells {
        match runs.last_mut() {
            Some(last) if last.style == cell.style && last.link.as_deref() == cell.link => last.text.push(cell.scalar),
            _ => runs.push(Run {
                text: cell.scalar.to_string(),
                style: cell.style,
                link: cell.link.map(str::to_owned),
            }),
        }
    }
    runs
}

fn prefixed(prefix: &str, runs: Vec<Run>) -> Vec<Run> {
    if prefix.is_empty() {
        return runs;
    }
    let mut out = Vec::with_capacity(runs.len() + 1);
    out.push(Run::new(prefix, Style::empty()));
    out.extend(runs);
    out
}

#[cfg(test)]
mod tests;
