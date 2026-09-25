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

//! A debugger engine for the fx-991CN X emulator.
//!
//! This is the layer between the emulator and whatever a person is typing at:
//! execution breakpoints with conditions, memory watchpoints, stepping, rollback
//! snapshots, an instruction trace, and code patches.  It has no user interface of
//! its own, so the same engine drives the `emu dbg` script/REPL front end and would
//! drive a TUI or a window without changing.
//!
//! # What already existed, and what is here
//!
//! `fx991::Emu` already had breakpoints, `step_with`, `run_until` and a CSV
//! [`fx991::Trace`], and those remain the scripting API.  Three things stopped them
//! being a debugger, and this crate is those three:
//!
//! * **Conditions.**  [`condition`] parses and evaluates `if` clauses.  That is why
//!   [`fx991_chipset::Chipset::tick`]'s hook now receives the bus: testing
//!   `[filter] == 0xFF` has to read memory *before* the instruction runs.  Testing
//!   it by re-running the instruction instead would advance the timer divider, and
//!   the machine's determinism is pinned by `parity.rs`.
//! * **Watchpoints.**  `Emu::Breakpoint` only ever matched a PC.  Data watches live
//!   in [`fx991_bus::Bus`], because that is where an access exists.
//! * **State to roll back to.**  [`fx991_chipset::ChipsetSnapshot`] covers the
//!   peripherals, whose state the bus cannot see.  Without it "go back and look
//!   again" is not possible, only "start over".
//!
//! # The subtle decisions
//!
//! * **A stopped machine does not step.**  The ROM parks in STOP for most of its
//!   life, and a tick then returns [`fx991_chipset::TickOutcome::Stopped`] without
//!   executing anything.  [`StopReason::Stopped`] reports that rather than letting
//!   `step` look like it worked; [`StopReason::Stopped`]'s documentation says what
//!   to do about it.
//! * **A data watch stops *after* the access.**  The instruction that wrote is the
//!   interesting one, and it is the one whose PC the report should carry.
//! * **Patches live in the bus's overlay list, and nowhere else.**  A patch has to
//!   survive a rollback, which means the overlay list is the single source of
//!   truth; keeping a second copy in the debugger would be the obvious way to get
//!   the two out of step.  See [`patch::Patches`].
//! * **A snapshot does not include breakpoints.**  They are the debugger's
//!   bookkeeping, not machine state: rolling the machine back must not resurrect a
//!   breakpoint that was cleared in between.

#![warn(missing_docs)]

pub mod breakpoint;
pub mod command;
pub mod condition;
pub mod patch;
pub mod session;
pub mod snapshot;
pub mod trace;
pub mod view;

pub use breakpoint::{Breakpoint, BreakpointId, Watch, WatchId, WatchKind};
pub use command::{CommandError, CommandResult, Session};
pub use condition::{Context, Expr, ParseError};
pub use patch::{Patch, PatchError, Patcher, Patches};
pub use session::{Debugger, StopReason};
pub use snapshot::{Diff, Snapshot, Snapshots};
pub use trace::{Sample, TraceEntry, TraceRing};
