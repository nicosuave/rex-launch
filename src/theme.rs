//! Palette. Every cell the popup draws gets an explicit background from here,
//! because Rex floating layers show the pane underneath through any cell that
//! keeps the terminal's default background.

use ratatui::style::Color;

/// The popup itself.
pub const PAGE: Color = Color::Rgb(0x27, 0x26, 0x24);
/// The project/host strip that sits behind the composer.
pub const STRIP: Color = Color::Rgb(0x31, 0x30, 0x2d);
/// The composer card.
pub const CARD: Color = Color::Rgb(0x3b, 0x3a, 0x36);
/// Menus that open over the composer.
pub const POPOVER: Color = Color::Rgb(0x43, 0x42, 0x3d);
/// A chip under the mouse or with its menu open.
pub const RAISED: Color = Color::Rgb(0x4b, 0x4a, 0x44);
/// The highlighted row of a menu.
pub const SELECTED: Color = Color::Rgb(0x55, 0x53, 0x4c);
pub const RULE: Color = Color::Rgb(0x55, 0x54, 0x4e);

pub const TEXT: Color = Color::Rgb(0xec, 0xea, 0xe3);
pub const MUTED: Color = Color::Rgb(0xa0, 0x9d, 0x95);
pub const FAINT: Color = Color::Rgb(0x75, 0x73, 0x6c);

pub const ORANGE: Color = Color::Rgb(0xe8, 0x8a, 0x4c);
pub const TEAL: Color = Color::Rgb(0x74, 0xc2, 0xaa);
pub const VIOLET: Color = Color::Rgb(0x9a, 0x92, 0xf6);
pub const RED: Color = Color::Rgb(0xee, 0x72, 0x6c);
/// The send button.
pub const CREAM: Color = Color::Rgb(0xea, 0xe2, 0xc6);
