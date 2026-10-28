//! Drawing. Nothing here uses ratatui's stock widgets: cards are filled
//! rectangles whose top and bottom rows are half blocks, which gives them soft
//! edges against whatever is behind them, and chips are pills. When the
//! terminal supports kitty graphics, card corners, pill caps, and the send
//! button are anti-aliased images (see `chrome`); otherwise they're quadrant
//! and half-block characters.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Hit, Overlay, Status, host_label};
use crate::chrome::{Chrome, Corner, Shape};
use crate::config::Level;
use crate::theme::*;

const MAX_WIDTH: u16 = 112;
const MIN_EDITOR_ROWS: u16 = 3;
const MAX_EDITOR_ROWS: u16 = 8;
/// Rows besides the editor: a margin (1), the strip (2), the composer's edges,
/// gap, and chip row (4), and a gap and the footer (2).
const CHROME_ROWS: u16 = 9;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    if let Some(cursor) = paint(frame.buffer_mut(), area, app) {
        frame.set_cursor_position(cursor);
    }
}

/// Draws the popup and returns where the text cursor goes, if anywhere.
fn paint(buf: &mut Buffer, area: Rect, app: &mut App) -> Option<Position> {
    fill(buf, area, PAGE);
    app.hits.clear();
    if area.width < 44 || area.height < CHROME_ROWS + MIN_EDITOR_ROWS {
        let line = Line::styled("Make this pane larger", Style::new().fg(MUTED));
        put(buf, area.x + 1, area.y, &line, area.width.saturating_sub(2));
        return None;
    }

    let width = (area.width - 4).min(MAX_WIDTH);
    let x = area.x + (area.width - width) / 2;
    // Two columns of padding on each side, plus one so the cursor fits after a full row.
    app.text_width = usize::from(width - 5);
    let wanted = app.editor.rows(app.text_width).len() as u16;
    let editor_rows = wanted
        .clamp(MIN_EDITOR_ROWS, MAX_EDITOR_ROWS)
        .min(area.height - CHROME_ROWS);

    // Laid out from the top, with the hints under the composer. Menus drop
    // down over the composer.
    let strip = Rect::new(x + 1, area.y + 1, width - 2, 3);
    // The strip's last row is hidden behind the composer's top edge.
    let composer = Rect::new(x, strip.y + 2, width, editor_rows + 4);
    let chips_y = composer.y + editor_rows + 2;
    let footer_y = composer.bottom() + 1;

    card(buf, strip, STRIP, Edges::Top, app.chrome.as_mut());
    card(buf, composer, CARD, Edges::Both, app.chrome.as_mut());

    let host_chip = header(buf, app, strip);
    let cursor = prompt(
        buf,
        app,
        Rect::new(x + 2, composer.y + 1, width - 4, editor_rows),
    );
    let cursor = chip_row(buf, app, x, width, chips_y).or(cursor);
    footer(buf, app, Rect::new(x + 2, footer_y, width - 4, 1));

    match app.overlay {
        Overlay::Projects => project_menu(buf, app, area, strip),
        Overlay::Hosts => host_menu(buf, app, area, host_chip.x, strip.y + 2),
        Overlay::Models => {
            let anchor_right = x + width - 2;
            model_menu(buf, app, area, anchor_right, chips_y)
        }
        _ => cursor,
    }
}

/// The project and connection chips. Returns the connection chip's area.
fn header(buf: &mut Buffer, app: &mut App, strip: Rect) -> Rect {
    let y = strip.y + 1;
    let project = app.current_project();
    let name_color = if app.project_available(project) {
        TEXT
    } else {
        FAINT
    };
    let project_chip = Line::from(vec![
        Span::styled(project.name.clone(), Style::new().fg(name_color)),
        Span::styled(" ▾", Style::new().fg(FAINT)),
    ]);
    let mut host_chip = host_line(app.current_host());
    host_chip.push_span(Span::styled(" ▾", Style::new().fg(FAINT)));
    let open = app.overlay == Overlay::Projects;
    let r = chip(
        buf,
        app,
        strip.x,
        y,
        &project_chip,
        open,
        Some(Hit::Project),
    );
    let open = app.overlay == Overlay::Hosts;
    chip(
        buf,
        app,
        r.right() + 1,
        y,
        &host_chip,
        open,
        Some(Hit::Host),
    )
}

