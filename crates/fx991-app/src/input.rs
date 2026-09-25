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

//! Mouse input, translated into key presses.
//!
//! This is deliberately separate from the window: it takes plain `(x, y)`
//! coordinates in *window pixels* and a viewport size, and it needs no GPU or
//! event loop to test.   is the spec.
//!
//! # Why the mapping is computed, not assumed
//!
//! The obvious way to turn a click into a skin coordinate is `x / zoom`, where
//! `zoom` is the factor the window was opened at.  That is wrong as soon as the
//! window is not exactly `zoom * skin_size`: the window manager's title bar and
//! borders change the *client* size, a high-DPI display scales it again, and a
//! user who maximises the window (or resizes it) breaks the assumption outright.
//! The click then lands on the wrong key, and how wrong depends on the monitor.
//!
//! So the mapping is derived from the **viewport** -- the client area we are
//! actually drawing into -- using [`Viewport::fit`].  The skin is drawn centred
//! and aspect-preserved, and a click is mapped back through the same rectangle.
//! It is correct at any window size, on any display, with or without a title
//! bar, and it degrades sensibly if the window is a different shape than the
//! calculator: the letterboxed margins simply count as "outside".

use fx991::Chipset;
use fx991_ui::{button_at, NATURAL_HEIGHT, NATURAL_WIDTH};

/// Which mouse button was involved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
}

/// What a click did, for logging and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A key went down (left button) or its latch toggled (right button).
    Pressed { code: u8, stuck: bool },
    /// Every unlatched key came up.
    ReleasedAll,
    /// The click landed between buttons.
    Missed,
    /// The click landed outside the drawn body (a letterbox margin, say).
    Outside,
}

/// Where the skin is drawn inside the client area, and at what zoom.
///
/// The skin keeps its aspect ratio and is centred; `scale` is a *real* number
/// because a client area is rarely an exact multiple of the skin size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// Left edge of the drawn skin, in client pixels.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Horizontal zoom actually used.
    pub scale: f64,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            scale: 1.0,
        }
    }
}

impl Viewport {
    /// Fit the skin into a client area of `width` x `height` pixels.
    ///
    /// Uses `min` of the two axis ratios so the whole skin is visible, then
    /// centres it.  A zero-sized client area (which Windows reports while a
    /// window is minimised) collapses to a 1:1 viewport rather than dividing by
    /// zero.
    pub fn fit(width: u32, height: u32) -> Self {
        if width == 0 || height == 0 {
            return Self::default();
        }
        let sx = width as f64 / NATURAL_WIDTH as f64;
        let sy = height as f64 / NATURAL_HEIGHT as f64;
        let scale = sx.min(sy);
        let drawn_w = NATURAL_WIDTH as f64 * scale;
        let drawn_h = NATURAL_HEIGHT as f64 * scale;
        Self {
            x: (width as f64 - drawn_w) / 2.0,
            y: (height as f64 - drawn_h) / 2.0,
            scale,
        }
    }

    /// Map a client-space point to skin coordinates.
    ///
    /// Returns `None` when the point falls in a letterbox margin, i.e. outside
    /// the drawn body.
    #[allow(clippy::wrong_self_convention)] // `to_` reads better than `skin_at`
    pub fn to_skin(&self, x: f64, y: f64) -> Option<(i32, i32)> {
        if self.scale <= 0.0 {
            return None;
        }
        let sx = (x - self.x) / self.scale;
        let sy = (y - self.y) / self.scale;
        if sx < 0.0 || sy < 0.0 {
            return None;
        }
        let (sx, sy) = (sx.floor(), sy.floor());
        if sx >= NATURAL_WIDTH as f64 || sy >= NATURAL_HEIGHT as f64 {
            return None;
        }
        Some((sx as i32, sy as i32))
    }
}

/// The mouse state machine.
///
/// Holds the viewport, so it stays correct across resizes.
#[derive(Debug, Default)]
pub struct Mouse {
    /// Where the skin is drawn.  Updated on every resize.
    pub viewport: Viewport,
}

