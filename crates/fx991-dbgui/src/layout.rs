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

//! Where the panels are, and which one a click landed on.
//!
//! The arrangement follows a native debugger: disassembly across the top, memory
//! under it, the register file down the right-hand side, and the breakpoint list
//! across the bottom.  Only the two dimensions of the frame decide the
//! rectangles, so the same layout serves any window size and can be asserted on
//! without a GPU.
//!
//! ```text
//! +--------------------------------------------------------------+
//! | toolbar                                                       |
//! +---------------------------------------------+----------------+
//! | disassembly                                  | registers      |
//! |                                              |                |
//! +---------------------------------------------+----------------+
//! | memory                                       | stack          |
//! |                                              |                |
//! +---------------------------------------------+----------------+
//! | breakpoints / watches                                         |
//! +--------------------------------------------------------------+
//! ```
//!
//! The right-hand column is a fixed share of the width rather than a fixed pixel
//! count, so a wider window gives the extra room to the disassembly and the hex
//! dump, which are the panels whose content grows with width.

use crate::theme::Metrics;

/// A rectangle in frame pixels.  Half-open on both axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

impl Rect {
    /// Whether a point is inside.  Half-open, so a shared edge belongs to the
    /// rectangle that starts there rather than to both.
    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    /// The area inside the padding, where content is drawn.
    pub fn content(&self, metrics: &Metrics) -> Rect {
        let pad = metrics.pad;
        Rect {
            x: self.x + pad,
            y: self.y + metrics.title_h + pad,
            w: self.w.saturating_sub(pad * 2),
            h: self.h.saturating_sub(metrics.title_h + pad * 2),
        }
    }

    /// How many text rows fit in the content area.
    pub fn rows(&self, metrics: &Metrics) -> usize {
        metrics.rows_in(self.h)
    }

    /// How many text columns fit in the content area.
    pub fn cols(&self, metrics: &Metrics) -> usize {
        metrics.cols_in(self.w)
    }

    /// Which content row a frame pixel is on, if it is on one.
    ///
    /// The inverse of the arithmetic that places a row, which is what keeps a
    /// click and the thing it lands on in agreement.
    pub fn row_at(&self, metrics: &Metrics, y: u32) -> Option<usize> {
        let content = self.content(metrics);
        if y < content.y || y >= content.y + content.h {
            return None;
        }
        let row = ((y - content.y) / metrics.cell_h) as usize;
        (row < self.rows(metrics)).then_some(row)
    }
}

/// The panels the debugger draws, in the order they are hit-tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    /// The toolbar: run state and the step controls.
    Toolbar,
    /// The disassembly listing.
    Disassembly,
    /// The hex and ASCII memory dump.
    Memory,
    /// The register file.
    Registers,
    /// The stack.
    Stack,
    /// Breakpoints and watches.
    Breakpoints,
}

impl Panel {
    /// Every panel, in the order they are listed.
    pub const ALL: &'static [Panel] = &[
        Panel::Toolbar,
        Panel::Disassembly,
        Panel::Memory,
        Panel::Registers,
        Panel::Stack,
        Panel::Breakpoints,
    ];

    /// The panel's title, as drawn in its title bar.
    pub fn title(self) -> &'static str {
        match self {
            Panel::Toolbar => "fx-991CN X",
            Panel::Disassembly => "Disassembly",
            Panel::Memory => "Memory",
            Panel::Registers => "Registers",
            Panel::Stack => "Stack",
            Panel::Breakpoints => "Breakpoints / Watches",
        }
    }

    /// Whether the panel scrolls and takes keyboard focus.
    ///
    /// The toolbar is a strip of controls rather than a view onto data, so it
    /// neither scrolls nor holds the focus.
    pub fn is_scrollable(self) -> bool {
        !matches!(self, Panel::Toolbar)
    }
}

/// Every panel's rectangle for a frame of this size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The whole frame.
    pub frame: Rect,
    /// The toolbar strip.
    pub toolbar: Rect,
    /// The disassembly panel.
    pub disassembly: Rect,
    /// The memory panel.
    pub memory: Rect,
    /// The register panel.
    pub registers: Rect,
    /// The stack panel.
    pub stack: Rect,
    /// The breakpoint and watch panel.
    pub breakpoints: Rect,
}