/// A connection with its icon: a house for this computer, a violet globe for
/// a Rex host.
fn host_line(host: Option<&str>) -> Line<'static> {
    let (icon, color) = if host.is_some() {
        ("⊕ ", VIOLET)
    } else {
        ("⌂ ", MUTED)
    };
    Line::from(vec![
        Span::styled(icon, Style::new().fg(color)),
        Span::styled(host_label(host).to_string(), Style::new().fg(TEXT)),
    ])
}

/// Where a menu of `needed` rows opens upward: just above `bottom` when it
/// fits, otherwise as high as it can go, overlapping what's below.
fn menu_above(area: Rect, bottom: u16, needed: u16) -> (u16, u16) {
    let height = needed.min(area.height.saturating_sub(1));
    let y = bottom.saturating_sub(height).max(area.y);
    (y, height)
}

/// Where a menu of `needed` rows drops down: from `top` when it fits,
/// otherwise moved up just enough to fit.
fn menu_below(area: Rect, top: u16, needed: u16) -> (u16, u16) {
    let height = needed.min(area.height.saturating_sub(1));
    let y = top.min(area.bottom().saturating_sub(height)).max(area.y);
    (y, height)
}

fn prompt(buf: &mut Buffer, app: &mut App, area: Rect) -> Option<Position> {
    let width = app.text_width;
    let rows = app.editor.rows(width);
    let (row, col) = app.editor.cursor_pos(width);
    let height = usize::from(area.height);
    let e = &mut app.editor;
    if row < e.scroll {
        e.scroll = row;
    } else if row >= e.scroll + height {
        e.scroll = row + 1 - height;
    }
    e.scroll = e.scroll.min(rows.len().saturating_sub(height));

    if e.text().is_empty() {
        put(
            buf,
            area.x,
            area.y,
            &Line::styled("Do anything", Style::new().fg(FAINT)),
            area.width,
        );
    }
    for (i, r) in rows.iter().skip(e.scroll).take(height).enumerate() {
        let line = Line::styled(e.slice(*r), Style::new().fg(TEXT));
        put(buf, area.x, area.y + i as u16, &line, area.width);
    }
    let cursor = Position::new(area.x + col as u16, area.y + (row - e.scroll) as u16);
    (app.overlay == Overlay::None && !app.busy()).then_some(cursor)
}

