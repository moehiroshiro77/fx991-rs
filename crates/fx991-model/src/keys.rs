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

//! Measured key table for the fx-991CN X matrix.
//!
//! Each row is a matrix code, a name, and the byte the ROM writes into the input
//! area when that key is pressed.  The tokens were measured by pressing each key
//! and reading what the ROM stored, so they are evidence rather than guesswork.
//!
//! The `lua_name` field is the name the hardware's own layout file uses.  It is a
//! *position*, not a function, and several are wrong -- `0x07` is `SHIFT`, not
//! `F1`.  Trust the token.

//! Measured key table for the fx-991CN X matrix.
//!
//! `code` is the matrix value from; `token` is the byte the ROM
//! writes into `ram:0xD180` for that key, or `None` when the key inserts no
//! character.  Names come from decoding the token through the character
//! table in `data/web/rop/keys.py`.

use crate::Key;

/// Every button, with its measured input-area token where there is one.
pub const KEYS: &[Key] = &[
    Key::new(0x00, "1", Some(0x31)),
    Key::new(0x01, "4", Some(0x34)),
    Key::new(0x02, "7", Some(0x37)),
    Key::new(0x10, "2", Some(0x32)),
    Key::new(0x11, "5", Some(0x35)),
    Key::new(0x12, "8", Some(0x38)),
    Key::new(0x20, "3", Some(0x33)),
    Key::new(0x21, "6", Some(0x36)),
    Key::new(0x22, "9", Some(0x39)),
    Key::new(0x64, "0", Some(0x30)),
    Key::new(0x63, ".", Some(0x2e)),
    Key::new(0x62, "x10", Some(0x2d)),
    Key::new(0x61, "Ans", Some(0x41)),
    Key::new(0x30, "+", Some(0xa6)),
    Key::new(0x40, "-", Some(0xa7)),
    Key::new(0x31, "*", Some(0xa8)),
    Key::new(0x41, "/", Some(0xa9)),
    Key::new(0x23, "(", Some(0x60)),
    Key::new(0x33, ")", Some(0xd0)),
    Key::new(0x34, "sin", Some(0x77)),
    Key::new(0x44, "cos", Some(0x78)),
    Key::new(0x54, "tan", Some(0x79)),
    Key::new(0x45, "log(", Some(0x7d)),
    Key::new(0x55, "ln", Some(0x75)),
    Key::new(0x15, "sqrt", Some(0x74)),
    Key::new(0x04, "x^-1", Some(0xc0)),
    Key::new(0x05, "x^2", Some(0xc8)),
    Key::new(0x14, "deg", Some(0xdc)),
    Key::new(0x46, "F7", Some(0x51)),
    Key::new(0x56, "F8", Some(0x48)),
    Key::new(0x24, "10^x", Some(0x19)),
    Key::new(0x25, "e^x", Some(0x19)),
    Key::new(0x35, "abs", Some(0x19)),
    Key::new(0x07, "SHIFT", None),
    Key::new(0x17, "ALPHA", None),
    Key::new(0x03, "STO", None),
    Key::new(0x13, "unnamed-13", None),
    Key::new(0x06, "F5", None),
    Key::new(0x16, "F6", None),
    Key::new(0x26, "LEFT", None),
    Key::new(0x27, "UP", None),
    Key::new(0x36, "DOWN", None),
    Key::new(0x37, "RIGHT", None),
    Key::new(0x32, "DEL", None),
    Key::new(0x42, "AC", None),
    Key::new(0x43, "unnamed-43", None),
    Key::new(0x47, "F3", None),
    Key::new(0x53, "unnamed-53", None),
    Key::new(0x60, "EXE", None),
    Key::new(0xff, "ON", None),
];

/// Names as they appear in.
///
/// One of them is wrong and it matters: it calls code `0x30=`,
/// but that code types token `0xA6`, which decodes as `+`.
pub const MODEL_LUA_NAMES: &[(&str, u8)] = &[
    ("1", 0x00),
    ("4", 0x01),
    ("7", 0x02),
    ("F5", 0x06),
    ("2", 0x10),
    ("5", 0x11),
    ("8", 0x12),
    ("F6", 0x16),
    ("3", 0x20),
    ("6", 0x21),
    ("9", 0x22),
    ("Left", 0x26),
    ("Up", 0x27),
    ("=", 0x30),
    ("Backspace", 0x32),
    ("Down", 0x36),
    ("Right", 0x37),
    ("-", 0x40),
    ("/", 0x41),
    ("F7", 0x46),
    ("F3", 0x47),
    ("F8", 0x56),
    ("Return", 0x60),
    ("E", 0x62),
    (".", 0x63),
    ("0", 0x64),
];

/// The three unnamed blocks in  order (row-major, 6 per row).
pub const UNNAMED_BLOCK_CODES: &[[u8; 6]] = &[
    [0x05, 0x15, 0x25, 0x35, 0x45, 0x55],
    [0x04, 0x14, 0x24, 0x34, 0x44, 0x54],
    [0x03, 0x13, 0x23, 0x33, 0x43, 0x53],
];