impl Mouse {
    /// A mouse for a client area of the given size.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            viewport: Viewport::fit(width, height),
        }
    }

    /// Tell the mouse the client area changed.
    pub fn set_client_size(&mut self, width: u32, height: u32) {
        self.viewport = Viewport::fit(width, height);
    }

    /// Convert client coordinates to skin coordinates, if inside the body.
    pub fn to_skin(&self, x: f64, y: f64) -> Option<(i32, i32)> {
        self.viewport.to_skin(x, y)
    }

    /// Handle a button going down at client coordinates `(x, y)`.
    pub fn press(&self, chipset: &mut Chipset, button: MouseButton, x: f64, y: f64) -> Action {
        let Some((sx, sy)) = self.to_skin(x, y) else {
            return Action::Outside;
        };
        self.press_skin(chipset, button, sx, sy)
    }

    /// [`Mouse::press`] in skin coordinates.
    ///
    /// A left click presses the key and holds it until the button comes up.  A
    /// right click *toggles* a latch instead, which is
    /// how you hold `SHIFT` while clicking something else.
    pub fn press_skin(&self, chipset: &mut Chipset, button: MouseButton, x: i32, y: i32) -> Action {
        let Some(hit) = button_at(x, y) else {
            return if inside_skin(x, y) {
                Action::Missed
            } else {
                Action::Outside
            };
        };
        let code = hit.code;
        let stick = button == MouseButton::Right;
        if !chipset.press_key_with(code, stick) {
            return Action::Missed;
        }
        Action::Pressed {
            code,
            stuck: chipset.keyboard.borrow().is_stuck(code),
        }
    }

    /// Handle the left button coming up: release everything not latched.
    pub fn release(&self, chipset: &mut Chipset) -> Action {
        chipset.keyboard.borrow_mut().release(None);
        Action::ReleasedAll
    }
}