/// The access, branch, and model chips and the send button, with two cells
/// of padding inside the card on each side.
fn chip_row(buf: &mut Buffer, app: &mut App, x: u16, width: u16, y: u16) -> Option<Position> {
    let last = x + width - 3;

    let (fill, ink) = if app.can_submit() {
        (CREAM, PAGE)
    } else {
        (RAISED, FAINT)
    };
    let busy = app.busy();
    let send = match app.chrome.as_mut() {
        Some(chrome) if !busy => {
            let send = Rect::new(last - 1, y, 2, 1);
            chrome.put(buf, send.x, y, Shape::SendButton, fill, ink);
            send
        }
        chrome => {
            let label = if busy { "…" } else { "↑" };
            let style = Style::new().fg(ink).bg(fill).add_modifier(Modifier::BOLD);
            pill(buf, last - 2, y, &Line::styled(label, style), fill, chrome)
        }
    };
    app.hits.push((send, Hit::Send));

    let agent = app.current_agent();
    let detail = app
        .effort(app.agent)
        .map_or_else(|| agent.detail.clone(), effort_label);
    let model = Line::from(vec![
        Span::styled(agent.name.clone(), Style::new().fg(TEXT)),
        Span::styled(format!(" {detail}"), Style::new().fg(MUTED)),
        Span::styled(" ▾", Style::new().fg(FAINT)),
    ]);
    // The chip's right cap is blank unless it's raised, so it doubles as the gap before the button.
    let model_x = send.x.saturating_sub(model.width() as u16 + 2);
    let open = app.overlay == Overlay::Models;
    let model_rect = chip(buf, app, model_x, y, &model, open, Some(Hit::Model));

    let access = app.current_access();
    let (icon, color) = match access.level {
        Level::Full => ("◆ ", ORANGE),
        Level::Auto => ("◈ ", TEAL),
        Level::Default => ("◇ ", MUTED),
    };
    let access_line = Line::from(vec![
        Span::styled(icon, Style::new().fg(color)),
        Span::styled(access.name.clone(), Style::new().fg(color)),
    ]);
    let access_rect = chip(buf, app, x + 1, y, &access_line, false, Some(Hit::Access));

    // The branch chip gets whatever room is left, and truncates to fit.
    let editing = app.overlay == Overlay::Branch;
    let branch_x = access_rect.right() + 1;
    let room = model_rect.x.saturating_sub(branch_x + 2).saturating_sub(4) as usize;
    let (name, color) = match app.branch.as_str() {
        "" if !editing => ("branch".to_string(), FAINT),
        name => (
            truncate_start(name, room),
            if editing { TEXT } else { MUTED },
        ),
    };
    let branch = Line::from(vec![
        Span::styled("⎇ ", Style::new().fg(FAINT)),
        Span::styled(name, Style::new().fg(color)),
    ]);
    let rect = chip(buf, app, branch_x, y, &branch, editing, Some(Hit::Branch));
    editing.then(|| Position::new(rect.right() - 1, y))
}

fn footer(buf: &mut Buffer, app: &App, area: Rect) {
    let line = match &app.status {
        Status::Idle => {
            let hints = [
                ("⏎", "start"),
                ("⇧⏎", "newline"),
                ("^P", "project"),
                ("^R", "connection"),
                ("^O", "model"),
                ("^S", "access"),
                ("^B", "branch"),
                ("esc", "close"),
            ];
            let mut spans = Vec::new();
            for (key, label) in hints {
                spans.push(Span::styled(key, Style::new().fg(MUTED)));
                spans.push(Span::styled(format!(" {label}   "), Style::new().fg(FAINT)));
            }
            Line::from(spans)
        }
        Status::Busy(text) => status_line("◌ ", TEAL, text),
        Status::Notice(text) => status_line("✓ ", TEAL, text),
        Status::Error(text) => status_line("✕ ", RED, text),
    };
    put(buf, area.x, area.y, &line, area.width);
}

