//! The shared table overlay: a filter line, grouped rows, a status line, and a hint.

use std::cell::{Cell, RefCell};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Table};
use unicode_width::UnicodeWidthStr;
use uuid::Uuid;

use crate::theme::Theme;
use crate::wrap::wrap_line;

/// The mark before the selected row.
const HIGHLIGHT: &str = "› ";

/// The columns the border takes from a box's width, one on each side.
const BORDER: usize = 2;

/// The columns a selectable row has in a box `width` wide: inside the border, and after the
/// selection mark.
pub fn item_room(width: u16) -> usize {
    usize::from(width).saturating_sub(BORDER + HIGHLIGHT.width())
}

/// The columns a line such as the hint has in a box `width` wide, inside the border.
pub fn line_room(width: u16) -> usize {
    usize::from(width).saturating_sub(BORDER)
}

/// What a row stands for, so the selection survives the rows being rebuilt.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowKey {
    Model(Uuid),
    Workspace(Option<Uuid>),
    Organization(Uuid),
    Chat(Uuid),
    Queued(i64),
    /// An organization MCP server in the `/mcp` panel.
    Mcp(Uuid),
    /// A file in the `/files` panel.
    File(Uuid),
    SearchAll,
    /// An action in a panel, named by its id.
    Action(&'static str),
    /// A row that stands for nothing, such as a group header.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Item,
    /// Shown dim but still selectable, so choosing it can say why it is refused, such as an
    /// organization where chats cannot be created.
    Dimmed,
    /// Shown dim and never selected.
    Disabled,
    /// A group title, never selected. A title of one cell spans every column, so a long one
    /// is never cut to the first column's width.
    Header,
    /// Plain text in a read-only panel, never selected.
    Text,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub key: RowKey,
    pub cells: Vec<Line<'static>>,
    pub kind: RowKind,
}

impl Row {
    /// A read-only label and value.
    pub fn text(cells: Vec<Line<'static>>) -> Row {
        Row {
            key: RowKey::None,
            cells,
            kind: RowKind::Text,
        }
    }

    pub fn item(key: RowKey, cells: Vec<Line<'static>>) -> Row {
        Row {
            key,
            cells,
            kind: RowKind::Item,
        }
    }

    pub fn dimmed(key: RowKey, cells: Vec<Line<'static>>) -> Row {
        Row {
            key,
            cells,
            kind: RowKind::Dimmed,
        }
    }

    pub fn disabled(key: RowKey, cells: Vec<Line<'static>>) -> Row {
        Row {
            key,
            cells,
            kind: RowKind::Disabled,
        }
    }

    pub fn header(cells: Vec<Line<'static>>) -> Row {
        Row {
            key: RowKey::None,
            cells,
            kind: RowKind::Header,
        }
    }

    pub fn selectable(&self) -> bool {
        !matches!(
            self.kind,
            RowKind::Disabled | RowKind::Header | RowKind::Text
        )
    }
}

/// A cell that shows the activity spinner at `offset` columns into cell `column` of row
/// `row`. The table paints the current frame there after drawing, as the transcript does, so
/// a timer frame can redraw the rows it already built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spinner {
    pub row: usize,
    pub column: usize,
    pub offset: u16,
}

#[derive(Debug, Clone, Default)]
pub struct TableView {
    pub title: String,
    pub widths: Vec<Constraint>,
    pub rows: Vec<Row>,
    /// Loading, empty, or error text: in place of the rows when there are none, else below.
    pub status: Option<String>,
    /// A dim line at the bottom, such as the keys the overlay takes.
    pub hint: Option<String>,
    /// Draws the hint in the error style, for a key that destroys something.
    pub hint_alarm: bool,
    /// Whether typing filters the rows, which shows the filter line.
    pub filterable: bool,
    /// What the filter line says before the filter text; `None` keeps the `> ` prompt.
    pub filter_label: Option<&'static str>,
    /// Dim text the filter line shows while the filter is empty.
    pub placeholder: Option<&'static str>,
    /// The cells that animate.
    pub spinners: Vec<Spinner>,
}

