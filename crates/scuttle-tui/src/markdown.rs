//! Markdown to styled terminal lines, with tables laid out to the pane width, recording where
//! code blocks and links are for copying and clicking.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::highlight;
use crate::wrap::{cells_width, cols_on_rows, wrap_line};

/// Cap on cached entries; the cache is cleared rather than evicted individually since
/// rendering is cheap and a transcript's distinct messages rarely exceed this.
const CACHE_LIMIT: usize = 512;

/// The narrowest a table column shrinks to; below that the table stacks its rows instead.
const MIN_COLUMN: usize = 4;

thread_local! {
    static CACHE: RefCell<HashMap<u64, (String, u16, Rendered)>> = RefCell::new(HashMap::new());
}

/// The width that decides how `text` renders: the pane's for text that may hold a table, which
/// is laid out to fit it, and 0 for any other text, which renders the same at every width, so
/// a resize finds it cached.
fn layout_width(text: &str, width: u16) -> u16 {
    if text.contains('|') { width } else { 0 }
}

fn cache_key(text: &str, width: u16) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    width.hash(&mut hasher);
    highlight::is_ready().hash(&mut hasher);
    hasher.finish()
}

/// `render`, memoized by the text, the width a table in it is laid out for, and whether syntax
/// highlighting is ready, so re-rendering an unchanged message (every redraw, until the
/// assets warm up flips highlighting on) doesn't redo the parse and highlight work. The text
/// and width are stored alongside the hash so a collision is detected as a miss rather than
/// returning the wrong `Rendered`.
pub fn render_cached(text: &str, width: u16) -> Rendered {
    let width = layout_width(text, width);
    let key = cache_key(text, width);
    CACHE.with(|cache| {
        if let Some((cached_text, cached_width, hit)) = cache.borrow().get(&key)
            && cached_text == text
            && *cached_width == width
        {
            return hit.clone();
        }
        let rendered = render(text, width);
        let mut cache = cache.borrow_mut();
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, (text.to_owned(), width, rendered.clone()));
        rendered
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodeBlock {
    pub start: usize,
    pub end: usize,
    pub code: String,
}

/// Link text on rendered line `line`, at display columns `cols` of that line before wrapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub line: usize,
    pub cols: Range<usize>,
    pub url: String,
    /// Which link of the text this is, counted from 0, so the parts of one link that a line
    /// break or a table cell's wrap splits share it.
    pub id: usize,
}

/// A link's destination and its [`Link::id`], carried with each part of it until it is placed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    url: String,
    id: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub code_blocks: Vec<CodeBlock>,
    pub links: Vec<Link>,
}

/// One table cell: its styled text, its links as span index ranges into it, and the span
/// indices where a `<br>` starts a new line inside it.
#[derive(Default)]
struct Cell {
    spans: Vec<Span<'static>>,
    links: Vec<(usize, usize, Target)>,
    breaks: Vec<usize>,
}

impl Cell {
    /// The span index ranges of the cell's lines, split at its `<br>` tags.
    fn lines(&self) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        let mut start = 0;
        for at in self.breaks.iter().copied().chain([self.spans.len()]) {
            out.push(start..at.max(start));
            start = at.max(start);
        }
        out
    }

    /// The display columns of the cell's widest line.
    fn width(&self) -> usize {
        self.lines()
            .into_iter()
            .map(|line| span_cols(&self.spans[line]))
            .max()
            .unwrap_or(0)
    }

    /// The display columns of the cell's longest word.
    fn longest_word(&self) -> usize {
        self.lines()
            .into_iter()
            .flat_map(|line| {
                let text: String = self.spans[line]
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect();
                text.split_whitespace().map(cells_width).collect::<Vec<_>>()
            })
            .max()
            .unwrap_or(0)
    }
}

/// Whether inline HTML `html` is a line break tag: `<br>`, `<br/>`, or `<br />`, in any case.
fn is_br(html: &str) -> bool {
    let tag = html.trim().to_ascii_lowercase();
    tag.strip_prefix("<br")
        .and_then(|rest| rest.strip_suffix('>'))
        .is_some_and(|rest| matches!(rest.trim(), "" | "/"))
}

/// A table being read: its column alignments, its header row, and its body rows.
struct TableBuf {
    alignments: Vec<Alignment>,
    header: Vec<Cell>,
    rows: Vec<Vec<Cell>>,
    /// The cells of the row being read.
    row: Vec<Cell>,
}

impl TableBuf {
    /// How many columns the table has: its alignment row's count, or more if a row runs longer.
    fn columns(&self) -> usize {
        std::iter::once(&self.header)
            .chain(&self.rows)
            .map(Vec::len)
            .max()
            .unwrap_or(0)
            .max(self.alignments.len())
    }
}

/// One rendered table line, and its links as display columns of the line.
#[derive(Default)]
struct TableLine {
    spans: Vec<Span<'static>>,
    links: Vec<(Range<usize>, Target)>,
}