fn status_line(icon: &'static str, color: Color, text: &str) -> Line<'static> {
    let text = text.lines().next().unwrap_or_default().to_string();
    Line::from(vec![
        Span::styled(icon, Style::new().fg(color)),
        Span::styled(text, Style::new().fg(TEXT)),
    ])
}

fn project_menu(buf: &mut Buffer, app: &mut App, area: Rect, strip: Rect) -> Option<Position> {
    let matches = app.project_matches();
    let width = strip.width.min(60);
    // Edges, search, and rule take four rows.
    let (y, height) = menu_below(area, strip.y + 2, matches.len().max(1) as u16 + 4);
    if height < 5 {
        return None;
    }
    let menu = Rect::new(strip.x, y, width, height);
    menu_card(buf, app, menu);

    let cursor = search_row(buf, app, menu, "Search projects");
    if app
        .scanning
        .iter()
        .any(|h| h.as_deref() == app.current_host())
    {
        scanning_note(buf, menu);
    }
    let visible = usize::from(height - 4);
    let first = scroll_to_selected(app, visible, matches.len());
    for (row, &index) in matches.iter().enumerate().skip(first).take(visible) {
        let project = &app.projects[index];
        let line = if project.scratch {
            Line::from(vec![
                Span::styled(project.name.clone(), Style::new().fg(MUTED)),
                Span::styled("  in ~, without a worktree", Style::new().fg(FAINT)),
            ])
        } else {
            let color = if app.project_available(project) {
                TEXT
            } else {
                FAINT
            };
            Line::styled(project.name.clone(), Style::new().fg(color))
        };
        let y = menu.y + 3 + (row - first) as u16;
        menu_row(
            buf,
            app,
            menu,
            y,
            line,
            row == app.selected,
            index == app.project,
        );
        app.hits
            .push((Rect::new(menu.x, y, menu.width, 1), Hit::ProjectRow(index)));
    }
    if matches.is_empty() {
        put(
            buf,
            menu.x + 3,
            menu.y + 3,
            &Line::styled("No matching projects", Style::new().fg(FAINT)),
            menu.width - 4,
        );
    }
    scrollbar(buf, menu, first, visible, matches.len());
    cursor
}

/// "scanning…" at the right of a menu's search row.
fn scanning_note(buf: &mut Buffer, menu: Rect) {
    let note = Line::styled("scanning…", Style::new().fg(FAINT));
    let w = note.width() as u16;
    if menu.width > w + 20 {
        put(buf, menu.right() - w - 3, menu.y + 1, &note, w);
    }
}

/// The first row to show in a menu of `count` rows with room for `visible`,
/// scrolling only as far as needed to keep the selected row in view.
fn scroll_to_selected(app: &mut App, visible: usize, count: usize) -> usize {
    let selected = app.selected;
    let first = &mut app.menu_scroll;
    if selected < *first {
        *first = selected;
    } else if selected >= *first + visible {
        *first = selected + 1 - visible;
    }
    *first = (*first).min(count.saturating_sub(visible));
    *first
}

/// A thin scrollbar in a menu's right margin when its rows don't all fit.
fn scrollbar(buf: &mut Buffer, menu: Rect, first: usize, visible: usize, count: usize) {
    if count <= visible || visible == 0 {
        return;
    }
    let thumb = (visible * visible / count).max(1);
    let start = (first * (visible - thumb) + (count - visible) / 2) / (count - visible);
    for row in 0..visible {
        let on_thumb = (start..start + thumb).contains(&row);
        let (symbol, color) = if on_thumb {
            ("┃", MUTED)
        } else {
            ("│", RULE)
        };
        set(
            buf,
            menu.right() - 2,
            menu.y + 3 + row as u16,
            symbol,
            color,
            None,
        );
    }
}

fn host_menu(buf: &mut Buffer, app: &mut App, area: Rect, x: u16, top: u16) -> Option<Position> {
    let matches = app.host_matches();
    let width = 44.min(area.right().saturating_sub(x));
    let (y, height) = menu_below(area, top, matches.len().max(1) as u16 + 4);
    if height < 5 || width < 20 {
        return None;
    }
    let menu = Rect::new(x, y, width, height);
    menu_card(buf, app, menu);

    let cursor = search_row(buf, app, menu, "Connections");
    let current = app.current_host().map(String::from);
    let visible = usize::from(height - 4);
    let first = scroll_to_selected(app, visible, matches.len());
    for (row, &index) in matches.iter().enumerate().skip(first).take(visible) {
        let host = app.hosts[index].clone();
        let count = app
            .projects_on(host.as_deref())
            .iter()
            .filter(|&&i| !app.projects[i].scratch)
            .count();
        let mut line = host_line(host.as_deref());
        let noun = if count == 1 { "project" } else { "projects" };
        let scanning = if app.scanning.contains(&host) {
            ", scanning…"
        } else {
            ""
        };
        line.push_span(Span::styled(
            format!("  {count} {noun}{scanning}"),
            Style::new().fg(FAINT),
        ));
        let y = menu.y + 3 + (row - first) as u16;
        menu_row(
            buf,
            app,
            menu,
            y,
            line,
            row == app.selected,
            host == current,
        );
        app.hits
            .push((Rect::new(menu.x, y, menu.width, 1), Hit::HostRow(index)));
    }
    scrollbar(buf, menu, first, visible, matches.len());
    cursor
}

fn model_menu(
    buf: &mut Buffer,
    app: &mut App,
    area: Rect,
    right: u16,
    above: u16,
) -> Option<Position> {
    let matches = app.agent_matches();
    let width = 46.min(right - area.x);
    let (y, height) = menu_above(area, above, matches.len().max(1) as u16 + 4);
    if height < 5 {
        return None;
    }
    let menu = Rect::new(right - width, y, width, height);
    menu_card(buf, app, menu);

    let cursor = search_row(buf, app, menu, "Model");
    if !app.fetching.is_empty() {
        scanning_note(buf, menu);
    }
    let harness_width = app
        .agents
        .iter()
        .map(|a| a.harness.width())
        .max()
        .unwrap_or(0);
    let visible = usize::from(height - 4);
    let first = scroll_to_selected(app, visible, matches.len());
    for (row, &index) in matches.iter().enumerate().skip(first).take(visible) {
        let agent = &app.agents[index];
        let selected = row == app.selected;
        let detail = match app.effort(index) {
            // The arrows show that left and right change the highlighted model's effort.
            Some(effort) if selected && agent.efforts.len() > 1 => {
                format!(" ‹ {} ›", effort_label(effort))
            }
            Some(effort) => format!(" {}", effort_label(effort)),
            None => format!(" {}", agent.detail),
        };
        let line = Line::from(vec![
            Span::styled(
                format!("{:harness_width$}  ", agent.harness),
                Style::new().fg(FAINT),
            ),
            Span::styled(agent.name.clone(), Style::new().fg(TEXT)),
            Span::styled(detail, Style::new().fg(MUTED)),
        ]);
        let y = menu.y + 3 + (row - first) as u16;
        menu_row(
            buf,
            app,
            menu,
            y,
            line,
            row == app.selected,
            index == app.agent,
        );
        app.hits
            .push((Rect::new(menu.x, y, menu.width, 1), Hit::ModelRow(index)));
    }
    scrollbar(buf, menu, first, visible, matches.len());
    cursor
}

fn menu_card(buf: &mut Buffer, app: &mut App, menu: Rect) {
    card(buf, menu, POPOVER, Edges::Both, app.chrome.as_mut());
    app.hits.push((menu, Hit::Menu));
}

/// The query line and the rule under it. Returns the cursor position.
fn search_row(buf: &mut Buffer, app: &App, menu: Rect, placeholder: &str) -> Option<Position> {
    let y = menu.y + 1;
    let query = if app.query.is_empty() {
        Span::styled(placeholder.to_string(), Style::new().fg(FAINT))
    } else {
        Span::styled(app.query.clone(), Style::new().fg(TEXT))
    };
    put(
        buf,
        menu.x + 2,
        y,
        &Line::from(vec![Span::styled("⌕ ", Style::new().fg(FAINT)), query]),
        menu.width - 4,
    );
    let rule = Line::styled(
        "─".repeat(usize::from(menu.width - 4)),
        Style::new().fg(RULE),
    );
    put(buf, menu.x + 2, y + 1, &rule, menu.width - 4);
    Some(Position::new(menu.x + 4 + app.query.width() as u16, y))
}

fn menu_row(
    buf: &mut Buffer,
    app: &mut App,
    menu: Rect,
    y: u16,
    line: Line,
    selected: bool,
    current: bool,
) {
    let row = Rect::new(menu.x + 1, y, menu.width - 2, 1);
    let hovered = app.hover.is_some_and(|p| row.contains(p));
    if selected || hovered {
        let bg = if selected { SELECTED } else { RAISED };
        pill(
            buf,
            row.x,
            y,
            &Line::raw(" ".repeat(usize::from(row.width - 2))),
            bg,
            app.chrome.as_mut(),
        );
    }
    put(buf, menu.x + 2, y, &line, menu.width - 6);
    if current {
        put(
            buf,
            menu.right() - 3,
            y,
            &Line::styled("✓", Style::new().fg(TEXT)),
            1,
        );
    }
}

/// Draws `line` as a chip at (x, y): raised into a pill when `raised` or
/// under the mouse, flat otherwise, with the same footprint either way.
fn chip(
    buf: &mut Buffer,
    app: &mut App,
    x: u16,
    y: u16,
    line: &Line,
    raised: bool,
    hit: Option<Hit>,
) -> Rect {
    let rect = Rect::new(x, y, line.width() as u16 + 2, 1);
    let hovered = hit.is_some() && app.hover.is_some_and(|p| rect.contains(p));
    let rect = if raised || hovered {
        pill(buf, x, y, line, RAISED, app.chrome.as_mut())
    } else {
        put(buf, x + 1, y, line, line.width() as u16);
        rect
    };
    if let Some(hit) = hit {
        app.hits.push((rect, hit));
    }
    rect
}

/// A pill: `line` on `bg` between two caps that blend into what's behind:
/// semicircle images with `chrome`, half blocks without.
fn pill(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line: &Line,
    bg: Color,
    chrome: Option<&mut Chrome>,
) -> Rect {
    let width = line.width() as u16;
    for dx in 0..width {
        set(buf, x + 1 + dx, y, " ", TEXT, Some(bg));
    }
    put(buf, x + 1, y, line, width);
    match chrome {
        Some(chrome) => {
            chrome.put(buf, x, y, Shape::PillCap { left: true }, bg, bg);
            chrome.put(
                buf,
                x + 1 + width,
                y,
                Shape::PillCap { left: false },
                bg,
                bg,
            );
        }
        None => {
            set(buf, x, y, "▐", bg, None);
            set(buf, x + 1 + width, y, "▌", bg, None);
        }
    }
    Rect::new(x, y, width + 2, 1)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edges {
    Top,
    Both,
}

/// A filled rectangle. Its top and bottom edges are half blocks over the
/// existing background, so the card sits softly on whatever is behind it; its
/// corners are rounded images with `chrome` and quadrant blocks without.
fn card(buf: &mut Buffer, r: Rect, color: Color, edges: Edges, mut chrome: Option<&mut Chrome>) {
    let last = r.bottom().saturating_sub(1);
    let rounded_bottom = edges == Edges::Both;
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            let (left, right) = (x == r.left(), x == r.right() - 1);
            let (top, bottom) = (y == r.top(), y == last && rounded_bottom);
            let corner = match (top, bottom, left, right) {
                (true, _, true, _) => Some((Corner::TopLeft, "▗")),
                (true, _, _, true) => Some((Corner::TopRight, "▖")),
                (_, true, true, _) => Some((Corner::BottomLeft, "▝")),
                (_, true, _, true) => Some((Corner::BottomRight, "▘")),
                _ => None,
            };
            match (corner, chrome.as_deref_mut()) {
                (Some((corner, _)), Some(chrome)) => {
                    chrome.put(buf, x, y, Shape::CardCorner(corner), color, color)
                }
                (Some((_, symbol)), None) => set(buf, x, y, symbol, color, None),
                (None, _) if top => set(buf, x, y, "▄", color, None),
                (None, _) if bottom => set(buf, x, y, "▀", color, None),
                (None, _) => set(buf, x, y, " ", TEXT, Some(color)),
            }
        }
    }
}

