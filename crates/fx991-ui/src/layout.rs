// SPDX-License-Identifier: GPL-3.0-only
//
// Copyright (C) 2026 fx991-rs contributors
//
// This program is free software: you can redistribute it and/or modify it under
// the terms of the GNU General Public License as published by the Free Software
// Foundation, version 3.  It is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU General
// Public License in LICENSE for more details.

//! Screen geometry: button rectangles and indicator sprites.
//!
//! Coordinates are in skin pixels.  Geometry only -- no rendering logic lives here;
//! see `Renderer` for that.

/// A button's hit box in skin coordinates, plus the matrix code it presses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Button {
    /// Left edge, inclusive.
    pub x: u16,
    /// Top edge, inclusive.
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// The matrix code the key scan sees.
    pub code: u8,
    /// A short label for diagnostics.  **Positional, not semantic** -- it
    /// names the button's place on the face, and several are misleading.
    pub label: &'static str,
}

/// A sprite's source rectangle in the skin and where it lands on screen.
///
/// The destination size always equals the source size -- the reference
/// implementation never scales a sprite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sprite {
    pub src_x: u16,
    pub src_y: u16,
    pub w: u16,
    pub h: u16,
    pub dest_x: u16,
    pub dest_y: u16,
}

/// An indicator sprite plus the screen-buffer bit that turns it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Indicator {
    pub name: &'static str,
    pub sprite: Sprite,
    /// Index into the screen buffer's row 0 (`0xF800 + offset`).
    pub offset: u8,
    /// Bit mask within that byte.
    pub mask: u8,
}

/// The body skin: `rsd_interface`'s source rectangle.
pub const SKIN_CROP: Sprite = Sprite {
    src_x: 0,
    src_y: 0,
    w: 284,
    h: 597,
    dest_x: 0,
    dest_y: 0,
};

/// The window's natural size: the skin crop's dimensions.
pub const SKIN_W: u32 = 284;
pub const SKIN_H: u32 = 597;

/// The full skin texture's size, which is larger than the visible crop.
///
/// The indicator atlas sits at `y >= 597`, outside [`SKIN_CROP`], so the
/// whole texture must be kept and sampled -- cropping first would make
/// every indicator read transparent pixels.
pub const SKIN_SOURCE_W: u32 = 307;
pub const SKIN_SOURCE_H: u32 = 615;

/// The 1x1 ink stamp every dot and indicator is drawn with.
///
/// Its source pixel is inside the LCD area, which is a single flat colour
/// in the source image, so this is an opaque ink *stamp*, not a
/// two-state sprite: the ink is modulated per pixel by the alpha.
pub const PIXEL_STAMP: Sprite = Sprite {
    src_x: 47,
    src_y: 112,
    w: 1,
    h: 1,
    dest_x: 47,
    dest_y: 112,
};

/// The ink colour, multiplied into the stamp's pixels.
///
/// The rendered colour is `skin_pixel * INK / 255`, **not** `INK` itself,
/// by the alpha, never written directly.
pub const INK: (u8, u8, u8) = (30, 52, 90);

/// The dot-matrix origin: where `rsd_pixel`'s destination sits.
pub const DOTMATRIX_ORIGIN: (u16, u16) = (47, 112);