/// The width of the right-hand column, as a fraction of the frame.
///
/// A share rather than a constant: the register file has a fixed number of rows
/// and the stack's rows are short, so a wider window should give the extra width
/// to the disassembly and the hex dump.
const RIGHT_COLUMN_NUM: u32 = 30;
/// The divisor for [`RIGHT_COLUMN_NUM`].
const RIGHT_COLUMN_DEN: u32 = 100;
/// The narrowest the right-hand column is allowed to get, in pixels.
const RIGHT_COLUMN_MIN: u32 = 260;
/// The height of the toolbar strip, in pixels.
///
/// A floor rather than the final word: the toolbar shows a title and one status
/// row, so it is grown to fit those when the font's cells are taller than this
/// allows for.  A fixed height with a large font would leave the strip with no
/// room for its row.
const TOOLBAR_H: u32 = 34;
/// The height of the breakpoint strip, in pixels.
const BOTTOM_H: u32 = 120;
/// The narrowest a frame may be before the panels stop being usable.
pub const MIN_WIDTH: u32 = 900;
/// The shortest a frame may be before the panels stop being usable.
pub const MIN_HEIGHT: u32 = 600;

impl Layout {
    /// Lay the panels out in a frame of `width` x `height` pixels.
    ///
    /// The frame is clamped to [`MIN_WIDTH`] x [`MIN_HEIGHT`] first, so a window
    /// squeezed smaller than the panels need still produces a valid layout rather
    /// than zero-height rectangles.  The caller is expected to keep the window at
    /// or above the minimum; this is the belt to that pair of braces.
    pub fn new(width: u32, height: u32, metrics: &Metrics) -> Self {
        let width = width.max(MIN_WIDTH);
        let height = height.max(MIN_HEIGHT);
        let gap = metrics.gap;

        let frame = Rect {
            x: 0,
            y: 0,
            w: width,
            h: height,
        };

        // The toolbar holds a title and one row of status, so it is tall enough
        // for both whatever the font's cell height is.
        let toolbar_h = TOOLBAR_H.max(metrics.title_h + metrics.pad * 2 + metrics.cell_h);

        // A right column that is a share of the width, but never narrower than
        // the register names plus a value.
        let right_w = (width * RIGHT_COLUMN_NUM / RIGHT_COLUMN_DEN).max(RIGHT_COLUMN_MIN);
        let left_w = width.saturating_sub(right_w + gap);

        // The two rows of the left column and of the right column split the
        // remaining height evenly; the register file and the stack both want
        // about the same room.
        let body_y = toolbar_h + gap;
        let body_h = height.saturating_sub(toolbar_h + BOTTOM_H + gap * 3);
        let upper_h = body_h / 2;
        let lower_h = body_h - upper_h;

        let lower_y = body_y + upper_h + gap;
        let bottom_y = lower_y + lower_h + gap;
        let right_x = left_w + gap;

        Self {
            frame,
            toolbar: Rect {
                x: 0,
                y: 0,
                w: width,
                h: toolbar_h,
            },
            disassembly: Rect {
                x: 0,
                y: body_y,
                w: left_w,
                h: upper_h,
            },
            memory: Rect {
                x: 0,
                y: lower_y,
                w: left_w,
                h: lower_h,
            },
            registers: Rect {
                x: right_x,
                y: body_y,
                w: right_w,
                h: upper_h,
            },
            stack: Rect {
                x: right_x,
                y: lower_y,
                w: right_w,
                h: lower_h,
            },
            breakpoints: Rect {
                x: 0,
                y: bottom_y,
                w: width,
                h: height.saturating_sub(bottom_y),
            },
        }
    }

    /// The rectangle of one panel.
    pub fn rect(&self, panel: Panel) -> Rect {
        match panel {
            Panel::Toolbar => self.toolbar,
            Panel::Disassembly => self.disassembly,
            Panel::Memory => self.memory,
            Panel::Registers => self.registers,
            Panel::Stack => self.stack,
            Panel::Breakpoints => self.breakpoints,
        }
    }

