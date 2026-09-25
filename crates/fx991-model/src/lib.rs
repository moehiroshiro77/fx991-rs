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

//! fx-991CN X model data: the measured key table and the variable format.
//!
//! This crate is *data and pure functions about that data*.  It has no CPU, no
//! bus and no peripherals, so it can be used by a disassembler, a keystroke
//! generator, or a test harness just as happily as by the emulator.
//!
//! * [`keys`] -- the measured matrix-to-token table, produced by pressing each
//!   key and reading what the ROM stored.
//! * [`variable`] -- the 10-byte BCD variable format.
//! * [`address`] -- the named RAM addresses the rest of the workspace uses.

#![warn(missing_docs)]

pub mod address;
pub mod keys;
mod keytable;
pub mod variable;

pub use keytable::{Key, KeyTable};
pub use variable::{decode as decode_variable, encode as encode_variable, Decoded};