/// The indicators, in the order they are drawn.
pub const INDICATORS: &[Indicator] = &[
    Indicator {
        name: "rsd_s",
        sprite: Sprite {
            src_x: 47,
            src_y: 603,
            w: 8,
            h: 8,
            dest_x: 47,
            dest_y: 102,
        },
        offset: 0x00,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_a",
        sprite: Sprite {
            src_x: 55,
            src_y: 603,
            w: 8,
            h: 8,
            dest_x: 55,
            dest_y: 102,
        },
        offset: 0x01,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_m",
        sprite: Sprite {
            src_x: 63,
            src_y: 603,
            w: 10,
            h: 8,
            dest_x: 63,
            dest_y: 102,
        },
        offset: 0x02,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_sto",
        sprite: Sprite {
            src_x: 73,
            src_y: 603,
            w: 13,
            h: 8,
            dest_x: 73,
            dest_y: 102,
        },
        offset: 0x03,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_math",
        sprite: Sprite {
            src_x: 86,
            src_y: 603,
            w: 17,
            h: 8,
            dest_x: 86,
            dest_y: 102,
        },
        offset: 0x05,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_d",
        sprite: Sprite {
            src_x: 103,
            src_y: 603,
            w: 8,
            h: 8,
            dest_x: 103,
            dest_y: 102,
        },
        offset: 0x06,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_r",
        sprite: Sprite {
            src_x: 111,
            src_y: 603,
            w: 7,
            h: 8,
            dest_x: 111,
            dest_y: 102,
        },
        offset: 0x07,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_g",
        sprite: Sprite {
            src_x: 118,
            src_y: 603,
            w: 9,
            h: 8,
            dest_x: 118,
            dest_y: 102,
        },
        offset: 0x08,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_fix",
        sprite: Sprite {
            src_x: 127,
            src_y: 603,
            w: 14,
            h: 8,
            dest_x: 127,
            dest_y: 102,
        },
        offset: 0x09,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_sci",
        sprite: Sprite {
            src_x: 141,
            src_y: 603,
            w: 15,
            h: 8,
            dest_x: 141,
            dest_y: 102,
        },
        offset: 0x0a,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_e",
        sprite: Sprite {
            src_x: 156,
            src_y: 603,
            w: 9,
            h: 8,
            dest_x: 156,
            dest_y: 102,
        },
        offset: 0x0b,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_cmplx",
        sprite: Sprite {
            src_x: 165,
            src_y: 603,
            w: 6,
            h: 8,
            dest_x: 165,
            dest_y: 102,
        },
        offset: 0x0c,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_angle",
        sprite: Sprite {
            src_x: 171,
            src_y: 603,
            w: 7,
            h: 8,
            dest_x: 171,
            dest_y: 102,
        },
        offset: 0x0d,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_wdown",
        sprite: Sprite {
            src_x: 178,
            src_y: 603,
            w: 11,
            h: 8,
            dest_x: 178,
            dest_y: 102,
        },
        offset: 0x0f,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_left",
        sprite: Sprite {
            src_x: 189,
            src_y: 603,
            w: 7,
            h: 8,
            dest_x: 189,
            dest_y: 102,
        },
        offset: 0x10,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_down",
        sprite: Sprite {
            src_x: 196,
            src_y: 603,
            w: 7,
            h: 8,
            dest_x: 196,
            dest_y: 102,
        },
        offset: 0x11,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_up",
        sprite: Sprite {
            src_x: 203,
            src_y: 603,
            w: 6,
            h: 8,
            dest_x: 203,
            dest_y: 102,
        },
        offset: 0x12,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_right",
        sprite: Sprite {
            src_x: 209,
            src_y: 603,
            w: 9,
            h: 8,
            dest_x: 209,
            dest_y: 102,
        },
        offset: 0x13,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_pause",
        sprite: Sprite {
            src_x: 218,
            src_y: 603,
            w: 12,
            h: 8,
            dest_x: 218,
            dest_y: 102,
        },
        offset: 0x15,
        mask: 0x01,
    },
    Indicator {
        name: "rsd_sun",
        sprite: Sprite {
            src_x: 230,
            src_y: 603,
            w: 9,
            h: 8,
            dest_x: 230,
            dest_y: 102,
        },
        offset: 0x16,
        mask: 0x01,
    },
];

