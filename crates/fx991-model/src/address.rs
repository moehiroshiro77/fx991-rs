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

//! Named addresses, from -spans.txt` and the disassembly.
//!
//! Only addresses that something else in this workspace actually uses are listed.
//! The full map is `mem-spans.txt`; this is the part that is load-bearing.

/// Linear input area: 200 bytes (`mem-spans.txt`, and `LABEL_INPUT_BUF` in the
/// reference emulator's).
pub const INPUT_BUFFER: u32 = 0x0_D180;

/// Length of the linear input area.
pub const INPUT_BUFFER_LEN: u32 = 200;

/// Replay area, 200 bytes; the VerC tooling calls it `0xD248`.
///
/// VerF has no literal reference to it, so treat it as unconfirmed.
pub const REPLAY_BUFFER: u32 = 0x0_D248;

/// Random seed, 10 bytes.
pub const RANDOM_SEED: u32 = 0x0_D310;

/// Little-endian counter, 2 bytes.
pub const COUNTER: u32 = 0x0_D318;

/// First variable slot (M).  Twelve slots of 10 bytes follow.
pub const VARIABLES: u32 = 0x0_D31A;

/// `Ans`, the second slot.
pub const VARIABLE_ANS: u32 = VARIABLES + 10;

/// The variable slot order, as `mem-spans.txt` lists it.
pub const VARIABLE_ORDER: [&str; 12] = [
    "M", "Ans", "A", "B", "C", "D", "E", "F", "x", "y", "PreAns", "@",
];

/// Bytes per variable slot.
pub const VARIABLE_SIZE: u32 = 10;

/// Screen row / font selector; 111 references in the disassembly.
pub const SCREEN_ROW: u32 = 0x0_D137;

/// Screen buffer mirror in RAM (`0xDDD4`, `mem-spans.txt`).
pub const SCREEN_MIRROR_A: u32 = 0x0_DDD4;
/// Screen buffer mirror in RAM (`0xE3D4`, `mem-spans.txt`).
pub const SCREEN_MIRROR_B: u32 = 0x0_E3D4;

/// Input-method selection variable.
///
/// The main loop at `rom:024E80` dispatches on this; the only writers in the whole
/// ROM are `rom:00ECB0` (restore path, writes `193`) and `rom:01F7CE` (writes
/// `R8`, which `rom:01F7DA` compares against 3/6/7/69/74/75/136/193).  See
///
pub const INPUT_MODE: u32 = 0x0_D111;

/// The cursor/counter the VerC notes call `x`; `rom:01F49A` increments it through
/// `[EA]` and `rom:0233B0` decrements it.
pub const CURSOR: u32 = 0x0_D5EB;

/// Keyboard input filter (`0xF042`); the ROM arms it inside its key-wait loop.
pub const KEY_INPUT_FILTER: u32 = 0x0_F042;

/// Value the ROM writes to [`KEY_INPUT_FILTER`] when it is ready for a key.
pub const KEY_INPUT_FILTER_ARMED: u8 = 0xFF;

/// The address the ROM lands on when it has accepted a key (`rom:0F968`).
///
/// Useful as a breakpoint: everything before it is "did we see the key at all",
/// everything after is "what did the ROM do with it".
pub const ROM_KEY_ACCEPTED: u32 = 0x0_F968;

/// The ROM's idle STOP loop (`rom:09216`).
pub const ROM_IDLE: u32 = 0x0_9216;

/// The ROM's reset entry trampoline (`rom:0946A`).
pub const ROM_RESET_ENTRY: u32 = 0x0_946A;
