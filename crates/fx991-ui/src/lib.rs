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

//! The calculator's face, composed into an RGBA buffer.
//!
//! This crate is the *rendering* half of the UI: it turns a [`Chipset`]'s state
//! into a picture.  It owns no window and no GPU, so it is testable without
//! either -- see `tests/golden.rs`.
//!
//! # The model
//!
//! The skin image already has the LCD's background colour baked in, and
//! `rsd_pixel` is a 1x1 sample of that flat colour.  Drawing the display means
//! *stamping* that pixel once per dot and once per indicator, letting the
//! per-stamp alpha decide how much ink lands.  "Off" is therefore not
//! transparent -- it is a light wash of ink over the LCD's base colour, which is
//! what a real LCD looks like.
//!
//! Three outcomes are possible for any LCD pixel:
//!
//! | condition | pixel | meaning |
//! |---|---|---|
//! | `mode` not in `{4,5,6}` | `(214,227,214)` | nothing drawn at all |
//! | `mode` in `{4,5,6}`, bit 0 | `(152,167,168)` | light ink |
//! | `mode` in `{4,5,6}`, bit 1 | `(25,46,75)` | full ink |
//!
//! # Two details that are easy to get wrong
//!
//! * **The ink is multiplied into the stamp**, not written directly:
//!   `stamp = skin * INK / 255`.  At contrast 20 that is `(25,46,75)`, not the
//!   `(30,52,90)` that `ink_colour` holds.
//! * **The blend rounds**, it does not truncate.  The light-ink red channel
//!   works out to 151.74: rounding gives 152 (the baseline), truncating gives
//!   151 and every golden hash fails.

pub mod layout;

use fx991_chipset::screen::{N_ROW, ROW_SIZE, ROW_SIZE_DISP};
use fx991_chipset::Chipset;

use layout::{
    Button, Sprite, BUTTONS, DOTMATRIX_ORIGIN, INDICATORS, INK, PIXEL_STAMP, SKIN_CROP, SKIN_H,
    SKIN_W,
};

/// The decoded `interface.png`, as raw RGBA.
///
/// Produced by; regenerate with that script rather than
/// editing the blob.  The full 307x615 image is kept because the indicator
/// atlas lives *below* the visible crop -- cropping
/// to the body first would make every indicator sample transparent pixels.
#[derive(Clone)]
pub struct Skin {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

/// Why a skin could not be loaded.
#[derive(Debug)]
pub enum SkinError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The file's length does not match `width * height * 4`.
    Size { expected: usize, actual: usize },
}

impl std::fmt::Display for SkinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkinError::Io(err) => write!(f, "{err}"),
            SkinError::Size { expected, actual } => write!(
                f,
                "skin is {actual} bytes, expected {expected} for the given size"
            ),
        }
    }
}

impl std::error::Error for SkinError {}

impl From<std::io::Error> for SkinError {
    fn from(err: std::io::Error) -> Self {
        SkinError::Io(err)
    }
}

impl Skin {
    /// Load a raw RGBA skin from disk.
    ///
    /// The file is a bare pixel buffer with no header, so its length must match
    /// the expected size exactly.  That is checked rather than trusted: a
    /// truncated file would otherwise render garbage instead of failing.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, SkinError> {
        let rgba = std::fs::read(path)?;
        Self::from_rgba(rgba, layout::SKIN_SOURCE_W, layout::SKIN_SOURCE_H)
    }

    /// A skin from an RGBA blob.
    pub fn from_rgba(rgba: Vec<u8>, width: u32, height: u32) -> Result<Self, SkinError> {
        let expected = width as usize * height as usize * 4;
        if rgba.len() != expected {
            return Err(SkinError::Size {
                expected,
                actual: rgba.len(),
            });
        }
        Ok(Self {
            rgba,
            width,
            height,
        })
    }

    /// The texture's width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The texture's height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The raw RGBA bytes, for uploading to a GPU.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// The RGBA at `(x, y)`.  Out-of-bounds reads give transparent black, which
    /// the stamping code treats as "nothing to draw".
    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0, 0, 0, 0];
        }
        let offset = ((y * self.width + x) * 4) as usize;
        let p = &self.rgba[offset..offset + 4];
        [p[0], p[1], p[2], p[3]]
    }
}

/// A composed frame: RGBA8, `SKIN_W * SKIN_H` pixels.
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Frame {
    /// The RGBA at `(x, y)`.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * self.width + x) * 4) as usize;
        let p = &self.rgba[offset..offset + 4];
        [p[0], p[1], p[2], p[3]]
    }

    /// FNV-1a 64 over the whole buffer, for golden comparisons.
    pub fn digest(&self) -> u64 {
        let mut digest: u64 = 0xCBF2_9CE4_8422_2325;
        for byte in &self.rgba {
            digest ^= *byte as u64;
            digest = digest.wrapping_mul(0x0000_0100_0000_01B3);
        }
        digest
    }
}