/// The display columns `spans` take.
fn span_cols(spans: &[Span]) -> usize {
    spans.iter().map(|s| cells_width(&s.content)).sum()
}

/// A link's part on one row of a wrapped cell: (row, display columns in that row, URL).
type RowLink = (usize, Range<usize>, Target);

/// `row` without its leading and trailing spaces, and how many leading columns it lost.
fn trim_row(row: Line<'static>) -> (Line<'static>, usize) {
    let mut spans = row.spans;
    let mut lead = 0;
    while let Some(first) = spans.first_mut() {
        let kept = first.content.trim_start_matches(' ');
        lead += first.content.len() - kept.len();
        if kept.is_empty() {
            spans.remove(0);
        } else {
            first.content = kept.to_owned().into();
            break;
        }
    }
    while let Some(last) = spans.last_mut() {
        let kept = last.content.trim_end_matches(' ');
        if kept.is_empty() {
            spans.pop();
        } else {
            last.content = kept.to_owned().into();
            break;
        }
    }
    (Line::from(spans).style(row.style), lead)
}

/// `cell` wrapped to `width` columns, each of its lines on its own rows, with every row trimmed
/// of the spaces a wrap leaves at its edges, and each link's part on each row.
fn wrap_cell(cell: &Cell, width: usize) -> (Vec<Line<'static>>, Vec<RowLink>) {
    let mut rows = Vec::new();
    let mut links = Vec::new();
    for line in cell.lines() {
        let spans = &cell.spans[line.clone()];
        let wrapped = wrap_line(&Line::from(spans.to_vec()), width.max(1) as u16);
        let mut parts = Vec::new();
        for (start, end, url) in &cell.links {
            let (start, end) = ((*start).max(line.start), (*end).min(line.end));
            if start >= end {
                continue;
            }
            let from = span_cols(&cell.spans[line.start..start]);
            let to = from + span_cols(&cell.spans[start..end]);
            parts.extend(
                cols_on_rows(&wrapped, &(from..to))
                    .into_iter()
                    .map(|(row, cols)| (row, cols, url.clone())),
            );
        }
        let count = wrapped.len();
        for (r, row) in wrapped.into_iter().enumerate() {
            let (row, lead) = trim_row(row);
            let width = span_cols(&row.spans);
            // A row a run of spaces filled is dropped, unless it is all the line has.
            if width == 0 && count > 1 {
                continue;
            }
            let at = rows.len();
            for (_, cols, url) in parts.iter().filter(|(pr, _, _)| *pr == r) {
                let from = cols.start.saturating_sub(lead).min(width);
                let to = cols.end.saturating_sub(lead).min(width);
                if from < to {
                    links.push((at, from..to, url.clone()));
                }
            }
            rows.push(row);
        }
    }
    (rows, links)
}

/// Each column's width when the table, borders included, fits `avail` columns: its widest
/// cell, or when that is too wide, a cap lowered on the widest columns until it fits, with the
/// columns left over handed back to the capped ones. A shrunk column keeps at least its longest
/// word, columns with shorter words first, while the pane has room for it; the rest keep
/// `MIN_COLUMN`, and their words break. `None` when even `MIN_COLUMN` columns of text per
/// column do not fit.
fn column_widths(table: &TableBuf, avail: usize) -> Option<Vec<usize>> {
    let count = table.columns();
    if count == 0 {
        return None;
    }
    let mut widths = vec![1; count];
    let mut words = vec![0; count];
    for row in std::iter::once(&table.header).chain(&table.rows) {
        for (i, cell) in row.iter().enumerate().take(count) {
            widths[i] = widths[i].max(cell.width());
            words[i] = words[i].max(cell.longest_word());
        }
    }
    // `│ ` before each cell, a space after it, and the closing `│`.
    let budget = avail.checked_sub(3 * count + 1)?;
    if widths.iter().sum::<usize>() <= budget {
        return Some(widths);
    }
    if budget < MIN_COLUMN * count {
        return None;
    }
    let mut floors: Vec<usize> = widths.iter().map(|w| (*w).min(MIN_COLUMN)).collect();
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by_key(|i| words[*i]);
    for i in order {
        let word = words[i].min(widths[i]).max(floors[i]);
        if floors.iter().sum::<usize>() - floors[i] + word <= budget {
            floors[i] = word;
        }
    }
    let fit = |cap: usize| -> Vec<usize> {
        widths
            .iter()
            .zip(&floors)
            .map(|(w, floor)| (*w).min(cap).max(*floor))
            .collect()
    };
    let mut cap = widths.iter().copied().max().unwrap_or(1);
    while fit(cap).iter().sum::<usize>() > budget {
        cap -= 1;
    }
    let mut fitted = fit(cap);
    let mut spare = budget - fitted.iter().sum::<usize>();
    while spare > 0 {
        let before = spare;
        for (fit, natural) in fitted.iter_mut().zip(&widths) {
            if spare > 0 && *natural > *fit {
                *fit += 1;
                spare -= 1;
            }
        }
        if spare == before {
            break;
        }
    }
    Some(fitted)
}

