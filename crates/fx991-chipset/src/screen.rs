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

//! Screen buffer and controls.
//!
//! Geometry for `HW_CLASSWIZ`:
//!
//! `text
//! N_ROW = 63          rows of dot matrix, excluding the status row
//! ROW_SIZE = 32       bytes per row in the buffer
//! OFFSET = 32         bytes skipped at the start of each row
//! ROW_SIZE_DISP = 24  bytes actually displayed
//! buffer base 0xF800, total (63 + 1) * 32 = 0x800 bytes
//! `
//!
//! Row 0 is the status indicator row.  For the dot matrix, row `iy` and display
//! byte `ix` live at `iy * ROW_SIZE + OFFSET + ix`, and each set bit is one lit
//! pixel with the MSB leftmost.
//!
//! Bytes at `offset % ROW_SIZE >= ROW_SIZE_DISP` are padding: writes are
//! discarded and reads return 0.  Dropping that rule makes
//! the buffer look 33% larger than it is and quietly changes what the ROM sees.

/// Rows of dot matrix, excluding the status row.
pub const N_ROW: usize = 63;
/// Bytes per row in the buffer.
pub const ROW_SIZE: usize = 32;
/// Bytes skipped at the start of each row.
pub const OFFSET: usize = 32;
/// Bytes of each row that are actually displayed.
pub const ROW_SIZE_DISP: usize = 24;
/// Bytes in the whole buffer.
pub const BUFFER_SIZE: usize = (N_ROW + 1) * ROW_SIZE;

/// `0xF800`
pub const BUFFER_BASE: u32 = 0x0_F800;
/// `0xF030`, masked with 0x07
pub const CONTROL_RANGE: u32 = 0x0_F030;
/// `0xF031`, masked with 0x07
pub const CONTROL_MODE: u32 = 0x0_F031;
/// `0xF032`, masked with 0x3F
pub const CONTROL_CONTRAST: u32 = 0x0_F032;

/// The display buffer and its control registers.
pub struct Screen {
    buffer: Vec<u8>,
    /// `0xF030`
    pub range: u8,
    /// `0xF031`
    pub mode: u8,
    /// `0xF032`
    pub contrast: u8,
    /// Set when something visible changed, so a renderer knows to redraw.
    pub dirty: bool,
}

/// The display's state, for snapshot and restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenState {
    /// The raw dot-matrix buffer, including padding bytes.
    pub buffer: Vec<u8>,
    /// `0xF030`
    pub range: u8,
    /// `0xF031`
    pub mode: u8,
    /// `0xF032`
    pub contrast: u8,
    /// Set when something visible changed.
    pub dirty: bool,
}

impl Default for Screen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen {
    /// Everything needed to put this display back where it was.
    ///
    /// A snapshot has to survive the round trip exactly, because the renderer's
    /// output is a function of the buffer, `mode` and `contrast` alone: a partial
    /// restore would draw a plausible but wrong screen.
    pub fn snapshot(&self) -> ScreenState {
        ScreenState {
            buffer: self.buffer.clone(),
            range: self.range,
            mode: self.mode,
            contrast: self.contrast,
            dirty: self.dirty,
        }
    }

    /// Restore a [`ScreenState`].
    pub fn restore(&mut self, state: &ScreenState) {
        let length = state.buffer.len().min(self.buffer.len());
        self.buffer[..length].copy_from_slice(&state.buffer[..length]);
        self.range = state.range;
        self.mode = state.mode;
        self.contrast = state.contrast;
        self.dirty = state.dirty;
    }

    /// A blank screen.
    pub fn new() -> Self {
        Self {
            buffer: vec![0; BUFFER_SIZE],
            range: 0,
            mode: 0,
            contrast: 0,
            dirty: true,
        }
    }