/// The stamp alphas for a given contrast, after the mode-6 override.
///
///  for the formulas, `:355-360` for the override.
pub fn alpha_levels(contrast: u8, mode: u8) -> (u8, u8) {
    let on = (20 + contrast as u16 * 16).min(255) as u8;
    let off = ((contrast as i16 - 8) * 7).max(0) as u8;
    if mode == 6 {
        (80, 20)
    } else {
        (on, off)
    }
}

/// SDL2 `BLENDMODE_BLEND`: `dst = src*a + dst*(1-a)`, **rounded**.
///
/// The `+ 127` before the divide is what makes this round rather than truncate.
/// The light-ink red channel is 151.74 and has to land on 152.
#[inline]
fn blend_channel(src: u8, alpha: u8, dst: u8) -> u8 {
    let value = src as u32 * alpha as u32 + dst as u32 * (255 - alpha as u32);
    ((value + 127) / 255) as u8
}

/// `SetTextureColorMod(ink_colour)`: a multiply, not a write.
#[inline]
fn ink_rgb(src: [u8; 4]) -> [u8; 3] {
    [
        (src[0] as u32 * INK.0 as u32 / 255) as u8,
        (src[1] as u32 * INK.1 as u32 / 255) as u8,
        (src[2] as u32 * INK.2 as u32 / 255) as u8,
    ]
}

/// Composes frames from machine state.
pub struct Renderer {
    skin: Skin,
}

impl Renderer {
    /// A renderer for a loaded skin.
    pub fn new(skin: Skin) -> Self {
        Self { skin }
    }

    pub fn skin(&self) -> &Skin {
        &self.skin
    }

    /// Compose the face for the machine's current state.
    ///
    /// Reads the screen buffer, `mode` (`0xF031`) and `contrast` (`0xF032`).
    /// `range` (`0xF030`) is deliberately *not* consulted: the hardware registers
    /// it but never uses it for rendering.
    ///
    /// Pressed-button highlights are drawn too, because the machine shows them.
    pub fn render(&self, chipset: &mut Chipset) -> Frame {
        let (mode, contrast, buffer) = {
            let screen = chipset.screen.borrow();
            (screen.mode, screen.contrast, screen.buffer().to_vec())
        };
        let pressed = chipset.keyboard.borrow().pressed_buttons();
        let mut frame = self.render_buffer(&buffer, mode, contrast);
        self.draw_press_highlights(&mut frame, &pressed);
        frame
    }

    /// Fill each pressed button's rectangle, over everything else.
    ///
    /// This is drawn *after* the chipset's own frame, so a highlight covers the
    /// skin and any dots under it:
    /// black at 50% for a held key, dark red at 50% for a latched one.
    fn draw_press_highlights(&self, frame: &mut Frame, pressed: &[(u8, bool)]) {
        for &(code, stuck) in pressed {
            let Some(button) = layout::BUTTONS.iter().find(|b| b.code == code) else {
                continue;
            };
            // SDL_BLENDMODE_BLEND with alpha 127 over the composed pixels.
            let tint = if stuck { [127u8, 0, 0] } else { [0u8, 0, 0] };
            for y in button.y as u32..button.y as u32 + button.h as u32 {
                for x in button.x as u32..button.x as u32 + button.w as u32 {
                    frame.blend(x, y, tint, 127);
                }
            }
        }
    }

