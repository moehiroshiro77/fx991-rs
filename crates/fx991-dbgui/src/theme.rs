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

//! Colours and metrics for the debugger's panels.
//!
//! Everything a panel draws is measured in **text cells**: a panel's content is a
//! grid of rows and columns, and a click is turned back into a row by the same
//! arithmetic.  That is what keeps hit-testing and drawing from drifting apart --
//! they are the same division, not two implementations of it.
//!
//! The palette is dark and flat, in the shape a native debugger uses: a darker
//! background per panel, a slightly lighter chrome, and a small set of semantic
//! colours for the things a reader scans for (the current instruction, a
//! breakpoint, a changed register).

/// An 8-bit RGBA colour.
pub type Rgba = [u8; 4];

/// The window background, behind every panel.
pub const BACKGROUND: Rgba = [24, 26, 30, 255];
/// Panel background.
pub const PANEL: Rgba = [32, 35, 41, 255];
/// Panel background for the panel the keyboard is aimed at.
pub const PANEL_FOCUS: Rgba = [38, 42, 50, 255];
/// Panel title bar.
pub const TITLE: Rgba = [46, 51, 60, 255];
/// Panel border.
pub const BORDER: Rgba = [64, 70, 82, 255];
/// Ordinary text.
pub const TEXT: Rgba = [214, 219, 228, 255];
/// Text that is present but secondary: addresses, column headers, hints.
pub const TEXT_DIM: Rgba = [138, 146, 162, 255];
/// The instruction the PC is on.
pub const CURRENT: Rgba = [122, 186, 255, 255];
/// The row the cursor is on.
pub const SELECTION: Rgba = [48, 58, 78, 255];
/// A row the user has marked, which is not the current instruction.
pub const CHOSEN: Rgba = [120, 130, 150, 255];
/// An armed breakpoint.
pub const BREAKPOINT: Rgba = [235, 96, 96, 255];
/// A value that changed, or a positive state.
pub const CHANGED: Rgba = [156, 220, 128, 255];
/// A warning or a disabled state.
pub const WARN: Rgba = [232, 178, 92, 255];

/// A cell size in pixels, and the padding around a panel's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    /// Width of one character cell.
    pub cell_w: u32,
    /// Height of one text row.
    pub cell_h: u32,
    /// Padding inside a panel, on every side.
    pub pad: u32,
    /// Height of a panel's title bar.
    pub title_h: u32,
    /// Space between two panels.
    pub gap: u32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            // A monospace face at this size keeps a hex dump readable: the
            // columns line up because every glyph has the same advance.
            cell_w: 8,
            cell_h: 15,
            pad: 6,
            title_h: 20,
            gap: 4,
        }
    }
}

impl Metrics {
    /// How many whole text rows fit in a panel of `height` pixels, after its
    /// title bar and padding are taken out.
    pub fn rows_in(&self, height: u32) -> usize {
        let usable = height.saturating_sub(self.title_h + self.pad * 2);
        (usable / self.cell_h) as usize
    }

    /// How many whole text columns fit in a panel of `width` pixels.
    pub fn cols_in(&self, width: u32) -> usize {
        let usable = width.saturating_sub(self.pad * 2);
        (usable / self.cell_w) as usize
    }
}
