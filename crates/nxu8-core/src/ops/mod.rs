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

//! Instruction handlers, grouped by family.
//!
//! Each handler is named after the mnemonic family it implements and the module
//! doc above it carries the translation notes, so the functions themselves are
//! deliberately left without per-function docs -- except where a handler has a
//! surprise worth flagging (the dummy stack byte, the CTRL selector packing, the
//! flag-merge mask), and those say so inline.
//!
//! Each opcode row names a handler, and each functional group lives in its own
//! translation unit.  Keeping that split makes the groups diffable one at a time
//! rather than as one large function.
//!
//! Dispatch itself lives on the tag: [`crate::op::Op::run`].  A handler is
//! reached by an array index and a jump rather than by matching its name, and
//! [`Op::name`](crate::op::Op::name) is the one place the printed spelling of a
//! handler lives -- the disassembler and the trace ring read it from there.

#![allow(missing_docs)]

pub mod arith;
pub mod coprocessor;
pub mod ctrl;
pub mod loadstore;
pub mod pushpop;
pub mod shift;