    /// Compose from a raw screen buffer, without a `Chipset`.
    ///
    /// This is the seam the golden tests use, and it is the whole renderer:
    /// everything visible is a function of these three inputs.
    pub fn render_buffer(&self, buffer: &[u8], mode: u8, contrast: u8) -> Frame {
        let (crop_x, crop_y, crop_w, crop_h) = (
            SKIN_CROP.src_x as u32,
            SKIN_CROP.src_y as u32,
            SKIN_CROP.w as u32,
            SKIN_CROP.h as u32,
        );
        let mut rgba = vec![0u8; (crop_w * crop_h * 4) as usize];

        // Start from the skin crop: row by row, so the source stride can differ
        // from the destination width.
        for y in 0..crop_h {
            let src_row = (((crop_y + y) * self.skin.width + crop_x) * 4) as usize;
            let dst_row = (y * crop_w * 4) as usize;
            let len = (crop_w * 4) as usize;
            rgba[dst_row..dst_row + len].copy_from_slice(&self.skin.rgba[src_row..src_row + len]);
        }

        let mut frame = Frame {
            width: crop_w,
            height: crop_h,
            rgba,
        };

        //: anything else means "draw nothing at all".
        if mode != 4 && mode != 5 && mode != 6 {
            return frame;
        }

        let (on, off) = alpha_levels(contrast, mode);
        // mode 4 and 6 clear the dots: the buffer is not read, every dot is "off".
        let clear_dots = mode == 4 || mode == 6;
        let enable_status = mode == 5 || mode == 6;

        let stamp = self
            .skin
            .pixel(PIXEL_STAMP.src_x as u32, PIXEL_STAMP.src_y as u32);
        let stamp_ink = ink_rgb(stamp);

        if enable_status {
            for indicator in INDICATORS {
                let lit = buffer
                    .get(indicator.offset as usize)
                    .map(|byte| byte & indicator.mask != 0)
                    .unwrap_or(false);
                let alpha = if lit { on } else { off };
                self.stamp_sprite(&mut frame, &indicator.sprite, alpha);
            }
        }

        // The dot matrix: 63 rows of 24 visible bytes, MSB leftmost.
        let origin_x = DOTMATRIX_ORIGIN.0 as u32;
        let origin_y = DOTMATRIX_ORIGIN.1 as u32;
        for iy in 0..N_ROW as u32 {
            for ix in 0..ROW_SIZE_DISP as u32 {
                let offset = (iy * ROW_SIZE as u32 + 32 + ix) as usize;
                let byte = buffer.get(offset).copied().unwrap_or(0);
                for bit in 0..8u32 {
                    let mask = 0x80u8 >> bit;
                    let lit = !clear_dots && (byte & mask) != 0;
                    let alpha = if lit { on } else { off };
                    // The stamp is 1x1, so this is a single blend per dot.
                    frame.blend(origin_x + ix * 8 + bit, origin_y + iy, stamp_ink, alpha);
                }
            }
        }

        frame
    }

    /// Stamp a sprite at its destination, blending each pixel.
    ///
    /// Sprite destinations are never scaled: `dest.w == src.w` always.
    ///
    /// The sprite's own alpha is a **coverage mask**, not a transparency to be
    /// discarded: `rsd_s` and friends are antialiased glyphs whose alpha varies
    /// per pixel (89 at the edge, 255 in the stem).  SDL2's
    /// `SetTextureAlphaMod(a)` *modulates* it -- the effective alpha is
    /// `tex_alpha * a / 255` -- so throwing the texture alpha away and using `a`
    /// everywhere smears every indicator into a solid block.
    fn stamp_sprite(&self, frame: &mut Frame, sprite: &Sprite, alpha: u8) {
        for y in 0..sprite.h as u32 {
            for x in 0..sprite.w as u32 {
                let src = self
                    .skin
                    .pixel(sprite.src_x as u32 + x, sprite.src_y as u32 + y);
                if src[3] == 0 {
                    continue; // fully transparent: nothing to stamp
                }
                // SetTextureAlphaMod: modulate, do not replace.
                let effective = (src[3] as u32 * alpha as u32 / 255) as u8;
                frame.blend(
                    sprite.dest_x as u32 + x,
                    sprite.dest_y as u32 + y,
                    ink_rgb(src),
                    effective,
                );
            }
        }
    }
}

impl Frame {
    /// Blend one ink pixel at `(x, y)`.  Out-of-bounds writes are dropped.
    #[inline]
    fn blend(&mut self, x: u32, y: u32, rgb: [u8; 3], alpha: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = ((y * self.width + x) * 4) as usize;
        let dest = &mut self.rgba[offset..offset + 3];
        for (dst, src) in dest.iter_mut().zip(rgb) {
            *dst = blend_channel(src, alpha, *dst);
        }
        // The stamp is opaque, so the destination alpha becomes 255.
        self.rgba[offset + 3] = 255;
    }
}

/// The button whose rectangle contains `(x, y)`, in skin coordinates.
///
/// The first match in  order wins and the search stops, so
/// overlapping boxes resolve to the earlier entry.  The interval is **half-open**
/// on the far edge: `x < rect.x + w`.
pub fn button_at(x: i32, y: i32) -> Option<&'static Button> {
    BUTTONS.iter().find(|button| {
        x >= button.x as i32
            && x < button.x as i32 + button.w as i32
            && y >= button.y as i32
            && y < button.y as i32 + button.h as i32
    })
}

/// The natural window size, in skin pixels.
pub const NATURAL_WIDTH: u32 = SKIN_W;
pub const NATURAL_HEIGHT: u32 = SKIN_H;

// ------------------------------------------------------------------ PNG out
//
// A minimal truecolour-with-alpha encoder, for `emu render`.  Deflate blocks are
// emitted *stored* (uncompressed) because compressing needs zlib and this
// workspace deliberately has no third-party dependencies.  Files are larger but
// byte-exact, which is what the golden comparisons want anyway.

