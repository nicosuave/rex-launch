//! Rounded chrome drawn with kitty graphics Unicode placeholders.
//!
//! Only cells that never hold text become images: the four corner cells of a
//! card, the two end cells of a pill, and the send button. Straight card edges
//! stay half blocks and interiors stay ordinary cells with a background color,
//! so ratatui's diffing, text rendering, and the half-block fallback are
//! unchanged. Each image is transmitted once with a virtual placement (`U=1`),
//! then shown by writing `U+10EEEE` cells whose foreground color carries the
//! image id. A transparent pixel shows the cell's own background, which is
//! whatever sits behind the card.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::num::NonZeroU16;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::style::Color;

const PLACEHOLDER: char = '\u{10EEEE}';
/// The first entries of kitty's rowcolumn-diacritics.txt; index n encodes n.
/// Chrome images are one row tall and at most three columns wide.
const DIACRITICS: [char; 3] = ['\u{0305}', '\u{030D}', '\u{030E}'];
/// Image ids ride in a 24-bit foreground color. Start well away from small ids
/// another program in the same terminal might pick.
const FIRST_ID: u32 = 0x52_4C_00;
/// Base64 characters per transmission chunk, the protocol's maximum.
const CHUNK: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shape {
    /// One cell: a card corner. The card's top and bottom edges sit half a
    /// cell in, matching the ▄ and ▀ half blocks along its straight edges.
    CardCorner(Corner),
    /// One cell: the left or right end of a full-height pill.
    PillCap { left: bool },
    /// Two cells, which is a circle at the usual 1:2 cell shape, with an up
    /// arrow drawn in `ink`.
    SendButton,
}