/// The 50 buttons, in the order the face's geometry lists them.
///
/// Hit testing takes the **first** match in this order and stops, so
/// overlapping boxes resolve to the earlier entry
/// (the interval is half-open on the far edge).
pub const BUTTONS: &[Button] = &[
    Button {
        x: 29,
        y: 404,
        w: 41,
        h: 29,
        code: 0x02,
        label: "7",
    },
    Button {
        x: 76,
        y: 404,
        w: 41,
        h: 29,
        code: 0x12,
        label: "8",
    },
    Button {
        x: 123,
        y: 404,
        w: 41,
        h: 29,
        code: 0x22,
        label: "9",
    },
    Button {
        x: 170,
        y: 404,
        w: 41,
        h: 29,
        code: 0x32,
        label: "Backspace",
    },
    Button {
        x: 217,
        y: 404,
        w: 41,
        h: 29,
        code: 0x42,
        label: "Space",
    },
    Button {
        x: 29,
        y: 444,
        w: 41,
        h: 29,
        code: 0x01,
        label: "4",
    },
    Button {
        x: 76,
        y: 444,
        w: 41,
        h: 29,
        code: 0x11,
        label: "5",
    },
    Button {
        x: 123,
        y: 444,
        w: 41,
        h: 29,
        code: 0x21,
        label: "6",
    },
    Button {
        x: 170,
        y: 444,
        w: 41,
        h: 29,
        code: 0x31,
        label: "",
    },
    Button {
        x: 217,
        y: 444,
        w: 41,
        h: 29,
        code: 0x41,
        label: "/",
    },
    Button {
        x: 29,
        y: 484,
        w: 41,
        h: 29,
        code: 0x00,
        label: "1",
    },
    Button {
        x: 76,
        y: 484,
        w: 41,
        h: 29,
        code: 0x10,
        label: "2",
    },
    Button {
        x: 123,
        y: 484,
        w: 41,
        h: 29,
        code: 0x20,
        label: "3",
    },
    Button {
        x: 170,
        y: 484,
        w: 41,
        h: 29,
        code: 0x30,
        label: "=",
    },
    Button {
        x: 217,
        y: 484,
        w: 41,
        h: 29,
        code: 0x40,
        label: "-",
    },
    Button {
        x: 29,
        y: 524,
        w: 41,
        h: 29,
        code: 0x64,
        label: "0",
    },
    Button {
        x: 76,
        y: 524,
        w: 41,
        h: 29,
        code: 0x63,
        label: ".",
    },
    Button {
        x: 123,
        y: 524,
        w: 41,
        h: 29,
        code: 0x62,
        label: "E",
    },
    Button {
        x: 170,
        y: 524,
        w: 41,
        h: 29,
        code: 0x61,
        label: "",
    },
    Button {
        x: 217,
        y: 524,
        w: 41,
        h: 29,
        code: 0x60,
        label: "Return",
    },
    Button {
        x: 29,
        y: 303,
        w: 33,
        h: 20,
        code: 0x05,
        label: "",
    },
    Button {
        x: 67,
        y: 303,
        w: 33,
        h: 20,
        code: 0x15,
        label: "",
    },
    Button {
        x: 105,
        y: 303,
        w: 33,
        h: 20,
        code: 0x25,
        label: "",
    },
    Button {
        x: 143,
        y: 303,
        w: 33,
        h: 20,
        code: 0x35,
        label: "",
    },
    Button {
        x: 181,
        y: 303,
        w: 33,
        h: 20,
        code: 0x45,
        label: "",
    },
    Button {
        x: 219,
        y: 303,
        w: 33,
        h: 20,
        code: 0x55,
        label: "",
    },
    Button {
        x: 29,
        y: 335,
        w: 33,
        h: 20,
        code: 0x04,
        label: "",
    },
    Button {
        x: 67,
        y: 335,
        w: 33,
        h: 20,
        code: 0x14,
        label: "",
    },
    Button {
        x: 105,
        y: 335,
        w: 33,
        h: 20,
        code: 0x24,
        label: "",
    },
    Button {
        x: 143,
        y: 335,
        w: 33,
        h: 20,
        code: 0x34,
        label: "",
    },
    Button {
        x: 181,
        y: 335,
        w: 33,
        h: 20,
        code: 0x44,
        label: "",
    },
    Button {
        x: 219,
        y: 335,
        w: 33,
        h: 20,
        code: 0x54,
        label: "",
    },
    Button {
        x: 29,
        y: 367,
        w: 33,
        h: 20,
        code: 0x03,
        label: "",
    },
    Button {
        x: 67,
        y: 367,
        w: 33,
        h: 20,
        code: 0x13,
        label: "",
    },
    Button {
        x: 105,
        y: 367,
        w: 33,
        h: 20,
        code: 0x23,
        label: "",
    },
    Button {
        x: 143,
        y: 367,
        w: 33,
        h: 20,
        code: 0x33,
        label: "",
    },
    Button {
        x: 181,
        y: 367,
        w: 33,
        h: 20,
        code: 0x43,
        label: "",
    },
    Button {
        x: 219,
        y: 367,
        w: 33,
        h: 20,
        code: 0x53,
        label: "",
    },
    Button {
        x: 29,
        y: 271,
        w: 33,
        h: 20,
        code: 0x06,
        label: "F5",
    },
    Button {
        x: 67,
        y: 271,
        w: 33,
        h: 20,
        code: 0x16,
        label: "F6",
    },
    Button {
        x: 181,
        y: 271,
        w: 33,
        h: 20,
        code: 0x46,
        label: "F7",
    },
    Button {
        x: 219,
        y: 271,
        w: 33,
        h: 20,
        code: 0x56,
        label: "F8",
    },
    Button {
        x: 29,
        y: 222,
        w: 28,
        h: 28,
        code: 0x07,
        label: "F1",
    },
    Button {
        x: 64,
        y: 222,
        w: 28,
        h: 28,
        code: 0x17,
        label: "F2",
    },
    Button {
        x: 193,
        y: 222,
        w: 28,
        h: 28,
        code: 0x47,
        label: "F3",
    },
    Button {
        x: 228,
        y: 222,
        w: 28,
        h: 28,
        code: 0xff,
        label: "F4",
    },
    Button {
        x: 100,
        y: 239,
        w: 24,
        h: 26,
        code: 0x26,
        label: "Left",
    },
    Button {
        x: 162,
        y: 239,
        w: 24,
        h: 26,
        code: 0x37,
        label: "Right",
    },
    Button {
        x: 123,
        y: 224,
        w: 40,
        h: 23,
        code: 0x27,
        label: "Up",
    },
    Button {
        x: 123,
        y: 257,
        w: 40,
        h: 23,
        code: 0x36,
        label: "Down",
    },
];