#[derive(Debug, Clone, Default)]
pub struct TableState {
    pub filter: String,
    /// The selected row's key; `None` selects the first selectable row.
    pub selected: Option<RowKey>,
    /// The drawn rows scrolled past, for a view with no selectable rows, such as `/info`.
    /// `render` reports the most it can be, and the owner clamps it to that.
    pub scroll: usize,
    /// The first drawn row the last frame showed. `render` starts the next frame from it, so
    /// moving the selection scrolls only once it leaves the rows on screen.
    pub offset: Cell<usize>,
    /// The selectable row above the selected one when it was last drawn or moved to, by key.
    /// When the selected row leaves the view, as an archived chat leaves `/chats`, the
    /// selection falls back to it instead of jumping to the top.
    pub above: RefCell<Option<RowKey>>,
    /// The row the selection fell back to once the selected row left the view, by key. It
    /// stands in for the selected row until the selection moves, so when it leaves in turn,
    /// the selection goes to the row above it rather than to the top.
    pub adopted: RefCell<Option<RowKey>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKey {
    /// Navigation or filter typing, already applied.
    Handled,
    Enter,
    Esc,
    /// A key the table does not use, for the overlay to handle.
    Unhandled,
}

impl TableState {
    pub fn with_selected(key: RowKey) -> TableState {
        TableState {
            selected: Some(key),
            ..TableState::default()
        }
    }

    /// The index of the selected row; else, once it left the view, of the row the selection
    /// adopted; else of the row that was above; else of the first selectable row.
    pub fn index(&self, view: &TableView) -> Option<usize> {
        let find = |key: &RowKey| {
            view.rows
                .iter()
                .position(|r| r.selectable() && &r.key == key)
        };
        let first = || view.rows.iter().position(Row::selectable);
        match self.selected.as_ref() {
            Some(key) => find(key)
                .or_else(|| self.adopted.borrow().as_ref().and_then(find))
                .or_else(|| self.above.borrow().as_ref().and_then(find))
                .or_else(first),
            None => first(),
        }
    }

    /// Remembers the selectable row above the selected one in `view`. Once the selected row
    /// is missing from `view`, the selection adopts the row it falls back to, and the row
    /// above that one is remembered instead. With no selection, or nothing in `view` to fall
    /// back to, it forgets both.
    pub fn remember_above(&self, view: &TableView) {
        let forget = || {
            *self.above.borrow_mut() = None;
            *self.adopted.borrow_mut() = None;
        };
        let Some(key) = self.selected.as_ref() else {
            forget();
            return;
        };
        let at = match view
            .rows
            .iter()
            .position(|r| r.selectable() && &r.key == key)
        {
            Some(at) => {
                *self.adopted.borrow_mut() = None;
                at
            }
            None => {
                let Some(at) = self.index(view) else {
                    forget();
                    return;
                };
                *self.adopted.borrow_mut() = Some(view.rows[at].key.clone());
                at
            }
        };
        *self.above.borrow_mut() = view.rows[..at]
            .iter()
            .rev()
            .find(|r| r.selectable())
            .map(|r| r.key.clone());
    }

    pub fn selected_row<'v>(&self, view: &'v TableView) -> Option<&'v Row> {
        self.index(view).map(|i| &view.rows[i])
    }

    fn step(&mut self, view: &TableView, delta: isize) {
        let selectable: Vec<usize> = view
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.selectable())
            .map(|(i, _)| i)
            .collect();
        let Some(current) = self.index(view) else {
            return;
        };
        let at = selectable.iter().position(|i| *i == current).unwrap_or(0) as isize;
        let next = (at + delta).clamp(0, selectable.len() as isize - 1) as usize;
        self.selected = Some(view.rows[selectable[next]].key.clone());
        self.remember_above(view);
    }

    /// Moves the selection, or scrolls when no row can be selected.
    fn move_by(&mut self, view: &TableView, delta: isize) {
        if view.rows.iter().any(Row::selectable) {
            self.step(view, delta);
        } else {
            self.scroll = self.scroll.saturating_add_signed(delta);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, view: &TableView) -> TableKey {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => return TableKey::Esc,
            KeyCode::Enter => return TableKey::Enter,
            KeyCode::Up => self.move_by(view, -1),
            KeyCode::Down => self.move_by(view, 1),
            KeyCode::PageUp => self.move_by(view, -10),
            KeyCode::PageDown => self.move_by(view, 10),
            KeyCode::Char(c) if view.filterable && plain => {
                self.filter.push(c);
                self.selected = None;
            }
            KeyCode::Backspace if view.filterable => {
                self.filter.pop();
                self.selected = None;
            }
            _ => return TableKey::Unhandled,
        }
        TableKey::Handled
    }
}

