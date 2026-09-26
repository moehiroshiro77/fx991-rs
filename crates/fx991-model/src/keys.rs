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

// @generated -- do not edit by hand.
//
// Measured from the emulator itself: press a matrix code, wait for the ROM to
// accept it, then read the input area.  Every entry with a token returned the same
// byte on two separate boots, which is what makes this evidence rather than a
// guess; the SHIFT and ALPHA notes under an entry were measured the same way, with
// the modifier pressed first.

//! Generated, not hand-written: see the header above for how the values were
//! measured.  Editing this file by hand would be lost the next time the table is
//! regenerated, so change the measurement instead.
//!
//! Measured key table for the fx-991CN X matrix.
//!
//! `code` is the matrix value from the emulator's own key map; `token` is the
//! byte the ROM writes into `ram:0xD180` for that key, or `None` when the key
//! inserts no character.  Names come from decoding the token through the
//! table published with the VerC ROP tooling.
//!
//! A `//   SHIFT ->` or `//   ALPHA ->` line under an entry records what that
//! key becomes with the modifier held.  Those are *notes*: a caller still
//! presses one key at a time, and the ROM carries the modifier into the next
//! press.  A blank note means the combination inserts nothing -- the key
//! either ignores that layer or opens a menu instead.  Tokens in parentheses
//! are the bytes written, where `1A`/`1B` marks a fillable box, `1C` separates
//! two boxes of one template, and `1D` is a fraction bar.

use crate::Key;

/// Every button, with its measured input-area token where there is one.
pub const KEYS: &[Key] = &[
    Key::new(0x00, "1", Some(0x31)),
    Key::new(0x01, "4", Some(0x34)),
    Key::new(0x02, "7", Some(0x37)),
    //   SHIFT -> constants menu (opens a menu)
    Key::new(0x10, "2", Some(0x32)),
    Key::new(0x11, "5", Some(0x35)),
    Key::new(0x12, "8", Some(0x38)),
    //   SHIFT -> unit-conversion menu (opens a menu)
    Key::new(0x20, "3", Some(0x33)),
    Key::new(0x21, "6", Some(0x36)),
    Key::new(0x22, "9", Some(0x39)),
    //   SHIFT -> reset menu (opens a menu)
    Key::new(0x64, "0", Some(0x30)),
    //   SHIFT -> Rnd (69)
    Key::new(0x63, ".", Some(0x2e)),
    //   SHIFT -> Ran# (FD 18)
    //   ALPHA -> RanInt (87)
    Key::new(0x62, "x10", Some(0x2d)),
    //   SHIFT -> pi (22)
    //   ALPHA -> e (21)
    Key::new(0x61, "Ans", Some(0x41)),
    //   SHIFT -> % (D7)
    //   ALPHA -> ANS (41)
    Key::new(0x30, "+", Some(0xa6)),
    //   SHIFT -> angle symbol (7E)
    Key::new(0x40, "-", Some(0xa7)),
    //   SHIFT -> greater-than (7F)
    Key::new(0x31, "*", Some(0xa8)),
    //   SHIFT -> nPr (AD)
    Key::new(0x41, "/", Some(0xa9)),
    //   SHIFT -> nCr (AE)
    Key::new(0x23, "(", Some(0x60)),
    //   SHIFT -> Abs (68 1A 19 1B)
    Key::new(0x33, ")", Some(0xd0)),
    //   SHIFT -> comma (2C)
    //   ALPHA -> x (48)
    Key::new(0x34, "sin", Some(0x77)),
    //   SHIFT -> sin-1 (7A)
    //   ALPHA -> D (45)
    Key::new(0x44, "cos", Some(0x78)),
    //   SHIFT -> cos-1 (7B)
    //   ALPHA -> E (46)
    Key::new(0x54, "tan", Some(0x79)),
    //   SHIFT -> tan-1 (7C)
    //   ALPHA -> F (47)
    Key::new(0x45, "log(a,b)", Some(0x7d)),
    //   SHIFT -> 10^ (73 1A 19 1B)
    Key::new(0x55, "ln", Some(0x75)),
    //   SHIFT -> e^ (72 1A 19 1B)
    Key::new(0x15, "sqrt", Some(0x74)),
    //   SHIFT -> x-root template (CA 1D 1A 33 1B..)
    Key::new(0x04, "(-)", Some(0xc0)),
    //   SHIFT -> log (7D, single argument)
    //   ALPHA -> A (42)
    Key::new(0x05, "fraction", Some(0xc8)),
    //   SHIFT -> a b/c fraction (18 1F 1D..)
    //   ALPHA -> Reciprocal (AA)
    Key::new(0x14, "deg", Some(0xdc)),
    //   ALPHA -> B (43)
    Key::new(0x46, "integral", Some(0x51)),
    //   SHIFT -> d/dx (52 1A 19 1C 19 1B)
    //   ALPHA -> : (23)
    Key::new(0x56, "x", Some(0x48)),
    //   SHIFT -> summation (50 1A 19 1C 19 1C 19 1B)
    Key::new(0x24, "x^-1", Some(0x19)),
    //   SHIFT -> x! factoral (D8)
    //   ALPHA -> C (44)
    Key::new(0x25, "x^2", Some(0x19)),
    //   SHIFT -> x^3 (19 C9 1A 33 1B)
    Key::new(0x35, "x^y", Some(0x19)),
    //   SHIFT -> x-root (CA 1D 1A 19 1B..)
    //   ALPHA -> RanInt (AA)
    Key::new(0x07, "SHIFT", None),
    Key::new(0x17, "ALPHA", None),
    Key::new(0x03, "STO", None),
    //   SHIFT -> power-off menu (opens a menu)
    Key::new(0x13, "ENG", None),
    Key::new(0x06, "OPTN", None),
    Key::new(0x16, "CALC", None),
    //   ALPHA -> = (A5), i.e. SOLVE
    Key::new(0x26, "LEFT", None),
    Key::new(0x27, "UP", None),
    Key::new(0x36, "DOWN", None),
    Key::new(0x37, "RIGHT", None),
    Key::new(0x32, "DEL", None),
    Key::new(0x42, "AC", None),
    //   SHIFT -> power-off menu (opens a menu)
    Key::new(0x43, "S<=>D", None),
    //   ALPHA -> y (49)
    Key::new(0x47, "F3", None),
    Key::new(0x53, "unnamed-53", None),
    //   ALPHA -> M (40)
    Key::new(0x60, "EXE", None),
    Key::new(0xff, "ON", None),
];

/// Names as they appear in the emulator's own key map.
///
/// Several are wrong and it matters: the file's author picked SDL key names by
/// screen position, not by what the key does.  It calls `0x30` `=`, for
/// instance, but that code types token `0xA6`, which decodes as `+`.
pub const KEYMAP_NAMES: &[(&str, u8)] = &[
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

/// The three unnamed blocks in key-map order (row-major, 6 per row).
pub const UNNAMED_BLOCK_CODES: &[[u8; 6]] = &[
    [0x05, 0x15, 0x25, 0x35, 0x45, 0x55],
    [0x04, 0x14, 0x24, 0x34, 0x44, 0x54],
    [0x03, 0x13, 0x23, 0x33, 0x43, 0x53],
];
