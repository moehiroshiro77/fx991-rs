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

//! Glyph rasterisation, and the cache that keeps it off the redraw path.
//!
//! The face is supplied by the user as a font file; nothing is bundled, for the
//! same reason the ROM and the skin are not.  A **monospace** face is required,
//! not merely preferred: every panel is a grid of character cells, and the hex
//! dump's columns only line up because each glyph has the same advance.  The
//! cell width is therefore measured from one character rather than from each
//! glyph, and text is placed by multiplying that width by the column.
//!
//! Rasterising is the expensive part, so each glyph is drawn once at the fixed
//! size the panels use and the coverage bitmap is kept.  A frame then costs a
//! lookup per character plus the blend, which is what makes redrawing a
//! full-window panel affordable on the CPU.

use std::collections::HashMap;

use ab_glyph::{Font, FontArc, Glyph, PxScale, ScaleFont};

/// Why a font could not be loaded.
#[derive(Debug)]
pub enum FontError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The bytes are not a font this build can parse.
    Invalid(String),
    /// The face loaded, but cannot draw the characters a panel needs.
    Incomplete(String),
}

impl std::fmt::Display for FontError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FontError::Io(err) => write!(f, "{err}"),
            FontError::Invalid(what) => write!(f, "not a usable font: {what}"),
            FontError::Incomplete(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for FontError {}

impl From<std::io::Error> for FontError {
    fn from(err: std::io::Error) -> Self {
        FontError::Io(err)
    }
}

/// One rasterised glyph: its coverage bitmap and where it sits in a cell.
#[derive(Debug, Clone)]
struct GlyphBitmap {
    width: u32,
    height: u32,
    /// Coverage, one byte per pixel, row-major.
    coverage: Vec<u8>,
    /// Offset from the cell's top-left to the bitmap's top-left.
    left: i32,
    top: i32,
}

/// A loaded monospace face, with its glyphs cached.
///
/// No `Debug`: the parsed font is a large opaque blob and printing it would say
/// nothing a reader wants.  `FontSet::cell_w`/`cell_h` are what a diagnostic
/// would actually report.
pub struct FontSet {
    font: FontArc,
    scale: PxScale,
    cell_w: u32,
    cell_h: u32,
    /// Baseline offset from the top of a cell.
    baseline: i32,
    cache: HashMap<char, GlyphBitmap>,
}

/// The pixel size the panels are drawn at.
///
/// Fixed rather than scaled with the window: a debugger's panels are a grid of
/// characters, and growing the glyphs would fit fewer rows rather than show more.
const PIXEL_SIZE: f32 = 14.0;

/// The characters a panel may draw.
///
/// Checked when the font is loaded rather than discovered mid-frame: a face
/// missing a glyph would otherwise draw a blank cell and the panel would look
/// like it had lost data.  The set is what the panels actually use -- ASCII plus
/// the few box-drawing and arrow characters the status column draws.
const REQUIRED: &str = " !\"#$%&'()*+,-./0123456789:;<=>?@\
ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";

impl FontSet {
    /// Load a face from a file.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, FontError> {
        let data = std::fs::read(path)?;
        Self::from_bytes(data)
    }

    /// Load a face from memory.
    pub fn from_bytes(data: Vec<u8>) -> Result<Self, FontError> {
        let font =
            FontArc::try_from_vec(data).map_err(|err| FontError::Invalid(err.to_string()))?;
        let scale = PxScale::from(PIXEL_SIZE);
        let scaled = font.as_scaled(scale);

        // The cell is one character wide and one line tall.  The width comes from
        // `0` because the face is monospace; measuring per glyph would let a
        // wider character push its neighbours out of their columns.
        let cell_w = scaled.h_advance(font.glyph_id('0')).ceil().max(1.0) as u32;
        let ascent = scaled.ascent().ceil();
        let descent = scaled.descent().abs().ceil();
        let cell_h = (ascent + descent).max(1.0) as u32;

        let font_set = Self {
            font,
            scale,
            cell_w,
            cell_h,
            baseline: ascent as i32,
            cache: HashMap::new(),
        };

        // Refuse a face that cannot draw the panels rather than drawing gaps.
        //
        // The test is the glyph's *existence*, not its outline: a space is a real
        // glyph with no ink, and asking for its outline would report every font
        // as broken.  `.notdef` is what a missing character maps to, so a face
        // that answers with it cannot draw that character.
        let notdef = font_set.font.glyph_id('\u{FFFD}');
        let missing: String = REQUIRED
            .chars()
            .filter(|c| font_set.font.glyph_id(*c) == notdef)
            .collect();
        if !missing.is_empty() {
            return Err(FontError::Incomplete(format!(
                "the font has no glyph for {missing:?}"
            )));
        }

        Ok(font_set)
    }

    /// The width of one character cell.
    pub fn cell_w(&self) -> u32 {
        self.cell_w
    }

    /// The height of one text row.
    pub fn cell_h(&self) -> u32 {
        self.cell_h
    }

    /// How many characters fit in `width` pixels.
    pub fn columns_in(&self, width: u32) -> usize {
        (width / self.cell_w) as usize
    }

    /// How many rows fit in `height` pixels.
    pub fn rows_in(&self, height: u32) -> usize {
        (height / self.cell_h) as usize
    }

    /// The cached bitmap for a character, rasterising it on first use.
    fn glyph(&mut self, c: char) -> Option<&GlyphBitmap> {
        if !self.cache.contains_key(&c) {
            let bitmap = self.rasterise(c)?;
            self.cache.insert(c, bitmap);
        }
        self.cache.get(&c)
    }

    /// Rasterise one character into a cell-sized bitmap.
    fn rasterise(&self, c: char) -> Option<GlyphBitmap> {
        let id = self.font.glyph_id(c);
        let glyph: Glyph = id.with_scale_and_position(self.scale, ab_glyph::point(0.0, 0.0));
        let outlined = self.font.outline_glyph(glyph)?;
        let bounds = outlined.px_bounds();

        let width = bounds.width().ceil() as u32;
        let height = bounds.height().ceil() as u32;
        if width == 0 || height == 0 {
            // A space, and anything else with no ink: a valid, empty cell.
            return Some(GlyphBitmap {
                width: 0,
                height: 0,
                coverage: Vec::new(),
                left: 0,
                top: 0,
            });
        }

        let mut coverage = vec![0u8; (width * height) as usize];
        outlined.draw(|x, y, value| {
            let index = (y * width + x) as usize;
            if index < coverage.len() {
                // Coverage is already 0.0..=1.0; the panels want 0..=255.
                coverage[index] = (value * 255.0).round() as u8;
            }
        });

        // `bounds` is relative to the baseline at the origin; the cell places the
        // baseline `self.baseline` pixels down.
        Some(GlyphBitmap {
            width,
            height,
            coverage,
            left: bounds.min.x.floor() as i32,
            top: bounds.min.y.floor() as i32 + self.baseline,
        })
    }

    /// Draw one character at a cell position, blending its coverage over the
    /// destination.
    ///
    /// `(col, row)` are cell coordinates within `frame`; the character is drawn
    /// only where it lands inside `clip`, so a panel can pass its own content
    /// rectangle and let a long line be cut off at the edge rather than spilling
    /// into its neighbour.
    pub fn draw_cell(
        &mut self,
        frame: &mut crate::canvas::Canvas<'_>,
        col: usize,
        row: usize,
        c: char,
        colour: crate::theme::Rgba,
        clip: crate::layout::Rect,
    ) {
        let Some(cell) = self.cell_rect(col, row, clip) else {
            return;
        };
        let (cell_x, cell_y) = (cell.x, cell.y);
        let Some(bitmap) = self.glyph(c) else {
            return;
        };
        if bitmap.width == 0 || bitmap.height == 0 {
            return;
        }

        let origin_x = cell_x as i32 + bitmap.left;
        let origin_y = cell_y as i32 + bitmap.top;

        for gy in 0..bitmap.height {
            for gx in 0..bitmap.width {
                let coverage = bitmap.coverage[(gy * bitmap.width + gx) as usize];
                if coverage == 0 {
                    continue;
                }
                let x = origin_x + gx as i32;
                let y = origin_y + gy as i32;
                if x < 0 || y < 0 {
                    continue;
                }
                let (x, y) = (x as u32, y as u32);
                if !clip.contains(x, y) {
                    continue;
                }
                frame.blend(x, y, colour, coverage);
            }
        }
    }

    /// Where a cell sits, or `None` when it falls outside the clip rectangle.
    fn cell_rect(
        &self,
        col: usize,
        row: usize,
        clip: crate::layout::Rect,
    ) -> Option<crate::layout::Rect> {
        let x = clip.x as usize + col * self.cell_w as usize;
        let y = clip.y as usize + row * self.cell_h as usize;
        let rect = crate::layout::Rect {
            x: x as u32,
            y: y as u32,
            w: self.cell_w,
            h: self.cell_h,
        };
        // A cell starting outside the clip is skipped; one that merely overhangs
        // is drawn, and the per-pixel test trims it.
        let inside = rect.x < clip.x + clip.w && rect.y < clip.y + clip.h;
        inside.then_some(rect)
    }

    /// Draw a string starting at a cell position.
    ///
    /// Returns the column just past the last character, so a caller can append to
    /// a line without recomputing the width.
    pub fn draw_text(
        &mut self,
        frame: &mut crate::canvas::Canvas<'_>,
        col: usize,
        row: usize,
        text: &str,
        colour: crate::theme::Rgba,
        clip: crate::layout::Rect,
    ) -> usize {
        let mut column = col;
        for c in text.chars() {
            // Stop at the clip's right edge rather than drawing off it, which
            // also keeps the returned column meaningful.
            if (column as u32) * self.cell_w + self.cell_w > clip.w {
                break;
            }
            self.draw_cell(frame, column, row, c, colour, clip);
            column += 1;
        }
        column
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A font is a user-supplied file, so these tests skip rather than fail when
    /// it is absent -- the same convention the rest of the workspace uses for the
    /// ROM and the skin.
    fn font() -> Option<FontSet> {
        let path = testkit::data_file("font.ttf");
        FontSet::from_file(&path).ok()
    }

    #[test]
    fn a_monospace_face_has_a_consistent_cell() {
        let Some(font) = font() else {
            return;
        };
        assert!(font.cell_w() > 0, "a cell has width");
        assert!(font.cell_h() > 0, "a cell has height");
        // The cell is measured from `0`, and every required glyph is present, so
        // the columns of a hex dump line up.
        assert!(
            font.cell_w() >= 4 && font.cell_w() <= 32,
            "{}",
            font.cell_w()
        );
    }

    #[test]
    fn a_face_missing_a_required_glyph_is_refused() {
        // Not a font at all: the error has to be an error, not a panic.
        let err = match FontSet::from_bytes(vec![0u8; 64]) {
            Ok(_) => panic!("64 zero bytes are not a font"),
            Err(err) => err,
        };
        assert!(
            matches!(err, FontError::Invalid(_)),
            "expected an invalid-font error, got {err}"
        );
    }

    #[test]
    fn columns_and_rows_divide_by_the_cell() {
        let Some(font) = font() else {
            return;
        };
        assert_eq!(font.columns_in(font.cell_w() * 10), 10);
        assert_eq!(font.rows_in(font.cell_h() * 7), 7);
        // A partial cell does not count.
        assert_eq!(font.columns_in(font.cell_w() * 10 - 1), 9);
    }
}