/// Whether `row` is a one-cell group title, drawn across every column.
fn spans(row: &Row) -> bool {
    row.kind == RowKind::Header && row.cells.len() == 1
}

/// The cell areas of `view.widths` in `body`, after the selection mark, as `Table` lays them
/// out (`Table::get_column_widths` in ratatui-widgets): from the left, one cell apart.
fn column_areas(body: Rect, mark: u16, widths: &[Constraint]) -> Vec<Rect> {
    let [_, columns] =
        Layout::horizontal([Constraint::Length(mark), Constraint::Fill(0)]).areas(body);
    Layout::horizontal(widths.to_vec())
        .flex(Flex::Start)
        .spacing(1)
        .split(columns)
        .to_vec()
}

/// The rows `row` is drawn as: one, or for a text row one per line of its cells wrapped to
/// their columns, so a long value continues below instead of being cut off.
fn drawn_rows(row: &Row, columns: &[Rect]) -> Vec<Vec<Line<'static>>> {
    if row.kind != RowKind::Text {
        return vec![row.cells.clone()];
    }
    let wrapped: Vec<Vec<Line<'static>>> = row
        .cells
        .iter()
        .enumerate()
        .map(|(i, cell)| match columns.get(i) {
            Some(column) => wrap_line(cell, column.width),
            None => vec![cell.clone()],
        })
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    (0..height)
        .map(|n| {
            wrapped
                .iter()
                .map(|lines| lines.get(n).cloned().unwrap_or_default())
                .collect()
        })
        .collect()
}

/// The rows `view` takes drawn at `width` columns with nothing scrolled: its border, filter,
/// status, and hint lines, and every row as `render` wraps it.
pub fn height(view: &TableView, width: u16) -> u16 {
    let inner = Rect::new(0, 0, width.saturating_sub(2), 1);
    let mark = if view.rows.iter().any(Row::selectable) {
        HIGHLIGHT.width() as u16
    } else {
        0
    };
    let columns = column_areas(inner, mark, &view.widths);
    let body: usize = if view.rows.is_empty() {
        1
    } else {
        view.rows
            .iter()
            .map(|r| drawn_rows(r, &columns).len())
            .sum()
    };
    let status_below = view.status.is_some() && !view.rows.is_empty();
    let lines = 2
        + usize::from(view.filterable)
        + body
        + usize::from(status_below)
        + usize::from(view.hint.is_some());
    u16::try_from(lines).unwrap_or(u16::MAX)
}