fn fill(buf: &mut Buffer, r: Rect, color: Color) {
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            set(buf, x, y, " ", TEXT, Some(color));
        }
    }
}

/// Sets one cell. With `bg` of `None` the cell keeps the background already
/// there, which is how half blocks blend into what's behind them.
fn set(buf: &mut Buffer, x: u16, y: u16, symbol: &str, fg: Color, bg: Option<Color>) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_symbol(symbol).set_fg(fg);
        if let Some(bg) = bg {
            cell.set_bg(bg);
        }
    }
}

/// Writes `line` at (x, y), clipped to `max` columns and to the buffer.
/// Spans without a background keep the one already in each cell.
fn put(buf: &mut Buffer, x: u16, y: u16, line: &Line, max: u16) {
    let area = buf.area;
    if !area.contains(Position::new(x, y)) {
        return;
    }
    let max = max.min(area.right() - x);
    buf.set_line(x, y, line, max);
}

/// "xhigh" reads as "XHigh", "high" as "High".
fn effort_label(effort: &str) -> String {
    match effort {
        "xhigh" => "XHigh".into(),
        _ => effort
            .chars()
            .take(1)
            .flat_map(char::to_uppercase)
            .chain(effort.chars().skip(1))
            .collect(),
    }
}

/// Keeps the end of `name`, which is the distinctive part of a long branch.
fn truncate_start(name: &str, max: usize) -> String {
    if name.width() <= max || max < 2 {
        return name.to_string();
    }
    let mut kept = String::new();
    for c in name.chars().rev() {
        if kept.width() + 2 > max {
            break;
        }
        kept.insert(0, c);
    }
    format!("…{kept}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, State};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn app() -> App {
        let config: Config = toml::from_str(
            r#"
            [[project]]
            name = "webapp"
            path = "/nonexistent/webapp"

            [[project]]
            name = "data"
            path = "/Users/me/Code/data"
            host = "studio"

            [[agent]]
            id = "claude"
            harness = "Claude"
            name = "Opus 5.5"
            detail = "High"
            command = ["claude"]
            access = [{ name = "Full access", level = "full" }]
            "#,
        )
        .unwrap();
        App::new(
            config,
            State::default(),
            None,
            vec!["studio".into(), "buildbox".into()],
            &Default::default(),
            &[],
        )
        .unwrap()
    }

    fn render(app: &mut App, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn every_cell_has_an_explicit_background() {
        for overlay in [
            Overlay::None,
            Overlay::Projects,
            Overlay::Hosts,
            Overlay::Models,
        ] {
            let mut app = app();
            app.overlay = overlay;
            let buf = render(&mut app, 100, 30);
            for cell in buf.content() {
                assert_ne!(
                    cell.bg,
                    Color::Reset,
                    "a cell would show the pane underneath"
                );
            }
        }
    }

    #[test]
    fn graphics_keep_backgrounds_and_put_images_in_corners() {
        use crate::chrome::CellSize;
        for overlay in [Overlay::None, Overlay::Projects, Overlay::Models] {
            let mut app = app();
            app.chrome = Some(Chrome::new(CellSize {
                width: 14,
                height: 28,
            }));
            app.overlay = overlay;
            let buf = render(&mut app, 100, 20);
            for cell in buf.content() {
                assert_ne!(
                    cell.bg,
                    Color::Reset,
                    "a cell would show the pane underneath"
                );
            }
            let images = buf
                .content()
                .iter()
                .filter(|c| c.symbol().starts_with('\u{10EEEE}'))
                .count();
            // Two cards' corners (strip top, composer), at least one pill's caps, and the send button.
            assert!(images >= 2 + 4 + 2, "found {images} image cells");
        }
    }

    #[test]
    fn small_panes_do_not_panic() {
        for (w, h) in [(1, 1), (20, 5), (44, 12), (45, 13), (60, 14)] {
            for overlay in [
                Overlay::None,
                Overlay::Projects,
                Overlay::Hosts,
                Overlay::Models,
            ] {
                let mut app = app();
                app.overlay = overlay;
                render(&mut app, w, h);
            }
        }
    }

    #[test]
    fn shows_the_placeholder_and_project() {
        let mut app = app();
        let buf = render(&mut app, 100, 30);
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Do anything"));
        assert!(text.contains(&app.current_project().name));
        assert!(text.contains("Full access"));
    }

    #[test]
    fn truncates_long_branches_from_the_start() {
        assert_eq!(truncate_start("add-retry-logic-around", 10), "…ic-around");
        assert_eq!(truncate_start("short", 10), "short");
    }
}