    /// `Screen::Reset`.
    pub fn reset(&mut self) {
        self.buffer.iter_mut().for_each(|byte| *byte = 0);
        self.range = 0;
        self.mode = 0;
        self.contrast = 0;
        self.dirty = true;
    }

    /// The raw buffer, including padding bytes.
    pub fn buffer(&self) -> &[u8] {
        &self.buffer
    }

    /// One visible byte, or `None` for padding and out-of-range offsets.
    pub fn visible_byte(&self, offset: usize) -> Option<u8> {
        if offset >= BUFFER_SIZE || offset % ROW_SIZE >= ROW_SIZE_DISP {
            return None;
        }
        Some(self.buffer[offset])
    }

    /// The dot-matrix rows as strings of `#` and `.`, for tests and terminals.
    pub fn render_ascii(&self) -> Vec<String> {
        (0..N_ROW)
            .map(|iy| {
                let base = iy * ROW_SIZE + OFFSET;
                let mut line = String::with_capacity(ROW_SIZE_DISP * 8);
                for ix in 0..ROW_SIZE_DISP {
                    let byte = self.buffer[base + ix];
                    for bit in (0..8).rev() {
                        line.push(if byte & (1 << bit) != 0 { '#' } else { '.' });
                    }
                }
                line
            })
            .collect()
    }

    /// How many pixels are lit, for a cheap "is anything on screen" check.
    pub fn lit_pixels(&self) -> usize {
        (0..N_ROW)
            .map(|iy| {
                let base = iy * ROW_SIZE + OFFSET;
                self.buffer[base..base + ROW_SIZE_DISP]
                    .iter()
                    .map(|byte| byte.count_ones() as usize)
                    .sum::<usize>()
            })
            .sum()
    }

    /// Render to a PNG byte stream.
    ///
    /// One pixel per bit, so this shows exactly what the guest wrote.  It is a
    /// debugging aid, not a picture of the calculator's font.
    ///
    /// The container comes from [`crate::png`], which is shared with the composed
    /// face: the framing is the same either way, and only the pixels differ.
    pub fn to_png(&self, scale: usize) -> Vec<u8> {
        let scale = scale.max(1);
        let width = ROW_SIZE_DISP * 8;
        let height = N_ROW;
        let on = [30u8, 52, 90];
        let off = [210u8, 220, 200];

        // One row at 1:1, scaled horizontally, then repeated `scale` times
        // vertically.  Both directions have to be scaled: repeating whole rows
        // alone would stretch the picture only downwards.
        let mut raw = Vec::with_capacity(width * scale * height * scale * 3);
        for iy in 0..height {
            let base = iy * ROW_SIZE + OFFSET;
            let mut row = Vec::with_capacity(width * scale * 3);
            for ix in 0..ROW_SIZE_DISP {
                let byte = self.buffer[base + ix];
                for bit in (0..8).rev() {
                    let colour = if byte & (1 << bit) != 0 { on } else { off };
                    for _ in 0..scale {
                        row.extend_from_slice(&colour);
                    }
                }
            }
            for _ in 0..scale {
                raw.extend_from_slice(&row);
            }
        }

        crate::png::encode(
            &raw,
            (width * scale) as u32,
            (height * scale) as u32,
            crate::png::ColourType::Truecolour,
        )
    }

    /// Read an SFR this peripheral owns.
    pub fn read(&mut self, address: u32) -> Option<u8> {
        match address {
            CONTROL_RANGE => Some(self.range & 0x07),
            CONTROL_MODE => Some(self.mode & 0x07),
            CONTROL_CONTRAST => Some(self.contrast & 0x3F),
            _ if (BUFFER_BASE..BUFFER_BASE + BUFFER_SIZE as u32).contains(&address) => {
                let offset = (address - BUFFER_BASE) as usize;
                if offset % ROW_SIZE >= ROW_SIZE_DISP {
                    Some(0)
                } else {
                    Some(self.buffer[offset])
                }
            }
            _ => None,
        }
    }