/// One table row as lines: every cell wrapped to its column and padded per its alignment, and
/// the row as tall as its tallest cell.
fn grid_row(cells: &[Cell], widths: &[usize], alignments: &[Alignment]) -> Vec<TableLine> {
    let border = Style::new().fg(Color::DarkGray);
    let empty = Cell::default();
    let wrapped: Vec<_> = widths
        .iter()
        .enumerate()
        .map(|(i, width)| wrap_cell(cells.get(i).unwrap_or(&empty), *width))
        .collect();
    let height = wrapped
        .iter()
        .map(|(rows, _)| rows.len())
        .max()
        .unwrap_or(1);
    (0..height)
        .map(|r| {
            let mut out = TableLine::default();
            let mut at = 0;
            for (i, (rows, links)) in wrapped.iter().enumerate() {
                out.spans.push(Span::styled("│ ", border));
                at += 2;
                let line = rows.get(r).cloned().unwrap_or_default();
                let pad = widths[i].saturating_sub(span_cols(&line.spans));
                let (before, after) = match alignments.get(i) {
                    Some(Alignment::Right) => (pad, 0),
                    Some(Alignment::Center) => (pad / 2, pad - pad / 2),
                    _ => (0, pad),
                };
                if before > 0 {
                    out.spans.push(Span::raw(" ".repeat(before)));
                }
                let start = at + before;
                out.links.extend(
                    links
                        .iter()
                        .filter(|(row, _, _)| *row == r)
                        .map(|(_, cols, url)| (start + cols.start..start + cols.end, url.clone())),
                );
                out.spans.extend(line.spans);
                out.spans.push(Span::raw(" ".repeat(after + 1)));
                at += widths[i] + 1;
            }
            out.spans.push(Span::styled("│", border));
            out
        })
        .collect()
}

/// The table in box-drawing borders, its columns `widths` wide: a rule above, the header, a
/// rule under it, the body rows, and a rule below.
fn grid(table: &TableBuf, widths: &[usize]) -> Vec<TableLine> {
    let border = Style::new().fg(Color::DarkGray);
    let rule = |left: &str, mid: &str, right: &str| {
        let runs: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
        TableLine {
            spans: vec![Span::styled(
                format!("{left}{}{right}", runs.join(mid)),
                border,
            )],
            links: Vec::new(),
        }
    };
    let mut out = vec![rule("┌", "┬", "┐")];
    out.extend(grid_row(&table.header, widths, &table.alignments));
    out.push(rule("├", "┼", "┤"));
    for row in &table.rows {
        out.extend(grid_row(row, widths, &table.alignments));
    }
    out.push(rule("└", "┴", "┘"));
    out
}

/// The table as one block per body row, each cell on its own line as `header: value`, for a
/// pane too narrow for its columns. Blocks are separated by a blank line, and a blank header
/// reads `Column N`.
fn stacked(table: &TableBuf, avail: usize) -> Vec<TableLine> {
    let width = avail.max(2);
    let mut out = Vec::new();
    if table.rows.is_empty() {
        for header in &table.header {
            let (rows, _) = wrap_cell(header, width);
            out.extend(rows.into_iter().map(|row| TableLine {
                spans: row.spans,
                links: Vec::new(),
            }));
        }
        return out;
    }
    for (n, row) in table.rows.iter().enumerate() {
        if n > 0 {
            out.push(TableLine::default());
        }
        for (i, cell) in row.iter().enumerate() {
            let mut spans = match table.header.get(i) {
                Some(h) if span_cols(&h.spans) > 0 => h.spans.clone(),
                _ => vec![Span::styled(
                    format!("Column {}", i + 1),
                    Style::new().add_modifier(Modifier::BOLD),
                )],
            };
            spans.push(Span::raw(": "));
            let offset = spans.len();
            spans.extend(cell.spans.iter().cloned());
            let labeled = Cell {
                spans,
                links: cell
                    .links
                    .iter()
                    .map(|(start, end, url)| (start + offset, end + offset, url.clone()))
                    .collect(),
                breaks: cell.breaks.iter().map(|at| at + offset).collect(),
            };
            let (rows, links) = wrap_cell(&labeled, width);
            for (r, row) in rows.into_iter().enumerate() {
                out.push(TableLine {
                    spans: row.spans,
                    links: links
                        .iter()
                        .filter(|(lr, _, _)| *lr == r)
                        .map(|(_, cols, url)| (cols.clone(), url.clone()))
                        .collect(),
                });
            }
        }
    }
    out
}

struct Builder {
    out: Rendered,
    current: Vec<Span<'static>>,
    styles: Vec<Style>,
    list_depth: usize,
    /// Per open list, `Some(next number)` for an ordered list or `None` for an unordered one.
    list_counters: Vec<Option<u64>>,
    quote_depth: usize,
    code: Option<(String, String)>,
    /// The pane width tables are laid out to fit.
    width: u16,
    /// The table being read, from its start to its end.
    table: Option<TableBuf>,
    /// The open link and the index in `current` where its text starts on this line.
    link: Option<(Target, usize)>,
    /// How many links the text has opened so far, which numbers the next one.
    links_opened: usize,
    /// Links on the current line, as span index ranges into `current`, placed by `flush`.
    line_links: Vec<(usize, usize, Target)>,
    /// Span indices in `current` where a `<br>` starts a new line in the table cell being read.
    cell_breaks: Vec<usize>,
}