    /// Which panel contains a point.
    ///
    /// The toolbar is checked last: it is a full-width strip at the top and the
    /// panels below it never overlap it, so this only decides the case where a
    /// point is outside every content panel.
    pub fn panel_at(&self, x: u32, y: u32) -> Option<Panel> {
        for panel in [
            Panel::Disassembly,
            Panel::Memory,
            Panel::Registers,
            Panel::Stack,
            Panel::Breakpoints,
        ] {
            if self.rect(panel).contains(x, y) {
                return Some(panel);
            }
        }
        self.toolbar.contains(x, y).then_some(Panel::Toolbar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::new(1280, 800, &Metrics::default())
    }

    #[test]
    fn the_panels_tile_the_frame_without_overlapping() {
        let layout = layout();
        // Every panel is inside the frame.
        for panel in Panel::ALL {
            let rect = layout.rect(*panel);
            assert!(
                rect.x + rect.w <= layout.frame.w && rect.y + rect.h <= layout.frame.h,
                "{panel:?} {rect:?} escapes the frame"
            );
        }
        // No two panels overlap.
        for (index, first) in Panel::ALL.iter().enumerate() {
            for second in &Panel::ALL[index + 1..] {
                let a = layout.rect(*first);
                let b = layout.rect(*second);
                let disjoint =
                    a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
                assert!(disjoint, "{first:?} {a:?} overlaps {second:?} {b:?}");
            }
        }
    }

    #[test]
    fn the_disassembly_gets_the_extra_width() {
        // The right column is a share, so growing the window widens the left
        // panels rather than the right one.
        let narrow = Layout::new(MIN_WIDTH, MIN_HEIGHT, &Metrics::default());
        let wide = Layout::new(1920, MIN_HEIGHT, &Metrics::default());
        let narrow_left = narrow.disassembly.w;
        let wide_left = wide.disassembly.w;
        let narrow_right = narrow.registers.w;
        let wide_right = wide.registers.w;
        assert!(wide_left > narrow_left, "the left column grew");
        // The right column may grow too, but by much less.
        assert!(
            wide_left - narrow_left > wide_right.saturating_sub(narrow_right),
            "the extra width should favour the disassembly"
        );
    }

    #[test]
    fn a_click_is_attributed_to_the_panel_it_landed_in() {
        let layout = layout();
        // The centre of each panel belongs to that panel.
        for panel in Panel::ALL {
            let rect = layout.rect(*panel);
            let (x, y) = (rect.x + rect.w / 2, rect.y + rect.h / 2);
            assert_eq!(layout.panel_at(x, y), Some(*panel), "centre of {panel:?}");
        }
        // A point outside every panel, in the gap between two, hits nothing.
        let gap_x = layout.disassembly.w + 1;
        let gap_y = layout.disassembly.y + 4;
        assert_eq!(
            layout.panel_at(gap_x, gap_y),
            None,
            "the gap between the columns is not a panel"
        );
    }

    #[test]
    fn a_tiny_frame_still_lays_out() {
        // A window squeezed below the minimum is clamped rather than producing
        // zero-height panels.
        let layout = Layout::new(10, 10, &Metrics::default());
        assert_eq!(layout.frame.w, MIN_WIDTH);
        assert_eq!(layout.frame.h, MIN_HEIGHT);
        for panel in Panel::ALL {
            let rect = layout.rect(*panel);
            assert!(rect.w > 0 && rect.h > 0, "{panel:?} collapsed: {rect:?}");
            assert!(
                rect.rows(&Metrics::default()) > 0,
                "{panel:?} shows no rows"
            );
        }
    }

    #[test]
    fn a_row_is_found_by_the_same_arithmetic_that_places_it() {
        let metrics = Metrics::default();
        let layout = layout();
        let rect = layout.disassembly;
        let content = rect.content(&metrics);
        let rows = rect.rows(&metrics);
        assert!(rows > 0);

        for row in 0..rows {
            let y = content.y + row as u32 * metrics.cell_h;
            assert_eq!(rect.row_at(&metrics, y), Some(row), "top of row {row}");
            // Any pixel inside the row's band maps to it.
            let last = content.y + (row as u32 + 1) * metrics.cell_h - 1;
            assert_eq!(
                rect.row_at(&metrics, last),
                Some(row),
                "bottom of row {row}"
            );
        }

        // The title bar and the padding above the first row belong to no row.
        assert_eq!(rect.row_at(&metrics, rect.y), None, "the title bar");
        assert_eq!(rect.row_at(&metrics, content.y - 1), None, "the padding");
        // And nothing below the last row does either.
        assert_eq!(rect.row_at(&metrics, content.y + rect.h), None);
    }

    #[test]
    fn the_toolbar_is_not_scrollable() {
        assert!(!Panel::Toolbar.is_scrollable());
        for panel in Panel::ALL.iter().filter(|p| **p != Panel::Toolbar) {
            assert!(panel.is_scrollable(), "{panel:?} should scroll");
        }
    }
}
