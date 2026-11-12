//! Colors. The dark palette is the one Files and Disk Usage use; the light one
//! mirrors it. Every color is a function so the whole window follows a switch
//! without anything holding on to stale values.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use gpui::{Hsla, hsla, rgb};

static DARK: AtomicBool = AtomicBool::new(true);

/// Returns true when this changed the palette.
pub fn set_dark(dark: bool) -> bool {
    DARK.swap(dark, Relaxed) != dark
}

pub fn is_dark() -> bool {
    DARK.load(Relaxed)
}

fn pick(dark: u32, light: u32) -> Hsla {
    rgb(if is_dark() { dark } else { light }).into()
}

pub fn bg() -> Hsla {
    pick(0x0c0e13, 0xf3f4f7)
}

pub fn panel() -> Hsla {
    pick(0x12151c, 0xffffff)
}

pub fn raised() -> Hsla {
    pick(0x181c25, 0xffffff)
}

pub fn hover() -> Hsla {
    if is_dark() {
        hsla(0.0, 0.0, 1.0, 0.045)
    } else {
        hsla(220.0 / 360.0, 0.3, 0.2, 0.05)
    }
}

pub fn border() -> Hsla {
    pick(0x222733, 0xe2e5eb)
}

pub fn border_strong() -> Hsla {
    pick(0x2e3443, 0xcfd4dd)
}

pub fn text() -> Hsla {
    pick(0xe6e9ef, 0x171a21)
}

pub fn text_soft() -> Hsla {
    pick(0xc4cad6, 0x2f3540)
}

pub fn muted() -> Hsla {
    pick(0x8b93a5, 0x5d6676)
}

pub fn faint() -> Hsla {
    pick(0x5a6276, 0x8c94a3)
}

pub fn accent() -> Hsla {
    pick(0x7c9cff, 0x3563f0)
}

pub fn accent_soft() -> Hsla {
    if is_dark() {
        hsla(226.0 / 360.0, 1.0, 0.74, 0.16)
    } else {
        hsla(225.0 / 360.0, 0.86, 0.57, 0.12)
    }
}

pub fn accent_strong() -> Hsla {
    if is_dark() {
        hsla(226.0 / 360.0, 1.0, 0.74, 0.26)
    } else {
        hsla(225.0 / 360.0, 0.86, 0.57, 0.22)
    }
}

pub fn on_accent() -> Hsla {
    pick(0x0b1020, 0xffffff)
}

pub fn danger() -> Hsla {
    pick(0xff7a85, 0xd93843)
}

pub fn success() -> Hsla {
    pick(0x7ddba8, 0x1a9a5c)
}

pub fn warning() -> Hsla {
    pick(0xf2c46d, 0xb87800)
}

/// Life-ring orange. Used for the mark and nowhere else, so it stays special.
pub fn brand() -> Hsla {
    pick(0xff8f5e, 0xe8622f)
}

pub fn match_bg() -> Hsla {
    hsla(
        42.0 / 360.0,
        0.95,
        0.62,
        if is_dark() { 0.30 } else { 0.45 },
    )
}

pub fn overlay() -> Hsla {
    hsla(0.0, 0.0, 0.0, if is_dark() { 0.55 } else { 0.28 })
}

/// A saturated hue (degrees) for a file category's icon.
pub fn category(hue: f32) -> Hsla {
    if is_dark() {
        hsla(hue / 360.0, 0.68, 0.68, 1.0)
    } else {
        hsla(hue / 360.0, 0.62, 0.47, 1.0)
    }
}

/// Soft background behind a big icon.
pub fn category_soft(hue: f32) -> Hsla {
    hsla(hue / 360.0, 0.55, 0.62, if is_dark() { 0.13 } else { 0.16 })
}