/// Draws `view` in a bordered box that clears what is under it, with the `spinner` frame, if
/// any, painted over every cell in `view.spinners`. Returns the most `state.scroll` can be.
pub fn render(
    f: &mut Frame,
    area: Rect,
    view: &TableView,
    state: &TableState,
    theme: &Theme,
    spinner: Option<&str>,
) -> usize {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", view.title));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let status_below = view.status.is_some() && !view.rows.is_empty();
    let [filter, body, status, hint] = Layout::vertical([
        Constraint::Length(u16::from(view.filterable)),
        Constraint::Min(1),
        Constraint::Length(u16::from(status_below)),
        Constraint::Length(u16::from(view.hint.is_some())),
    ])
    .areas(inner);
    if view.filterable {
        let mut spans = vec![Span::styled(
            view.filter_label.unwrap_or("> "),
            theme.accent,
        )];
        match view.placeholder.filter(|_| state.filter.is_empty()) {
            Some(placeholder) => spans.push(Span::styled(placeholder, theme.dim)),
            None => spans.push(Span::raw(state.filter.clone())),
        }
        f.render_widget(Paragraph::new(Line::from(spans)), filter);
    }
    let mut max_scroll = 0;
    if view.rows.is_empty() {
        state.remember_above(view);
        let text = match &view.status {
            Some(status) => status.clone(),
            None if !state.filter.trim().is_empty() => "No matches".to_owned(),
            None => String::new(),
        };
        f.render_widget(Paragraph::new(Span::styled(text, theme.dim)), body);
    } else {
        let index = state.index(view);
        state.remember_above(view);
        let mark = if index.is_some() {
            HIGHLIGHT.width() as u16
        } else {
            0
        };
        let columns = column_areas(body, mark, &view.widths);
        // Where each of `view.rows` starts among the drawn rows.
        let mut starts = Vec::with_capacity(view.rows.len());
        let mut rows = Vec::new();
        for r in &view.rows {
            let style = match r.kind {
                RowKind::Item | RowKind::Text => Style::default(),
                RowKind::Dimmed | RowKind::Disabled => theme.dim,
                RowKind::Header => theme.accent,
            };
            starts.push(rows.len());
            if spans(r) {
                // Painted across the columns once the table is drawn.
                rows.push(ratatui::widgets::Row::new(Vec::<Line>::new()).style(style));
                continue;
            }
            rows.extend(
                drawn_rows(r, &columns)
                    .into_iter()
                    .map(|cells| ratatui::widgets::Row::new(cells).style(style)),
            );
        }
        max_scroll = rows.len().saturating_sub(body.height as usize);
        let table = Table::new(rows, view.widths.clone())
            .row_highlight_style(theme.accent)
            .highlight_symbol(HIGHLIGHT);
        let mut widget_state = match index {
            Some(i) => ratatui::widgets::TableState::default()
                .with_offset(state.offset.get())
                .with_selected(Some(starts[i])),
            None => {
                ratatui::widgets::TableState::default().with_offset(state.scroll.min(max_scroll))
            }
        };
        f.render_stateful_widget(table, body, &mut widget_state);
        state.offset.set(widget_state.offset());
        let left = columns.first().map_or(body.x, |c| c.x);
        for (i, r) in view.rows.iter().enumerate().filter(|(_, r)| spans(r)) {
            let Some(line) = starts[i]
                .checked_sub(widget_state.offset())
                .filter(|line| *line < body.height as usize)
            else {
                continue;
            };
            let area = Rect {
                x: left,
                y: body.y + line as u16,
                width: body.right().saturating_sub(left),
                height: 1,
            };
            f.render_widget(Paragraph::new(r.cells[0].clone()).style(theme.accent), area);
        }
        if let Some(frame) = spinner {
            paint_spinners(
                f.buffer_mut(),
                body,
                view,
                &columns,
                &starts,
                &widget_state,
                frame,
            );
        }
        if let Some(text) = view.status.as_ref() {
            f.render_widget(
                Paragraph::new(Span::styled(text.clone(), theme.dim)),
                status,
            );
        }
    }
    if let Some(text) = view.hint.as_ref() {
        let style = if view.hint_alarm {
            theme.error
        } else {
            theme.dim
        };
        f.render_widget(Paragraph::new(Span::styled(text.clone(), style)), hint);
    }
    max_scroll
}

