// SPDX-License-Identifier: GPL-3.0-only
//
// Copyright (C) 2026 fx991-rs contributors
//
// This program is free software: you can redistribute it and/or modify it under
// the terms of the GNU General Public License as published by the Free Software
// Foundation, version 3.  It is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY
// or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU General Public License in
// LICENSE for more details.

//! The window debugger's panels, composed into an RGBA frame.
//!
//! This crate is the *presentation* half of the debugger: it turns a
//! [`fx991_dbg::Debugger`] and an [`fx991::Emu`] into a picture, in the shape a
//! native debugger uses -- disassembly across the top, memory under it, the
//! register file and the stack down the right, and the breakpoint list along the
//! bottom.
//!
//! # Why the CPU composites
//!
//! The panels are drawn into the same `Rgba8` frame the calculator face uses, so
//! the window side is unchanged: one texture, one textured triangle, no new
//! pipeline.  That also means the whole thing is testable without a GPU, which is
//! what the layout and content tests rely on.
//!
//! # Layout and hit-testing are one thing
//!
//! Panels are grids of character cells.  A row is placed by multiplying the cell
//! height, and a click is turned back into a row by dividing by it, so the two
//! cannot disagree -- see [`layout`].  The font supplies the cell size, and
//! because it is monospace the columns of a hex dump line up.
//!
//! # The font is the user's
//!
//! Nothing is bundled: the face is read from a file at run time, and it must be
//! monospace.  Without it the calculator still runs; only this crate's panels
//! need it.

#![warn(missing_docs)]

pub mod canvas;
pub mod font;
pub mod layout;
pub mod panels;
pub mod theme;
pub mod ui;

pub use canvas::Canvas;
pub use font::{FontError, FontSet};
pub use layout::{Layout, Panel, Rect, MIN_HEIGHT, MIN_WIDTH};
pub use theme::{Metrics, Rgba};
pub use ui::DebuggerUi;
