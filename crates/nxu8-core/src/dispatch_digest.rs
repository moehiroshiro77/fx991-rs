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

//! A checksum of the whole dispatch table.
//!
//! `dispatch_digest()` recomputes this over the live table and compares.  It is the
//! guard that keeps the table and the decoder in step: a stale table, a typo in an
//! operand layout, or a handler renamed without updating the table all move the
//! number.  Storing 64 bits instead of all 65536 entries keeps the check cheap.

//! FNV-1a 64 over every halfword's decoded handler and operand layout.
//!
//! `decode`'s tests recompute the same hash over the Rust table, so a bad
//! regenerated table cannot slip through without one side noticing.

/// Hash of the full 65536-entry dispatch.
pub const DISPATCH_DIGEST: u64 = 0x16774ac19790f1f2;

/// How many halfwords the table maps to a handler.
pub const MAPPED_OPCODES: usize = 52905;