/// Sets `frame` in each spinner cell that `Table` drew into `body`, in the `columns` it was
/// laid out with, where `starts` maps each of `view.rows` to its first drawn row.
fn paint_spinners(
    buf: &mut Buffer,
    body: Rect,
    view: &TableView,
    columns: &[Rect],
    starts: &[usize],
    drawn: &ratatui::widgets::TableState,
    frame: &str,
) {
    for s in &view.spinners {
        let Some(line) = starts
            .get(s.row)
            .and_then(|row| row.checked_sub(drawn.offset()))
            .filter(|line| *line < body.height as usize)
        else {
            continue;
        };
        let Some(column) = columns.get(s.column).filter(|c| s.offset < c.width) else {
            continue;
        };
        if let Some(cell) = buf.cell_mut((column.x + s.offset, body.y + line as u16)) {
            cell.set_symbol(frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    fn view(rows: Vec<Row>) -> TableView {
        TableView {
            title: "Model".into(),
            widths: vec![Constraint::Fill(1)],
            rows,
            filterable: true,
            ..Default::default()
        }
    }

    fn press(state: &mut TableState, view: &TableView, code: KeyCode) -> TableKey {
        state.handle_key(KeyEvent::new(code, KeyModifiers::NONE), view)
    }

    fn selected(state: &TableState, view: &TableView) -> Option<RowKey> {
        state.selected_row(view).map(|r| r.key.clone())
    }

    fn draw(v: &TableView, s: &TableState, theme: &Theme, width: u16, height: u16) -> Buffer {
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| {
            render(f, f.area(), v, s, theme, None);
        })
        .unwrap();
        term.backend().buffer().clone()
    }

    #[test]
    fn a_labeled_filter_line_shows_its_placeholder_until_something_is_typed() {
        let mut v = view(vec![Row::item(RowKey::SearchAll, vec![Line::from("x")])]);
        v.filter_label = Some("Search: ");
        v.placeholder = Some("Type to filter");
        let theme = Theme::terminal(true);
        let shown = |s: &TableState| {
            let buf = draw(&v, s, &theme, 40, 6);
            (0..6)
                .map(|y| {
                    (0..40)
                        .map(|x| buf[(x, y)].symbol().to_owned())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let empty = shown(&TableState::default());
        assert!(empty.contains("Search: Type to filter"), "{empty}");
        let typed = shown(&TableState {
            filter: "abc".into(),
            ..Default::default()
        });
        assert!(typed.contains("Search: abc"), "{typed}");
        assert!(!typed.contains("Type to filter"), "{typed}");
    }

    #[test]
    fn moving_skips_disabled_rows_and_stops_at_the_ends() {
        let (a, off, b) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let v = view(vec![
            Row::disabled(RowKey::Model(off), vec![Line::from("first, but off")]),
            Row::item(RowKey::Model(a), vec![Line::from("A")]),
            Row::disabled(RowKey::Model(off), vec![Line::from("off")]),
            Row::item(RowKey::Model(b), vec![Line::from("B")]),
        ]);
        let mut s = TableState::default();
        assert_eq!(selected(&s, &v), Some(RowKey::Model(a)));
        press(&mut s, &v, KeyCode::Down);
        assert_eq!(selected(&s, &v), Some(RowKey::Model(b)));
        press(&mut s, &v, KeyCode::Down);
        assert_eq!(selected(&s, &v), Some(RowKey::Model(b)));
        press(&mut s, &v, KeyCode::Up);
        press(&mut s, &v, KeyCode::Up);
        assert_eq!(selected(&s, &v), Some(RowKey::Model(a)));
    }

    #[test]
    fn text_rows_are_never_selected() {
        let v = view(vec![
            Row::text(vec![Line::from("Title"), Line::from("explore")]),
            Row::text(vec![Line::from("ID"), Line::from("1")]),
        ]);
        let mut s = TableState::default();
        assert_eq!(selected(&s, &v), None);
        press(&mut s, &v, KeyCode::Down);
        assert_eq!(selected(&s, &v), None);
    }

    #[test]
    fn moving_skips_group_headers() {
        let a = Uuid::new_v4();
        let v = view(vec![
            Row::header(vec![Line::from("Anthropic")]),
            Row::item(RowKey::Model(a), vec![Line::from("A")]),
        ]);
        let mut s = TableState::default();
        press(&mut s, &v, KeyCode::Up);
        assert_eq!(selected(&s, &v), Some(RowKey::Model(a)));
    }

    #[test]
    fn dimmed_rows_are_drawn_dim_and_can_still_be_chosen() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let v = view(vec![
            Row::item(RowKey::Organization(a), vec![Line::from("Open")]),
            Row::dimmed(RowKey::Organization(b), vec![Line::from("Shut")]),
        ]);
        let mut s = TableState::default();
        press(&mut s, &v, KeyCode::Down);
        assert_eq!(selected(&s, &v), Some(RowKey::Organization(b)));
        press(&mut s, &v, KeyCode::Up);
        let theme = Theme::terminal(true);
        let buf = draw(&v, &s, &theme, 30, 5);
        let row = (0..5u16)
            .find(|&y| (0..30u16).any(|x| buf[(x, y)].symbol() == "S"))
            .unwrap();
        let x = (0..30u16).find(|&x| buf[(x, row)].symbol() == "S").unwrap();
        assert_eq!(Some(buf[(x, row)].fg), theme.dim.fg, "the row is dimmed");
    }

    #[test]
    fn the_selection_follows_its_row_when_the_rows_change() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut s = TableState::with_selected(RowKey::Model(b));
        let reordered = view(vec![
            Row::item(RowKey::Model(b), vec![Line::from("B")]),
            Row::item(RowKey::Model(a), vec![Line::from("A")]),
        ]);
        assert_eq!(s.index(&reordered), Some(0));
        press(&mut s, &reordered, KeyCode::Down);
        assert_eq!(selected(&s, &reordered), Some(RowKey::Model(a)));
    }

    #[test]
    fn typing_edits_the_filter_and_enter_and_esc_go_to_the_caller() {
        let v = view(vec![]);
        let mut s = TableState::default();
        assert_eq!(press(&mut s, &v, KeyCode::Char('s')), TableKey::Handled);
        assert_eq!(press(&mut s, &v, KeyCode::Char('o')), TableKey::Handled);
        assert_eq!(s.filter, "so");
        press(&mut s, &v, KeyCode::Backspace);
        assert_eq!(s.filter, "s");
        assert_eq!(
            s.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL), &v),
            TableKey::Unhandled,
            "control keys are not filter text"
        );
        assert_eq!(press(&mut s, &v, KeyCode::Enter), TableKey::Enter);
        assert_eq!(press(&mut s, &v, KeyCode::Esc), TableKey::Esc);
    }

    #[test]
    fn a_filter_that_matches_nothing_says_so() {
        let v = view(vec![]);
        let theme = Theme::terminal(true);
        let shown = |s: &TableState| {
            let buf = draw(&v, s, &theme, 30, 5);
            (0..5u16)
                .map(|y| (0..30u16).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let filtered = TableState {
            filter: "zzz".into(),
            ..Default::default()
        };
        assert!(
            shown(&filtered).contains("No matches"),
            "{}",
            shown(&filtered)
        );
        assert!(
            !shown(&TableState::default()).contains("No matches"),
            "an empty table with no filter is not a failed search"
        );
    }

    #[test]
    fn an_empty_table_shows_its_status_and_the_filter_line() {
        let mut v = view(vec![]);
        v.status = Some("Loading models…".into());
        let s = TableState {
            filter: "son".into(),
            ..Default::default()
        };
        let theme = Theme::terminal(true);
        let buf = draw(&v, &s, &theme, 40, 6);
        let shown: String = (0..6)
            .map(|y| {
                (0..40)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(shown.contains("Model"), "{shown}");
        assert!(shown.contains("> son"), "{shown}");
        assert!(shown.contains("Loading models…"), "{shown}");
    }

    #[test]
    fn spinners_are_painted_in_their_cells_on_scrolled_rows() {
        let keys: Vec<Uuid> = (0..20).map(|_| Uuid::new_v4()).collect();
        let rows = keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                Row::item(
                    RowKey::Chat(*k),
                    vec![
                        Line::from("·"),
                        Line::from(format!("row {i}")),
                        Line::from("+1 ·"),
                    ],
                )
            })
            .collect();
        let v = TableView {
            widths: vec![
                Constraint::Length(1),
                Constraint::Fill(1),
                Constraint::Length(4),
            ],
            spinners: (0..20)
                .flat_map(|row| {
                    [
                        Spinner {
                            row,
                            column: 0,
                            offset: 0,
                        },
                        Spinner {
                            row,
                            column: 2,
                            offset: 3,
                        },
                    ]
                })
                .collect(),
            ..view(rows)
        };
        let s = TableState::with_selected(RowKey::Chat(keys[19]));
        let theme = Theme::terminal(true);
        let mut term = Terminal::new(TestBackend::new(30, 6)).unwrap();
        term.draw(|f| {
            render(f, f.area(), &v, &s, &theme, Some("X"));
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let lines: Vec<String> = (0..6u16)
            .map(|y| (0..30u16).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let last = lines.iter().find(|l| l.contains("row 19")).unwrap();
        assert!(last.contains("› X row 19"), "{lines:#?}");
        assert!(last.contains("+1 X"), "{lines:#?}");
        let above = lines.iter().find(|l| l.contains("row 18")).unwrap();
        assert!(
            above.contains("  X row 18") && above.contains("+1 X"),
            "{lines:#?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains('·')),
            "every visible spinner is painted: {lines:#?}"
        );
    }

    #[test]
    fn moving_up_through_a_long_list_scrolls_only_once_the_selection_reaches_the_top() {
        let keys: Vec<Uuid> = (0..20).map(|_| Uuid::new_v4()).collect();
        let rows = keys
            .iter()
            .enumerate()
            .map(|(i, k)| Row::item(RowKey::Chat(*k), vec![Line::from(format!("row {i:02}"))]))
            .collect();
        let v = view(rows);
        let mut s = TableState::with_selected(RowKey::Chat(keys[19]));
        let theme = Theme::terminal(true);
        let shown = |s: &TableState| {
            let buf = draw(&v, s, &theme, 30, 7);
            (0..7u16)
                .map(|y| (0..30u16).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
        };
        let first = shown(&s);
        assert!(first.iter().any(|l| l.contains("› row 19")), "{first:#?}");
        press(&mut s, &v, KeyCode::Up);
        let after = shown(&s);
        assert!(after.iter().any(|l| l.contains("› row 18")), "{after:#?}");
        assert!(
            after.iter().any(|l| l.contains("row 19")),
            "the list keeps its place while the selection moves up: {after:#?}"
        );
    }

    fn view_of(keys: &[i64]) -> TableView {
        TableView {
            rows: keys
                .iter()
                .map(|k| Row::item(RowKey::Queued(*k), vec![Line::from(k.to_string())]))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_row_that_leaves_hands_the_selection_to_the_row_above_it() {
        let mut state = TableState::with_selected(RowKey::Queued(2));
        state.remember_above(&view_of(&[1, 2, 3]));
        let after = view_of(&[3, 1]);
        assert_eq!(state.index(&after), Some(1), "found by key after a reorder");
        let first = TableState::with_selected(RowKey::Queued(1));
        first.remember_above(&view_of(&[1, 2, 3]));
        assert_eq!(
            first.index(&view_of(&[2, 3])),
            Some(0),
            "no row above: the first"
        );
        assert_eq!(first.index(&view_of(&[])), None, "nothing to select");
        let kept = view_of(&[1, 2, 3]);
        assert_eq!(
            state.index(&kept),
            Some(1),
            "a row that stays keeps the selection"
        );
        state.handle_key(
            crossterm::event::KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &kept,
        );
        assert_eq!(
            state.index(&view_of(&[1, 2])),
            Some(1),
            "a move remembers the new row above without a draw"
        );
        let mut moved = TableState::with_selected(RowKey::Queued(2));
        moved.remember_above(&view_of(&[1, 2, 3]));
        moved.selected = Some(RowKey::Queued(9));
        moved.remember_above(&view_of(&[1, 2, 3]));
        assert_eq!(
            moved.index(&view_of(&[3, 1])),
            Some(1),
            "a selection that is already missing falls back to key 1, the row above key 2"
        );
    }

    #[test]
    fn a_selected_row_that_returns_drops_its_adopted_stand_in() {
        // 1, 2, 3, 4 with 3 selected: 3 leaves, so 2 stands in for it.
        let state = TableState::with_selected(RowKey::Queued(3));
        state.remember_above(&view_of(&[1, 2, 3, 4]));
        state.remember_above(&view_of(&[1, 2, 4]));
        // 3 comes back in a new order, below 4, and leaves again: the row above it now is 4.
        state.remember_above(&view_of(&[4, 3, 1, 2]));
        let after = view_of(&[4, 1, 2]);
        assert_eq!(
            state.index(&after),
            Some(0),
            "the row above it when it left, not the stand-in from before"
        );
    }

    #[test]
    fn an_empty_list_or_no_selection_forgets_the_rows_around_it() {
        let state = TableState::with_selected(RowKey::Queued(3));
        state.remember_above(&view_of(&[1, 2, 3]));
        state.remember_above(&view_of(&[]));
        assert_eq!(
            state.index(&view_of(&[1, 2])),
            Some(0),
            "after an empty list, a reload without the row starts at the first"
        );
        let mut cleared = TableState::with_selected(RowKey::Queued(3));
        cleared.remember_above(&view_of(&[1, 2, 3]));
        cleared.selected = None;
        cleared.remember_above(&view_of(&[1, 2, 3]));
        cleared.selected = Some(RowKey::Queued(9));
        assert_eq!(
            cleared.index(&view_of(&[1, 2])),
            Some(0),
            "a cleared selection keeps nothing from before"
        );
    }

    #[test]
    fn a_selection_that_fell_back_adopts_its_row_so_the_next_to_leave_hands_on_too() {
        let state = TableState::with_selected(RowKey::Queued(4));
        state.remember_above(&view_of(&[1, 2, 3, 4, 5]));
        let without_4 = view_of(&[1, 2, 3, 5]);
        assert_eq!(state.index(&without_4), Some(2), "the row above 4");
        state.remember_above(&without_4);
        assert_eq!(
            state.index(&view_of(&[1, 2, 5])),
            Some(1),
            "the row above 3, the row the selection fell back to, and not the first"
        );
        state.remember_above(&view_of(&[1, 2, 5]));
        assert_eq!(
            state.index(&view_of(&[1, 5])),
            Some(0),
            "the row above 2 in turn"
        );
    }
}
