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

//! Filling and blending into an RGBA frame.
//!
//! A thin layer over the frame's bytes, so the panels never index the buffer
//! themselves: every write goes through a bounds check here, which is what lets a
//! panel draw a row that runs off its own rectangle without special-casing it.
//!
//! Blending is source-over with 8-bit coverage, which is what the glyph
//! rasteriser produces: `dst = (src * a + dst * (255 - a)) / 255`.  The same
//! rounding as the face renderer's blend, so a text pixel and a calculator pixel
//! at the same alpha come out identical.

use crate::layout::Rect;
use crate::theme::Rgba;

/// A mutable view onto a frame's pixels.
pub struct Canvas<'a> {
    rgba: &'a mut [u8],
    width: u32,
    height: u32,
}

impl<'a> Canvas<'a> {
    /// Wrap a frame's pixel buffer.
    ///
    /// The buffer must be `width * height * 4` bytes; the frame it comes from
    /// guarantees that.
    pub fn new(rgba: &'a mut [u8], width: u32, height: u32) -> Self {
        Self {
            rgba,
            width,
            height,
        }
    }

    /// The frame's width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The frame's height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The whole frame as a rectangle.
    pub fn bounds(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.width,
            h: self.height,
        }
    }

    /// Blend one pixel, replacing its alpha with the source's.
    ///
    /// Out-of-bounds writes are dropped rather than panicking: a panel's content
    /// is clipped to its own rectangle, but a glyph can still overhang by a pixel
    /// and that must not be a crash.
    pub fn blend(&mut self, x: u32, y: u32, colour: Rgba, coverage: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = ((y * self.width + x) * 4) as usize;
        let Some(pixel) = self.rgba.get_mut(offset..offset + 4) else {
            return;
        };
        let alpha = coverage as u32;
        if alpha == 0 {
            return;
        }
        if alpha == 255 {
            pixel.copy_from_slice(&colour);
            return;
        }
        for channel in 0..3 {
            let src = colour[channel] as u32;
            let dst = pixel[channel] as u32;
            pixel[channel] = ((src * alpha + dst * (255 - alpha) + 127) / 255) as u8;
        }
        // The source is opaque ink, so the destination becomes opaque too.
        pixel[3] = 255;
    }

    /// Fill a rectangle, clipped to the frame.
    ///
    /// The colour is written through, alpha and all.
    pub fn fill(&mut self, rect: Rect, colour: Rgba) {
        let x0 = rect.x;
        let y0 = rect.y;
        let x1 = rect.x.saturating_add(rect.w).min(self.width);
        let y1 = rect.y.saturating_add(rect.h).min(self.height);
        for y in y0..y1 {
            for x in x0..x1 {
                self.blend(x, y, colour, 255);
            }
        }
    }

    /// Draw a one-pixel outline just inside a rectangle.
    ///
    /// Inside rather than centred so the border never escapes the panel's own
    /// rectangle and cannot overwrite a neighbour.
    pub fn stroke(&mut self, rect: Rect, colour: Rgba) {
        if rect.w == 0 || rect.h == 0 {
            return;
        }
        let x1 = rect.x + rect.w - 1;
        let y1 = rect.y + rect.h - 1;
        for x in rect.x..=x1 {
            self.blend(x, rect.y, colour, 255);
            self.blend(x, y1, colour, 255);
        }
        for y in rect.y..=y1 {
            self.blend(rect.x, y, colour, 255);
            self.blend(x1, y, colour, 255);
        }
    }

    /// A horizontal line one pixel tall, clipped to the frame.
    pub fn hline(&mut self, x: u32, y: u32, width: u32, colour: Rgba) {
        if y >= self.height {
            return;
        }
        let x1 = x.saturating_add(width).min(self.width);
        for px in x..x1 {
            self.blend(px, y, colour, 255);
        }
    }

    /// The RGBA at a point, for tests.
    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgba> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = ((y * self.width + x) * 4) as usize;
        let p = self.rgba.get(offset..offset + 4)?;
        Some([p[0], p[1], p[2], p[3]])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> (Vec<u8>, u32, u32) {
        (vec![0u8; 8 * 8 * 4], 8, 8)
    }

    #[test]
    fn a_full_coverage_write_replaces_the_pixel() {
        let (mut bytes, w, h) = canvas();
        let mut canvas = Canvas::new(&mut bytes, w, h);
        canvas.blend(3, 4, [10, 20, 30, 255], 255);
        assert_eq!(canvas.pixel(3, 4), Some([10, 20, 30, 255]));
    }

    #[test]
    fn a_partial_coverage_write_blends_and_rounds() {
        let (mut bytes, w, h) = canvas();
        let mut canvas = Canvas::new(&mut bytes, w, h);
        canvas.fill(canvas.bounds(), [255, 255, 255, 255]);

        // Black at 128/255 over white: (0*128 + 255*127 + 127) / 255 = 127.
        // The `+ 127` is what rounds; without it the same sum would truncate to
        // 126 and a text edge would sit half a shade low.
        canvas.blend(0, 0, [0, 0, 0, 255], 128);
        assert_eq!(canvas.pixel(0, 0), Some([127, 127, 127, 255]));

        // One step up in coverage rounds the other way, so the rounding is real
        // and not a constant offset.
        canvas.fill(canvas.bounds(), [255, 255, 255, 255]);
        canvas.blend(0, 0, [0, 0, 0, 255], 129);
        assert_eq!(canvas.pixel(0, 0), Some([126, 126, 126, 255]));
    }

    #[test]
    fn a_zero_coverage_write_changes_nothing() {
        let (mut bytes, w, h) = canvas();
        let mut canvas = Canvas::new(&mut bytes, w, h);
        canvas.fill(canvas.bounds(), [1, 2, 3, 255]);
        canvas.blend(0, 0, [255, 255, 255, 255], 0);
        assert_eq!(canvas.pixel(0, 0), Some([1, 2, 3, 255]));
    }

    #[test]
    fn drawing_outside_the_frame_is_dropped_not_a_panic() {
        let (mut bytes, w, h) = canvas();
        let mut canvas = Canvas::new(&mut bytes, w, h);
        canvas.blend(8, 0, [255, 0, 0, 255], 255);
        canvas.blend(0, 8, [255, 0, 0, 255], 255);
        canvas.blend(u32::MAX, u32::MAX, [255, 0, 0, 255], 255);
        assert_eq!(canvas.pixel(0, 0), Some([0, 0, 0, 0]), "nothing was drawn");
    }

    #[test]
    fn a_fill_is_clipped_to_the_frame() {
        let (mut bytes, w, h) = canvas();
        let mut canvas = Canvas::new(&mut bytes, w, h);
        // A rectangle that overhangs every edge.
        canvas.fill(
            Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 100,
            },
            [7, 7, 7, 255],
        );
        assert_eq!(canvas.pixel(7, 7), Some([7, 7, 7, 255]));
        assert_eq!(canvas.pixel(0, 0), Some([7, 7, 7, 255]));
    }

    #[test]
    fn a_stroke_stays_inside_its_rectangle() {
        let (mut bytes, w, h) = canvas();
        let mut canvas = Canvas::new(&mut bytes, w, h);
        let rect = Rect {
            x: 1,
            y: 1,
            w: 4,
            h: 4,
        };
        canvas.stroke(rect, [9, 9, 9, 255]);
        // The border is drawn on the rectangle's own edge.
        assert_eq!(canvas.pixel(1, 1), Some([9, 9, 9, 255]));
        assert_eq!(canvas.pixel(4, 4), Some([9, 9, 9, 255]));
        // And nothing outside it.
        assert_eq!(canvas.pixel(0, 0), Some([0, 0, 0, 0]));
        assert_eq!(canvas.pixel(5, 5), Some([0, 0, 0, 0]));
        // The interior is untouched.
        assert_eq!(canvas.pixel(2, 2), Some([0, 0, 0, 0]));
    }

    #[test]
    fn an_empty_rectangle_draws_nothing() {
        let (mut bytes, w, h) = canvas();
        let mut canvas = Canvas::new(&mut bytes, w, h);
        canvas.stroke(
            Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            [9, 9, 9, 255],
        );
        canvas.fill(
            Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            [9, 9, 9, 255],
        );
        assert_eq!(canvas.pixel(0, 0), Some([0, 0, 0, 0]));
    }
}