    /// Write an SFR this peripheral owns.
    pub fn write(&mut self, address: u32, value: u8) -> bool {
        match address {
            CONTROL_RANGE => {
                let new = value & 0x07;
                self.dirty |= self.range != new;
                self.range = new;
                true
            }
            CONTROL_MODE => {
                let new = value & 0x07;
                self.dirty |= self.mode != new;
                self.mode = new;
                true
            }
            CONTROL_CONTRAST => {
                let new = value & 0x3F;
                self.dirty |= self.contrast != new;
                self.contrast = new;
                true
            }
            _ if (BUFFER_BASE..BUFFER_BASE + BUFFER_SIZE as u32).contains(&address) => {
                let offset = (address - BUFFER_BASE) as usize;
                if offset % ROW_SIZE >= ROW_SIZE_DISP {
                    return true; // padding: discarded, but still owned
                }
                self.dirty |= self.buffer[offset] != value;
                self.buffer[offset] = value;
                true
            }
            _ => false,
        }
    }

    /// `Screen::Tick` -- there is nothing to do per instruction.
    pub fn tick(&mut self) {}

    /// Take the dirty flag, clearing it.
    ///
    /// A renderer calls this once per frame: `true` means something visible
    /// changed since the last call.  The flag is only ever *set* by writes, so
    /// a renderer that never calls this redraws unnecessarily but cannot miss
    /// a change.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buffer_is_the_size_the_geometry_says() {
        assert_eq!(BUFFER_SIZE, (63 + 1) * 32);
        assert_eq!(Screen::new().buffer().len(), BUFFER_SIZE);
    }

    #[test]
    fn padding_bytes_are_zero_on_read_and_discarded_on_write() {
        let mut screen = Screen::new();
        assert!(screen.write(BUFFER_BASE + 30, 0xFF));
        assert_eq!(screen.read(BUFFER_BASE + 30), Some(0));
        assert!(screen.write(BUFFER_BASE + 23, 0xFF));
        assert_eq!(screen.read(BUFFER_BASE + 23), Some(0xFF));
    }

    #[test]
    fn the_control_registers_are_masked() {
        let mut screen = Screen::new();
        screen.write(CONTROL_RANGE, 0xFF);
        screen.write(CONTROL_MODE, 0xFF);
        screen.write(CONTROL_CONTRAST, 0xFF);
        assert_eq!(screen.read(CONTROL_RANGE), Some(0x07));
        assert_eq!(screen.read(CONTROL_MODE), Some(0x07));
        assert_eq!(screen.read(CONTROL_CONTRAST), Some(0x3F));
    }

    #[test]
    fn a_lit_bit_renders_as_a_pixel() {
        let mut screen = Screen::new();
        // Dot-matrix row 0 starts at `OFFSET` within the buffer; the first
        // `OFFSET` bytes are the status indicator row.
        screen.write(BUFFER_BASE + OFFSET as u32, 0x80);
        let rows = screen.render_ascii();
        assert!(rows[0].starts_with('#'), "bit 7 of the first display byte");
        assert_eq!(screen.lit_pixels(), 1);
    }

    #[test]
    fn the_ascii_render_is_the_full_dot_matrix() {
        let screen = Screen::new();
        let rows = screen.render_ascii();
        assert_eq!(rows.len(), N_ROW);
        assert_eq!(rows[0].len(), ROW_SIZE_DISP * 8);
    }

    #[test]
    fn png_output_has_the_expected_envelope() {
        let mut screen = Screen::new();
        screen.write(BUFFER_BASE + OFFSET as u32, 0xFF);
        let png = screen.to_png(2);
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(png.ends_with(b"IEND\xaeB`\x82"));
        // IHDR width/height live at a fixed offset.
        let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        let height = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
        assert_eq!(width as usize, ROW_SIZE_DISP * 8 * 2);
        assert_eq!(height as usize, N_ROW * 2);
    }
}