impl Shape {
    fn columns(self) -> u16 {
        match self {
            Shape::SendButton => 2,
            _ => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    shape: Shape,
    fill: (u8, u8, u8),
    ink: (u8, u8, u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellSize {
    pub width: u16,
    pub height: u16,
}

pub struct Chrome {
    cell: CellSize,
    ids: HashMap<Key, u32>,
    next_id: u32,
    /// Escape sequences to write after the next frame.
    pending: Vec<u8>,
}

impl Chrome {
    pub fn new(cell: CellSize) -> Self {
        Chrome {
            cell,
            ids: HashMap::new(),
            next_id: FIRST_ID,
            pending: Vec::new(),
        }
    }

    /// Uses a new cell size for images rendered from now on.
    pub fn set_cell_size(&mut self, cell: CellSize) {
        if cell.width > 0 && cell.height > 0 {
            self.cell = cell;
        }
    }

    /// Queues every image again at the current cell size. Ids stay the same,
    /// so cells already on screen pick up the new bitmaps without being
    /// rewritten. Rex's screen snapshot doesn't include image data, so this is
    /// also how an app that attaches late gets the images.
    pub fn retransmit(&mut self) {
        let keys: Vec<(Key, u32)> = self.ids.iter().map(|(k, id)| (*k, *id)).collect();
        for (key, id) in keys {
            self.transmit(key, id);
        }
    }

    /// Writes `shape` at (x, y) as placeholder cells, keeping each cell's
    /// background. `ink` is only used by shapes that draw a glyph.
    pub fn put(&mut self, buf: &mut Buffer, x: u16, y: u16, shape: Shape, fill: Color, ink: Color) {
        let key = Key {
            shape,
            fill: rgb(fill),
            ink: rgb(ink),
        };
        let id = self.id(key);
        let id_color = Color::Rgb((id >> 16) as u8, (id >> 8) as u8, id as u8);
        for col in 0..shape.columns() {
            let Some(cell) = buf.cell_mut((x + col, y)) else {
                continue;
            };
            // Every cell states its own row and column, so a diff that redraws
            // one cell alone still points at the right slice of the image.
            let symbol: String = [PLACEHOLDER, DIACRITICS[0], DIACRITICS[usize::from(col)]]
                .iter()
                .collect();
            cell.set_symbol(&symbol).set_fg(id_color);
            cell.modifier = Default::default();
            cell.set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
        }
    }

    /// Writes queued transmissions. Call after each `terminal.draw`.
    pub fn flush(&mut self, out: &mut impl Write) -> io::Result<()> {
        if !self.pending.is_empty() {
            out.write_all(&self.pending)?;
            out.flush()?;
            self.pending.clear();
        }
        Ok(())
    }

    /// Deletes every image and frees its data. Call before leaving the
    /// alternate screen.
    pub fn delete_all(&mut self, out: &mut impl Write) -> io::Result<()> {
        for id in self.ids.values() {
            write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")?;
        }
        out.flush()
    }

    fn id(&mut self, key: Key) -> u32 {
        if let Some(id) = self.ids.get(&key) {
            return *id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.ids.insert(key, id);
        self.transmit(key, id);
        id
    }

    fn transmit(&mut self, key: Key, id: u32) {
        let cols = u32::from(key.shape.columns());
        let (w, h) = (
            cols * u32::from(self.cell.width),
            u32::from(self.cell.height),
        );
        let rgba = rasterize(key, w, h);
        let data = BASE64.encode(rgba);
        let chunks: Vec<&[u8]> = data.as_bytes().chunks(CHUNK).collect();
        for (n, chunk) in chunks.iter().enumerate() {
            let more = u8::from(n + 1 < chunks.len());
            if n == 0 {
                // Transmit and create the virtual placement in one command.
                // c/r make every client scale the bitmap to its own cell grid.
                let _ = write!(
                    self.pending,
                    "\x1b_Ga=T,U=1,i={id},f=32,t=d,s={w},v={h},c={cols},r=1,q=2,m={more};"
                );
            } else {
                let _ = write!(self.pending, "\x1b_Gm={more},q=2;");
            }
            self.pending.extend_from_slice(chunk);
            self.pending.extend_from_slice(b"\x1b\\");
        }
    }
}

fn rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    }
}

// ---------- bitmaps ----------

/// Signed distance from (px, py) to a rounded rectangle; negative inside.
fn rounded_rect(px: f64, py: f64, (x0, y0, x1, y1): (f64, f64, f64, f64), radius: f64) -> f64 {
    let (hx, hy) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let qx = (px - (x0 + hx)).abs() - hx + radius;
    let qy = (py - (y0 + hy)).abs() - hy + radius;
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
}

/// Signed distance to a line segment of the given half width (a capsule).
fn segment(px: f64, py: f64, (ax, ay): (f64, f64), (bx, by): (f64, f64), half_width: f64) -> f64 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (px - (ax + t * dx)).hypot(py - (ay + t * dy)) - half_width
}

/// Straight-alpha RGBA for one chrome image. Coverage is a one-pixel ramp
/// across each shape's edge, which is enough anti-aliasing at Retina scale.
fn rasterize(key: Key, w: u32, h: u32) -> Vec<u8> {
    let (cw, ch) = (
        f64::from(w) / f64::from(key.shape.columns() as u32),
        f64::from(h),
    );
    let radius = cw.min(ch / 2.0);
    let far = 4.0 * ch; // stands in for "the rest of the card"
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (px, py) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            let fill_d = match key.shape {
                Shape::CardCorner(corner) => {
                    let rect = match corner {
                        Corner::TopLeft => (0.0, ch / 2.0, far, far),
                        Corner::TopRight => (-far, ch / 2.0, cw, far),
                        Corner::BottomLeft => (0.0, -far, far, ch / 2.0),
                        Corner::BottomRight => (-far, -far, cw, ch / 2.0),
                    };
                    rounded_rect(px, py, rect, radius)
                }
                Shape::PillCap { left: true } => rounded_rect(px, py, (0.0, 0.0, far, ch), radius),
                Shape::PillCap { left: false } => rounded_rect(px, py, (-far, 0.0, cw, ch), radius),
                Shape::SendButton => {
                    let r = (f64::from(w) / 2.0).min(ch / 2.0) - 0.5;
                    let (cx, cy) = (f64::from(w) / 2.0, ch / 2.0);
                    (px - cx).hypot(py - cy) - r
                }
            };
            let mut color = key.fill;
            let mut alpha = coverage(fill_d);
            if key.shape == Shape::SendButton && alpha > 0.0 {
                let (cx, cy) = (f64::from(w) / 2.0, ch / 2.0);
                let (top, bottom, arm) = (cy - ch * 0.26, cy + ch * 0.26, ch * 0.2);
                let stroke = ch * 0.045;
                let d = segment(px, py, (cx, bottom), (cx, top), stroke)
                    .min(segment(px, py, (cx - arm, top + arm), (cx, top), stroke))
                    .min(segment(px, py, (cx + arm, top + arm), (cx, top), stroke));
                let ink = coverage(d);
                color = mix(color, key.ink, ink);
                alpha = alpha.max(ink);
            }
            out.extend_from_slice(&[color.0, color.1, color.2, (alpha * 255.0).round() as u8]);
        }
    }
    out
}