impl Builder {
    fn style(&self) -> Style {
        self.styles
            .iter()
            .fold(Style::new(), |acc, s| acc.patch(*s))
    }

    fn flush(&mut self) {
        // A link still open at a line break goes on from the start of the next line.
        if let Some((url, start)) = self.link.as_mut() {
            if self.current.len() > *start {
                self.line_links
                    .push((*start, self.current.len(), url.clone()));
            }
            *start = 0;
        }
        if self.current.is_empty() {
            self.line_links.clear();
            return;
        }
        let mut spans = Vec::new();
        if self.quote_depth > 0 {
            spans.push(Span::styled(
                "│ ".repeat(self.quote_depth),
                Style::new().fg(Color::DarkGray),
            ));
        }
        let offset = spans.len();
        spans.append(&mut self.current);
        let line = self.out.lines.len();
        let width =
            |spans: &[Span]| -> usize { spans.iter().map(|s| cells_width(&s.content)).sum() };
        for (start, end, target) in self.line_links.drain(..) {
            let from = width(&spans[..offset + start]);
            let to = from + width(&spans[offset + start..offset + end]);
            self.out.links.push(Link {
                line,
                cols: from..to,
                url: target.url,
                id: target.id,
            });
        }
        self.out.lines.push(Line::from(spans));
    }

    fn blank(&mut self) {
        self.flush();
        if self.out.lines.last().is_some_and(|l| !l.spans.is_empty()) {
            self.out.lines.push(Line::default());
        }
    }

    fn finish_code(&mut self) {
        let Some((lang, code)) = self.code.take() else {
            return;
        };
        let start = self.out.lines.len();
        let mut lines = highlight::highlight(&code, &lang).unwrap_or_else(|| {
            code.lines()
                .map(|l| Line::from(Span::styled(l.to_owned(), Style::new().fg(Color::Gray))))
                .collect()
        });
        if self.quote_depth > 0 {
            let marker = "│ ".repeat(self.quote_depth);
            for line in &mut lines {
                line.spans.insert(
                    0,
                    Span::styled(marker.clone(), Style::new().fg(Color::DarkGray)),
                );
            }
        }
        self.out.lines.extend(lines);
        let end = self.out.lines.len();
        self.out.code_blocks.push(CodeBlock { start, end, code });
    }

    /// Lays out the table just read to the pane, inside any quote marker, and records the
    /// links in its cells.
    fn finish_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        let marker = "│ ".repeat(self.quote_depth);
        let indent = cells_width(&marker);
        let avail = usize::from(self.width).saturating_sub(indent);
        let lines = match column_widths(&table, avail) {
            Some(widths) => grid(&table, &widths),
            None => stacked(&table, avail),
        };
        for table_line in lines {
            let line = self.out.lines.len();
            let mut spans = Vec::new();
            if self.quote_depth > 0 {
                spans.push(Span::styled(
                    marker.clone(),
                    Style::new().fg(Color::DarkGray),
                ));
            }
            spans.extend(table_line.spans);
            for (cols, target) in table_line.links {
                self.out.links.push(Link {
                    line,
                    cols: cols.start + indent..cols.end + indent,
                    url: target.url,
                    id: target.id,
                });
            }
            self.out.lines.push(Line::from(spans));
        }
    }
}

/// Whether `drawable` leaves `c` out: a control character other than a newline or a tab, or
/// a format character.
fn hidden(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t') || scuttle_core::text::is_format(c)
}

/// `text` without the control characters ratatui drops when it draws, other than newlines
/// and tabs, and without format characters, so the widths, link columns, and table borders
/// measured from it match the screen, and a copy holds what was shown. A lone carriage
/// return stays a line ending. Every text from the server that the transcript draws goes
/// through here, and so does every copy.
pub fn drawable(text: &str) -> Cow<'_, str> {
    if !text.chars().any(hidden) {
        return Cow::Borrowed(text);
    }
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    Cow::Owned(text.chars().filter(|&c| !hidden(c)).collect())
}

