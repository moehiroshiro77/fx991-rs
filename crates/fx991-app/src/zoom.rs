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

//! Window geometry: how big the window is, and how big it may get.
//!
//! The zoom is kept in quarter steps as an integer so that repeated `+`/`-`
//! cannot accumulate a rounding error, and so that every step is a visible but
//! modest change -- whole steps would double the window at 1x to 2x.

use fx991_ui::{NATURAL_HEIGHT, NATURAL_WIDTH};

/// Quarter steps per whole unit of zoom.
pub const QUARTERS_PER_UNIT: u32 = 4;

/// The smallest zoom: three quarters, so the whole calculator fits on a small
/// screen without shrinking the dot matrix to mush.
pub const MIN_QUARTERS: u32 = 3;

/// The largest zoom.  Two times is the useful ceiling: the skin is already
/// readable at 1x, and a taller window does not fit a 1080p display (2x is
/// 1194px against 1080px of screen).
pub const MAX_QUARTERS: u32 = 8; // 2x

/// The zoom ceiling imposed by the GPU's maximum texture size.
///
/// `Surface:configure` validates the window size against the device's limits
/// and *panics* when it does not fit, so the cap has to respect them.  With the
/// zoom range capped at 2x (1194px) this never bites on a normal device, but an
/// old or software adapter reporting 1024 would otherwise crash on `+`; here it
/// degrades to a smaller maximum instead.
///
/// The whole window has to fit, not just the client area, hence the margin for
/// the title bar and borders.
pub fn max_quarters_for_texture_limit(max_texture_dimension: u32) -> u32 {
    /// Room for the title bar and borders, in pixels.
    const CHROME: u32 = 120;
    let usable = max_texture_dimension.saturating_sub(CHROME);
    let quarters = usable * QUARTERS_PER_UNIT / NATURAL_HEIGHT;
    quarters.clamp(MIN_QUARTERS, MAX_QUARTERS)
}

pub fn clamp_quarters(quarters: u32) -> u32 {
    quarters.clamp(MIN_QUARTERS, MAX_QUARTERS)
}

/// The window size for a zoom given in quarter steps.
///
/// Rounded to whole pixels: a fractional client size would put the dot matrix
/// on half pixels and blur it.
pub fn window_size_for(quarters: u32) -> (u32, u32) {
    let q = clamp_quarters(quarters);
    (
        (NATURAL_WIDTH * q).div_ceil(QUARTERS_PER_UNIT),
        (NATURAL_HEIGHT * q).div_ceil(QUARTERS_PER_UNIT),
    )
}

/// The zoom to open at, given the monitor's work area, in quarter steps.
///
/// Picks the largest zoom whose window still fits, leaving room for the title
/// bar and the taskbar.  Quarter steps, so a display that does not fit a whole
/// multiple still gets a sensible size instead of dropping a full step.
///
/// `work` is the usable area in physical pixels (`monitor.size` minus the
/// chrome the window manager adds).
pub fn fit_quarters(work_width: u32, work_height: u32) -> u32 {
    // Reserve room for the title bar; the caller already excluded the taskbar.
    const CHROME: u32 = 80;
    let usable_h = work_height.saturating_sub(CHROME);

    // How many quarters fit: floor(usable / (natural / 4)).
    let by_height = usable_h * QUARTERS_PER_UNIT / NATURAL_HEIGHT;
    let by_width = work_width * QUARTERS_PER_UNIT / NATURAL_WIDTH;
    clamp_quarters(by_height.min(by_width))
}