/// Whether a skin-space point is inside the drawn body.
fn inside_skin(x: i32, y: i32) -> bool {
    x >= 0 && y >= 0 && (x as u32) < NATURAL_WIDTH && (y as u32) < NATURAL_HEIGHT
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x viewport: the window is exactly twice the skin.
    fn vp2x() -> Viewport {
        Viewport::fit(NATURAL_WIDTH * 2, NATURAL_HEIGHT * 2)
    }

    fn chipset() -> Chipset {
        Chipset::new(fx991_bus::Bus::new(fx991_bus::Rom::new(vec![0u8; 1024])))
    }

    #[test]
    fn an_exact_multiple_maps_one_to_one() {
        let vp = vp2x();
        assert_eq!(vp.scale, 2.0);
        assert_eq!(vp.x, 0.0);
        assert_eq!(vp.y, 0.0);
        // Client (0,0) is skin (0,0); client (2,4) is skin (1,2).
        assert_eq!(vp.to_skin(0.0, 0.0), Some((0, 0)));
        assert_eq!(vp.to_skin(2.0, 4.0), Some((1, 2)));
        assert_eq!(vp.to_skin(3.9, 5.9), Some((1, 2)));
        assert_eq!(vp.to_skin(4.0, 6.0), Some((2, 3)));
    }

    #[test]
    fn a_non_integer_size_still_maps_correctly() {
        // A real client area is rarely an exact multiple: 600x1200 is 2.11x
        // horizontally but 2.01x vertically, so the fit picks the smaller and
        // letterboxes the sides.  A naive `x / 2` would be off by up to 5%.
        let vp = Viewport::fit(600, 1200);
        assert!((vp.scale - 1200.0 / NATURAL_HEIGHT as f64).abs() < 1e-9);
        assert!(vp.x > 0.0, "the sides should be letterboxed");

        // The centre of the drawn skin must map to the centre of the skin.
        let cx = vp.x + NATURAL_WIDTH as f64 * vp.scale / 2.0;
        let cy = vp.y + NATURAL_HEIGHT as f64 * vp.scale / 2.0;
        let (sx, sy) = vp.to_skin(cx, cy).expect("centre is inside");
        assert_eq!(sx, NATURAL_WIDTH as i32 / 2);
        assert_eq!(sy, NATURAL_HEIGHT as i32 / 2);

        // And a point in the left margin is outside, not skin x=0.
        assert_eq!(vp.to_skin(1.0, cy), None);
    }

    #[test]
    fn the_letterbox_margins_are_outside() {
        // A window far wider than the skin: big margins on both sides.
        let vp = Viewport::fit(NATURAL_WIDTH * 4, NATURAL_HEIGHT);
        assert_eq!(vp.scale, 1.0);
        assert!(vp.x > 100.0);
        assert_eq!(vp.to_skin(0.0, 300.0), None, "left margin");
        assert_eq!(
            vp.to_skin(vp.x - 1.0, 300.0),
            None,
            "just inside the margin"
        );
        assert_eq!(vp.to_skin(vp.x, 300.0), Some((0, 300)), "first skin column");
    }

    #[test]
    fn a_zero_sized_client_area_does_not_divide_by_zero() {
        // Windows reports 0x0 while a window is minimised.
        let vp = Viewport::fit(0, 0);
        assert_eq!(vp.scale, 1.0);
        assert_eq!(vp.to_skin(0.0, 0.0), Some((0, 0)));
    }

    #[test]
    fn clicks_past_the_skin_edge_are_outside() {
        let vp = vp2x();
        assert_eq!(vp.to_skin(NATURAL_WIDTH as f64 * 2.0, 0.0), None);
        assert_eq!(vp.to_skin(0.0, NATURAL_HEIGHT as f64 * 2.0), None);
        // The last valid pixel is one short of the edge.
        assert_eq!(
            vp.to_skin(
                NATURAL_WIDTH as f64 * 2.0 - 1.0,
                NATURAL_HEIGHT as f64 * 2.0 - 1.0
            ),
            Some((NATURAL_WIDTH as i32 - 1, NATURAL_HEIGHT as i32 - 1))
        );
    }

    #[test]
    fn the_mouse_follows_a_resize() {
        let mut mouse = Mouse::new(NATURAL_WIDTH * 2, NATURAL_HEIGHT * 2);
        let mut chipset = chipset();
        // '7' is at skin (29, 404); at 2x that is client (58, 808).
        assert_eq!(
            mouse.press(&mut chipset, MouseButton::Left, 58.0, 808.0),
            Action::Pressed {
                code: 0x02,
                stuck: false
            }
        );
        mouse.release(&mut chipset);

        // Shrink the window to 1x: the same skin key is now at client (29, 404).
        mouse.set_client_size(NATURAL_WIDTH, NATURAL_HEIGHT);
        assert_eq!(
            mouse.press(&mut chipset, MouseButton::Left, 29.0, 404.0),
            Action::Pressed {
                code: 0x02,
                stuck: false
            }
        );
    }

    #[test]
    fn a_click_in_the_margin_presses_nothing() {
        let mouse = Mouse::new(NATURAL_WIDTH * 4, NATURAL_HEIGHT);
        let mut chipset = chipset();
        assert_eq!(
            mouse.press(&mut chipset, MouseButton::Left, 5.0, 300.0),
            Action::Outside
        );
        assert!(!chipset.keyboard.borrow().has_input);
    }

    #[test]
    fn a_click_between_buttons_misses() {
        let mouse = Mouse::new(NATURAL_WIDTH, NATURAL_HEIGHT);
        // The 6px gutter between '7' (x 29.69) and '8' (x 76.116).
        assert_eq!(
            mouse.press_skin(&mut chipset(), MouseButton::Left, 72, 410),
            Action::Missed
        );
    }

    #[test]
    fn a_left_click_presses_and_the_release_lets_go() {
        let mouse = Mouse::new(NATURAL_WIDTH, NATURAL_HEIGHT);
        let mut chipset = chipset();
        assert_eq!(
            mouse.press_skin(&mut chipset, MouseButton::Left, 49, 418),
            Action::Pressed {
                code: 0x02,
                stuck: false
            }
        );
        assert!(chipset.keyboard.borrow().is_pressed(0x02));
        mouse.release(&mut chipset);
        assert!(!chipset.keyboard.borrow().is_pressed(0x02));
    }

    #[test]
    fn a_right_click_latches_and_a_second_one_clears_it() {
        let mouse = Mouse::new(NATURAL_WIDTH, NATURAL_HEIGHT);
        let mut chipset = chipset();
        assert_eq!(
            mouse.press_skin(&mut chipset, MouseButton::Right, 49, 418),
            Action::Pressed {
                code: 0x02,
                stuck: true
            }
        );
        mouse.release(&mut chipset);
        assert!(
            chipset.keyboard.borrow().is_pressed(0x02),
            "release-all cleared a latched key"
        );
        mouse.press_skin(&mut chipset, MouseButton::Right, 49, 418);
        assert!(!chipset.keyboard.borrow().is_pressed(0x02));
    }

    #[test]
    fn a_latched_shift_survives_clicking_another_key() {
        let mouse = Mouse::new(NATURAL_WIDTH, NATURAL_HEIGHT);
        let mut chipset = chipset();
        // SHIFT is at (29, 222) 28x28.
        mouse.press_skin(&mut chipset, MouseButton::Right, 40, 235);
        assert!(chipset.keyboard.borrow().is_stuck(0x07));
        // A left click on sin, at (143, 303) 33x20.
        mouse.press_skin(&mut chipset, MouseButton::Left, 160, 313);
        mouse.release(&mut chipset);
        assert!(
            chipset.keyboard.borrow().is_pressed(0x07),
            "SHIFT came unstuck when another key was clicked"
        );
    }

    #[test]
    fn clicking_the_power_key_resets_the_machine() {
        let mouse = Mouse::new(NATURAL_WIDTH, NATURAL_HEIGHT);
        let mut chipset = chipset();
        chipset.cpu.regs.sp = 0x1234;
        // ON is at (228, 222) 28x28.
        assert_eq!(
            mouse.press_skin(&mut chipset, MouseButton::Left, 240, 235),
            Action::Pressed {
                code: 0xFF,
                stuck: false
            }
        );
        assert_ne!(chipset.cpu.regs.sp, 0x1234, "the power key did not reset");
    }

    #[test]
    fn the_power_key_is_not_added_to_the_matrix() {
        let mouse = Mouse::new(NATURAL_WIDTH, NATURAL_HEIGHT);
        let mut chipset = chipset();
        mouse.press_skin(&mut chipset, MouseButton::Left, 240, 235);
        assert!(
            !chipset.keyboard.borrow().has_input,
            "the power key became visible to the key filter"
        );
    }
}