/// `text` as styled lines, with any table laid out to fit `width` columns.
pub fn render(text: &str, width: u16) -> Rendered {
    let text = drawable(text);
    let text = text.as_ref();
    let mut b = Builder {
        out: Rendered::default(),
        current: Vec::new(),
        styles: Vec::new(),
        list_depth: 0,
        list_counters: Vec::new(),
        quote_depth: 0,
        code: None,
        width,
        table: None,
        link: None,
        line_links: Vec::new(),
        links_opened: 0,
        cell_breaks: Vec::new(),
    };
    for event in Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES) {
        if let Some((_, code)) = b.code.as_mut() {
            match event {
                Event::Text(t) => {
                    code.push_str(&t);
                    continue;
                }
                Event::End(TagEnd::CodeBlock) => {
                    b.finish_code();
                    b.blank();
                    continue;
                }
                _ => continue,
            }
        }
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                b.blank();
                let color = if level == HeadingLevel::H1 {
                    Color::Cyan
                } else {
                    Color::Blue
                };
                b.styles
                    .push(Style::new().fg(color).add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Heading(_)) => {
                b.styles.pop();
                b.blank();
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => b.blank(),
            Event::Start(Tag::Strong) => b.styles.push(Style::new().add_modifier(Modifier::BOLD)),
            Event::Start(Tag::Emphasis) => {
                b.styles.push(Style::new().add_modifier(Modifier::ITALIC))
            }
            Event::Start(Tag::Strikethrough) => b
                .styles
                .push(Style::new().add_modifier(Modifier::CROSSED_OUT)),
            Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough) => {
                b.styles.pop();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                b.flush();
                b.quote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                b.flush();
                b.quote_depth -= 1;
            }
            Event::Start(Tag::List(start)) => {
                b.flush();
                b.list_depth += 1;
                b.list_counters.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                b.flush();
                b.list_depth -= 1;
                b.list_counters.pop();
                if b.list_depth == 0 {
                    b.blank();
                }
            }
            Event::Start(Tag::Item) => {
                b.flush();
                let indent = "  ".repeat(b.list_depth.saturating_sub(1));
                let marker = if let Some(Some(n)) = b.list_counters.last_mut() {
                    let marker = format!("{n}. ");
                    *n += 1;
                    marker
                } else {
                    "• ".to_owned()
                };
                b.current.push(Span::raw(format!("{indent}{marker}")));
            }
            Event::End(TagEnd::Item) => b.flush(),
            Event::Start(Tag::Table(alignments)) => {
                b.flush();
                b.table = Some(TableBuf {
                    alignments,
                    header: Vec::new(),
                    rows: Vec::new(),
                    row: Vec::new(),
                });
            }
            Event::End(TagEnd::Table) => {
                b.finish_table();
                b.blank();
            }
            Event::Start(Tag::TableHead) => {
                b.styles.push(Style::new().add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::TableHead) => {
                b.styles.pop();
                if let Some(table) = b.table.as_mut() {
                    table.header = std::mem::take(&mut table.row);
                }
            }
            Event::Start(Tag::TableRow) => {}
            Event::End(TagEnd::TableRow) => {
                if let Some(table) = b.table.as_mut() {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push(row);
                }
            }
            Event::Start(Tag::TableCell) => {
                b.current.clear();
                b.line_links.clear();
                b.cell_breaks.clear();
            }
            Event::End(TagEnd::TableCell) => {
                let cell = Cell {
                    spans: std::mem::take(&mut b.current),
                    links: std::mem::take(&mut b.line_links),
                    breaks: std::mem::take(&mut b.cell_breaks),
                };
                if let Some(table) = b.table.as_mut() {
                    table.row.push(cell);
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                b.flush();
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => {
                        l.split_whitespace().next().unwrap_or_default().to_owned()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                b.code = Some((lang, String::new()));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                b.styles
                    .push(Style::new().add_modifier(Modifier::UNDERLINED));
                let target = Target {
                    url: dest_url.to_string(),
                    id: b.links_opened,
                };
                b.links_opened += 1;
                b.link = Some((target, b.current.len()));
            }
            Event::End(TagEnd::Link) => {
                b.styles.pop();
                if let Some((url, start)) = b.link.take()
                    && b.current.len() > start
                {
                    b.line_links.push((start, b.current.len(), url));
                }
            }
            Event::Code(c) => {
                let mut style = Style::new().fg(Color::Yellow);
                if b.link.is_some() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                b.current.push(Span::styled(c.to_string(), style));
            }
            Event::Text(t) => {
                let style = b.style();
                b.current.push(Span::styled(t.to_string(), style));
            }
            Event::InlineHtml(html) if b.table.is_some() && is_br(&html) => {
                b.cell_breaks.push(b.current.len());
            }
            Event::SoftBreak => b.current.push(Span::raw(" ")),
            Event::HardBreak => b.flush(),
            Event::Rule => {
                b.flush();
                b.out.lines.push(Line::from(Span::styled(
                    "─".repeat(20),
                    Style::new().fg(Color::DarkGray),
                )));
            }
            _ => {}
        }
    }
    b.finish_code();
    b.flush();
    while b.out.lines.last().is_some_and(|l| l.spans.is_empty()) {
        b.out.lines.pop();
    }
    b.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;
    use unicode_width::UnicodeWidthStr;

    fn plain(r: &Rendered) -> Vec<String> {
        r.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn renders_headings_emphasis_and_lists() {
        let r = render("# Title\n\nSome **bold** and `code`.\n\n- one\n- two\n", 80);
        let text = plain(&r);
        assert_eq!(text[0], "Title");
        assert!(
            r.lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(text.iter().any(|l| l == "Some bold and code."));
        assert!(text.iter().any(|l| l == "• one"));
        assert!(text.iter().any(|l| l == "• two"));
    }

    #[test]
    fn control_characters_in_model_text_are_dropped_before_measuring() {
        let r = render(
            "ring\x07 the \x1b[31mbell [docs](https://example.com) now\n\n\
             | Name | Note |\n|---|---|\n| a\x1b | b\x07 |\n| long name | c |\n\n\
             ```\nmake\x1b[0m\tall\n```\n",
            80,
        );
        let text = plain(&r);
        for line in &text {
            assert!(
                !line.chars().any(|c| c.is_control() && c != '\t'),
                "a control character other than a tab is left in {line:?}"
            );
        }
        assert_eq!(text[0], "ring the [31mbell docs now");
        let at = text[0].find("docs").unwrap();
        assert_eq!(
            r.links[0].cols,
            at..at + 4,
            "the link's columns are where it draws"
        );
        let table: Vec<&String> = text.iter().filter(|l| l.contains('│')).collect();
        assert_eq!(table.len(), 3, "{text:#?}");
        let widths: Vec<usize> = table.iter().map(|l| l.width()).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "the right border is straight: {text:#?}"
        );
        assert_eq!(
            r.code_blocks[0].code, "make[0m\tall\n",
            "the copied code is clean"
        );
    }

    #[test]
    fn a_crlf_is_one_line_ending_and_a_lone_cr_becomes_one() {
        assert_eq!(drawable("a\r\nb"), "a\nb");
        assert_eq!(drawable("a\rb"), "a\nb");
        assert_eq!(drawable("a\r\n\r\nb"), "a\n\nb");
        assert_eq!(plain(&render("a\r\nb", 80)), ["a b"], "a soft break");
        assert_eq!(plain(&render("a\rb", 80)), ["a b"], "a soft break");
        assert_eq!(
            plain(&render("a\r\n\r\nb", 80)),
            ["a", "", "b"],
            "two CRLFs end a paragraph"
        );
    }

    #[test]
    fn records_code_block_ranges_and_source() {
        let r = render(
            "before\n\n```rust\nfn main() {}\nlet x = 1;\n```\n\nafter\n",
            80,
        );
        assert_eq!(r.code_blocks.len(), 1);
        let block = &r.code_blocks[0];
        assert_eq!(block.code, "fn main() {}\nlet x = 1;\n");
        assert_eq!(block.end - block.start, 2);
        assert_eq!(plain(&r)[block.start], "fn main() {}");
    }

    #[test]
    fn unterminated_code_fence_while_streaming_is_still_a_block() {
        let r = render("```\npartial", 80);
        assert_eq!(r.code_blocks.len(), 1);
        assert_eq!(r.code_blocks[0].code.trim_end(), "partial");
    }

    /// The display columns of every `│` on `line`.
    fn bars(line: &str) -> Vec<usize> {
        line.char_indices()
            .filter(|(_, c)| *c == '│')
            .map(|(i, _)| line[..i].width())
            .collect()
    }

    #[test]
    fn tables_draw_aligned_columns_inside_box_borders() {
        let r = render("| a | bb |\n| --- | ---: |\n| ccc | 1 |\n", 80);
        assert_eq!(
            plain(&r),
            [
                "┌─────┬────┐",
                "│ a   │ bb │",
                "├─────┼────┤",
                "│ ccc │  1 │",
                "└─────┴────┘",
            ]
        );
        let header = r.lines[1].spans.iter().find(|s| s.content == "a").unwrap();
        assert!(header.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn a_table_too_wide_for_the_pane_shrinks_its_widest_column_and_wraps_inside_it() {
        let r = render(
            "| name | description |\n| --- | --- |\n| x | the quick brown fox jumps over the lazy dog |\n",
            30,
        );
        let text = plain(&r);
        assert!(text.iter().all(|l| l.width() == 30), "{text:#?}");
        assert!(
            text.len() > 5,
            "the long cell wrapped onto more rows: {text:#?}"
        );
        assert!(
            text.iter().any(|l| l.contains("the quick brown")),
            "{text:#?}"
        );
        assert!(
            text.iter().any(|l| l.contains("fox jumps over the")),
            "{text:#?}"
        );
        assert!(text.iter().any(|l| l.contains("lazy dog")), "{text:#?}");
        let body: Vec<&String> = text.iter().filter(|l| l.starts_with('│')).collect();
        assert!(
            body.windows(2).all(|w| bars(w[0]) == bars(w[1])),
            "{text:#?}"
        );
    }

    #[test]
    fn a_long_unbroken_cell_and_wide_characters_stay_inside_their_columns() {
        let url = "https://example.com/a/very/long/path/that/never/breaks";
        let r = render(
            &format!("| 名前 | url |\n| --- | --- |\n| 中文字符 | {url} |\n"),
            32,
        );
        let text = plain(&r);
        assert!(text.iter().all(|l| l.width() <= 32), "{text:#?}");
        let body: Vec<&String> = text.iter().filter(|l| l.starts_with('│')).collect();
        assert!(body.iter().all(|l| l.ends_with('│')), "{text:#?}");
        assert!(
            body.windows(2).all(|w| bars(w[0]) == bars(w[1])),
            "{text:#?}"
        );
        let joined: String = text[3..]
            .iter()
            .flat_map(|l| l.chars())
            .filter(|c| *c != '│' && *c != ' ')
            .collect();
        assert!(
            joined.contains(&format!("中文字符{url}")),
            "nothing was cut: {joined}"
        );
    }

    #[test]
    fn below_the_minimum_a_table_stacks_its_rows_as_header_value_lines() {
        let r = render(
            "| a | b | c |\n| - | - | - |\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |\n",
            12,
        );
        assert_eq!(
            plain(&r),
            ["a: 1", "b: 2", "c: 3", "", "a: 4", "b: 5", "c: 6"]
        );
    }

    #[test]
    fn a_link_in_a_table_cell_is_located_inside_its_column() {
        let r = render(
            "| site |\n| --- |\n| [docs](https://coder.com/docs) |\n",
            80,
        );
        assert_eq!(plain(&r)[3], "│ docs │");
        assert_eq!(
            r.links,
            [Link {
                line: 3,
                cols: 2..6,
                url: "https://coder.com/docs".into(),
                id: 0
            }]
        );
    }

    #[test]
    fn every_part_of_one_link_shares_its_id_and_other_links_have_their_own() {
        let r = render(
            "| head | note |\n| --- | --- |\n| [cargo test](https://x.example) | [ok](https://y.example) |\n",
            20,
        );
        let parts: Vec<(usize, &str, usize)> = r
            .links
            .iter()
            .map(|l| (l.line, l.url.as_str(), l.id))
            .collect();
        assert_eq!(
            parts,
            [
                (3, "https://x.example", 0),
                (3, "https://y.example", 1),
                (4, "https://x.example", 0),
            ],
            "{:#?}",
            plain(&r)
        );
        let r = render(
            "[one  \ntwo](https://coder.com) and [three](https://coder.com)\n",
            80,
        );
        let ids: Vec<usize> = r.links.iter().map(|l| l.id).collect();
        assert_eq!(ids, [0, 0, 1], "two links to one URL stay apart");
    }

    #[test]
    fn wrapped_cell_rows_never_start_with_a_space_and_links_cover_only_their_text() {
        let r = render(
            "| head |\n| --- |\n| [cargo test](https://x.example) |\n",
            9,
        );
        assert_eq!(
            plain(&r),
            [
                "┌───────┐",
                "│ head  │",
                "├───────┤",
                "│ cargo │",
                "│ test  │",
                "└───────┘",
            ]
        );
        let parts: Vec<(usize, Range<usize>)> =
            r.links.iter().map(|l| (l.line, l.cols.clone())).collect();
        assert_eq!(parts, [(3, 2..7), (4, 2..6)]);
    }

    #[test]
    fn shrunk_columns_keep_their_longest_word_whole_when_the_pane_fits_it() {
        let r = render(
            "| File | Function | Change | Test | Status | Notes |\n\
             | --- | --- | --- | --- | --- | --- |\n\
             | markdown.rs | render | lays tables out to the pane width | grid_fits | passing | see the review notes |\n",
            60,
        );
        let text = plain(&r);
        assert!(text.iter().all(|l| l.width() <= 60), "{text:#?}");
        for word in ["Function", "Change", "grid_fits", "passing", "review"] {
            assert!(text.iter().any(|l| l.contains(word)), "{word}: {text:#?}");
        }
        let r = render(
            "| Id | Example | Note |\n| --- | --- | --- |\n| 1 | an example of a long cell | ok fine |\n",
            24,
        );
        let text = plain(&r);
        assert!(text.iter().all(|l| l.width() <= 24), "{text:#?}");
        assert!(text[1].contains("Example"), "{text:#?}");
    }

    #[test]
    fn a_br_tag_breaks_the_line_inside_its_cell() {
        let r = render(
            "| a | b |\n| --- | --- |\n| one<br>two | x<br/>y<BR />z |\n",
            80,
        );
        assert_eq!(
            plain(&r),
            [
                "┌─────┬───┐",
                "│ a   │ b │",
                "├─────┼───┤",
                "│ one │ x │",
                "│ two │ y │",
                "│     │ z │",
                "└─────┴───┘",
            ]
        );
    }

    #[test]
    fn a_centered_column_splits_its_padding() {
        let r = render("| a | b |\n| :-: | --- |\n| wide | x |\n", 80);
        assert_eq!(
            plain(&r),
            [
                "┌──────┬───┐",
                "│  a   │ b │",
                "├──────┼───┤",
                "│ wide │ x │",
                "└──────┴───┘",
            ]
        );
    }

    #[test]
    fn ragged_rows_fill_missing_cells_and_drop_extra_ones() {
        let r = render(
            "| a | b | c |\n| --- | --- | --- |\n| 1 |\n| 1 | 2 | 3 | 4 | 5 |\n",
            80,
        );
        assert_eq!(
            plain(&r),
            [
                "┌───┬───┬───┐",
                "│ a │ b │ c │",
                "├───┼───┼───┤",
                "│ 1 │   │   │",
                "│ 1 │ 2 │ 3 │",
                "└───┴───┴───┘",
            ]
        );
    }

    #[test]
    fn a_table_in_a_blockquote_fits_inside_the_marker() {
        let r = render(
            "> | name |\n> | --- |\n> | [docs](https://coder.com/docs) and more |\n",
            14,
        );
        let text = plain(&r);
        assert!(text.iter().all(|l| l.width() <= 14), "{text:#?}");
        assert!(text.iter().all(|l| l.starts_with("│ ")), "{text:#?}");
        assert_eq!(text[3], "│ │ docs     │", "{text:#?}");
        assert_eq!(text[4], "│ │ and more │", "{text:#?}");
        assert_eq!(
            r.links,
            [Link {
                line: 3,
                cols: 4..8,
                url: "https://coder.com/docs".into(),
                id: 0
            }]
        );
    }

    #[test]
    fn a_link_in_a_stacked_table_is_located_after_its_label() {
        let r = render(
            "| a | b | c |\n| - | - | - |\n| 1 | [go](https://g.example) | 3 |\n",
            12,
        );
        assert_eq!(plain(&r), ["a: 1", "b: go", "c: 3"]);
        assert_eq!(
            r.links,
            [Link {
                line: 1,
                cols: 3..5,
                url: "https://g.example".into(),
                id: 0
            }]
        );
    }

    #[test]
    fn render_cached_lays_a_table_out_for_each_width() {
        let text =
            "| name | note |\n| --- | --- |\n| a | the quick brown fox jumps over the lazy dog |\n";
        let wide = render_cached(text, 80);
        let narrow = render_cached(text, 30);
        assert!(plain(&narrow).iter().all(|l| l.width() <= 30));
        assert!(narrow.lines.len() > wide.lines.len());
        assert_eq!(plain(&render_cached(text, 80)), plain(&wide));
    }

    #[test]
    fn code_inside_blockquote_keeps_the_marker() {
        let r = render("> ```\n> fn main() {}\n> ```\n", 80);
        let text = plain(&r);
        assert!(text.iter().any(|l| l == "│ fn main() {}"));
    }

    #[test]
    fn numbers_ordered_lists() {
        let r = render("1. one\n2. two\n3. three\n", 80);
        let text = plain(&r);
        assert!(text.iter().any(|l| l == "1. one"));
        assert!(text.iter().any(|l| l == "2. two"));
        assert!(text.iter().any(|l| l == "3. three"));
    }

    #[test]
    fn render_cached_returns_the_same_lines_as_render() {
        let text = "# Cached\n\nSome **bold** text.\n\n```rust\nfn f() {}\n```\n";
        let direct = render(text, 80);
        let cached_first = render_cached(text, 80);
        let cached_second = render_cached(text, 80);
        assert_eq!(plain(&cached_first), plain(&direct));
        assert_eq!(plain(&cached_second), plain(&direct));
        assert_eq!(cached_first.code_blocks, direct.code_blocks);
    }

    #[test]
    fn links_are_underlined_and_located() {
        let r = render(
            "See [the docs](https://coder.com/docs) now.\n\n> quoted [x](https://x.example)\n",
            80,
        );
        let text = plain(&r);
        assert_eq!(text[0], "See the docs now.");
        assert_eq!(
            r.links[0],
            Link {
                line: 0,
                cols: 4..12,
                url: "https://coder.com/docs".into(),
                id: 0
            }
        );
        let docs = r.lines[0]
            .spans
            .iter()
            .find(|s| s.content == "the docs")
            .unwrap();
        assert!(docs.style.add_modifier.contains(Modifier::UNDERLINED));
        let quoted = text.iter().position(|l| l.contains("quoted")).unwrap();
        assert_eq!(
            r.links[1],
            Link {
                line: quoted,
                cols: 9..10,
                url: "https://x.example".into(),
                id: 1
            },
            "columns count the quote marker"
        );
    }

    #[test]
    fn a_link_split_by_a_hard_break_is_located_on_both_lines() {
        let r = render("[one  \ntwo](https://coder.com)\n", 80);
        assert_eq!(plain(&r), ["one", "two"]);
        let lines: Vec<(usize, std::ops::Range<usize>)> =
            r.links.iter().map(|l| (l.line, l.cols.clone())).collect();
        assert_eq!(lines, [(0, 0..3), (1, 0..3)]);
    }

    #[test]
    fn code_is_never_a_link() {
        let r = render("```\n[the docs](https://coder.com/docs)\n```\n", 80);
        assert!(r.links.is_empty());
        let r = render("[`code`](https://coder.com)", 80);
        assert_eq!(r.links.len(), 1, "inline code inside a link is part of it");
        assert!(
            r.lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
    }
}