impl Frame {
    /// Encode as an 8-bit RGBA PNG.
    pub fn to_png(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity((self.height * (1 + self.width * 4)) as usize);
        for y in 0..self.height {
            raw.push(0); // filter type 0 (None) for each scanline
            let row = (y * self.width * 4) as usize;
            raw.extend_from_slice(&self.rgba[row..row + (self.width * 4) as usize]);
        }

        let mut png = Vec::new();
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&self.width.to_be_bytes());
        ihdr.extend_from_slice(&self.height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
        push_chunk(&mut png, b"IHDR", &ihdr);
        push_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
        push_chunk(&mut png, b"IEND", &[]);
        png
    }
}

fn push_chunk(out: &mut Vec<u8>, tag: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    let mut body = Vec::with_capacity(4 + payload.len());
    body.extend_from_slice(tag);
    body.extend_from_slice(payload);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// A zlib stream made entirely of stored (type 0) deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // CMF/FLG: deflate, 32K window, no dict
    let mut offset = 0;
    while offset < data.len() {
        let take = (data.len() - offset).min(0xFFFF);
        let final_block = offset + take >= data.len();
        out.push(if final_block { 1 } else { 0 });
        out.extend_from_slice(&(take as u16).to_le_bytes());
        out.extend_from_slice(&(!(take as u16)).to_le_bytes());
        out.extend_from_slice(&data[offset..offset + take]);
        offset += take;
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + *byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_formulas_match_the_reference() {
        assert_eq!(alpha_levels(20, 5), (255, 84));
        assert_eq!(alpha_levels(0, 5), (20, 0));
        assert_eq!(alpha_levels(8, 5), (148, 0));
        // mode 6 overrides both.
        assert_eq!(alpha_levels(20, 6), (80, 20));
    }

    #[test]
    fn the_blend_rounds_rather_than_truncates() {
        // The light-ink red channel: (25*84 + 214*171) / 255 = 151.74 -> 152.
        // Truncating would give 151 and break every golden hash.
        assert_eq!(blend_channel(25, 84, 214), 152);
        // The other two channels round the same either way.
        assert_eq!(blend_channel(46, 84, 227), 167);
        assert_eq!(blend_channel(75, 84, 214), 168);
        // Full alpha and zero alpha are exact.
        assert_eq!(blend_channel(25, 255, 214), 25);
        assert_eq!(blend_channel(25, 0, 214), 214);
    }

    #[test]
    fn ink_is_multiplied_not_written() {
        // (214,227,214) * (30,52,90) / 255 = (25,46,75)
        assert_eq!(ink_rgb([214, 227, 214, 255]), [25, 46, 75]);
    }

    #[test]
    fn hit_testing_is_half_open_and_ordered() {
        // Buttons are 41 wide at x = 29, 76, 123,.. so there is a 6px gutter
        // between columns; a click there hits nothing.
        assert_eq!(
            button_at(29, 404).map(|b| b.code),
            Some(0x02),
            "'7' left edge"
        );
        assert_eq!(
            button_at(69, 404).map(|b| b.code),
            Some(0x02),
            "'7' last column"
        );
        assert_eq!(button_at(70, 404).map(|b| b.code), None, "gutter");
        assert_eq!(
            button_at(76, 404).map(|b| b.code),
            Some(0x12),
            "'8' left edge"
        );

        // Rows are 40 apart and 29 tall, so there is an 11px gutter vertically.
        assert_eq!(
            button_at(29, 432).map(|b| b.code),
            Some(0x02),
            "'7' last row"
        );
        assert_eq!(button_at(29, 433).map(|b| b.code), None, "row gutter");
        assert_eq!(
            button_at(29, 444).map(|b| b.code),
            Some(0x01),
            "'4' first row"
        );

        // Outside everything.
        assert!(button_at(0, 0).is_none());
        assert!(button_at(283, 596).is_none());
    }

    #[test]
    fn every_button_code_is_reachable_by_hit_testing() {
        // A button whose rectangle can never be hit would be dead on screen.
        for button in BUTTONS {
            let cx = button.x as i32 + button.w as i32 / 2;
            let cy = button.y as i32 + button.h as i32 / 2;
            let hit = button_at(cx, cy).expect("centre of a button should hit");
            // Overlapping boxes resolve to the earlier entry, so the centre may
            // legitimately belong to an earlier button; it must never miss.
            assert!(
                hit.code == button.code
                    || BUTTONS.iter().position(|b| b.code == hit.code)
                        < BUTTONS.iter().position(|b| b.code == button.code),
                "button {:?} at ({cx},{cy}) is shadowed by a later one",
                button.lua_name
            );
        }
    }
}