fn coverage(distance: f64) -> f64 {
    (0.5 - distance).clamp(0.0, 1.0)
}

fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> (u8, u8, u8) {
    let lerp = |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * t).round() as u8;
    (lerp(a.0, b.0), lerp(a.1, b.1), lerp(a.2, b.2))
}

// ---------- detection ----------

/// Asks the terminal whether it supports kitty graphics and how big a cell is
/// in pixels. Must run in raw mode, before anything else reads stdin.
/// Returns `None` when graphics are unsupported or nobody answered.
pub fn detect(timeout: Duration) -> io::Result<Option<CellSize>> {
    if std::env::var_os("TMUX").is_some()
        || std::env::var("TERM").is_ok_and(|t| t.starts_with("screen"))
    {
        return Ok(None); // would need passthrough wrapping
    }
    let mut out = io::stdout();
    // The graphics query, the cell size in pixels, then primary device
    // attributes, which every terminal answers last.
    out.write_all(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[c")?;
    out.flush()?;

    let reply = read_until_da1(timeout)?;
    if !reply.contains("\x1b_Gi=31;OK") {
        return Ok(None);
    }
    Ok(parse_cell_size(&reply))
}

fn read_until_da1(timeout: Duration) -> io::Result<String> {
    let deadline = Instant::now() + timeout;
    let mut got = Vec::new();
    let mut stdin = io::stdin().lock();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut fds = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut fds, 1, left.as_millis() as libc::c_int) };
        if ready <= 0 {
            break;
        }
        let mut chunk = [0u8; 1024];
        let n = stdin.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        got.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&got);
        if let Some(start) = text.rfind("\x1b[?")
            && text[start..].contains('c')
        {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&got).into_owned())
}

/// Parses `CSI 6 ; height ; width t`.
fn parse_cell_size(reply: &str) -> Option<CellSize> {
    let rest = reply.split("\x1b[6;").nth(1)?;
    let body = rest.split('t').next()?;
    let mut parts = body.split(';').map(|n| n.parse::<u16>().ok());
    let height = parts.next()??;
    let width = parts.next()??;
    (width > 0 && height > 0).then_some(CellSize { width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cell_size_among_other_replies() {
        let reply = "\x1b_Gi=31;OK\x1b\\\x1b[6;28;14t\x1b[?62;22c";
        assert_eq!(
            parse_cell_size(reply),
            Some(CellSize {
                width: 14,
                height: 28
            })
        );
    }

    #[test]
    fn corner_is_transparent_outside_and_opaque_inside() {
        let key = Key {
            shape: Shape::CardCorner(Corner::TopLeft),
            fill: (1, 2, 3),
            ink: (0, 0, 0),
        };
        let px = rasterize(key, 14, 28);
        let alpha = |x: usize, y: usize| px[(y * 14 + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "top half is outside the card");
        assert_eq!(alpha(0, 14), 0, "the corner itself is cut away");
        assert_eq!(alpha(13, 27), 255, "the inner corner is filled");
    }

    #[test]
    fn placeholder_cells_carry_id_row_and_column() {
        let mut chrome = Chrome::new(CellSize {
            width: 14,
            height: 28,
        });
        let mut buf = Buffer::empty(ratatui::layout::Rect::new(0, 0, 4, 1));
        chrome.put(
            &mut buf,
            0,
            0,
            Shape::SendButton,
            Color::Rgb(9, 9, 9),
            Color::Rgb(0, 0, 0),
        );
        let cell = &buf[(1, 0)];
        assert_eq!(cell.symbol(), "\u{10EEEE}\u{0305}\u{030D}");
        assert_eq!(cell.fg, Color::Rgb(0x52, 0x4C, 0x00));
        assert!(
            !chrome.pending.is_empty(),
            "the first use queues a transmission"
        );
    }
}
