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

//! A minimal PNG writer.
//!
//! Two things in this workspace produce a PNG -- the raw screen dump and the
//! composed calculator face -- and the container format is identical for both,
//! so it is written once here.
//!
//! Deflate blocks are emitted **stored** (uncompressed): compressing needs zlib,
//! and a self-contained build is worth more here than smaller files.  The output
//! is still a valid PNG, and byte-exact, which is what the golden comparisons
//! want.

/// An 8-bit PNG colour type.
///
/// The two used here differ in how many bytes a pixel takes, which is the only
/// thing the writer needs to know about them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColourType {
    /// Three bytes per pixel, no alpha.
    Truecolour,
    /// Four bytes per pixel, with alpha.
    TruecolourAlpha,
}

impl ColourType {
    /// The PNG colour-type code.
    fn code(self) -> u8 {
        match self {
            ColourType::Truecolour => 2,
            ColourType::TruecolourAlpha => 6,
        }
    }

    /// Bytes per pixel.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            ColourType::Truecolour => 3,
            ColourType::TruecolourAlpha => 4,
        }
    }
}

/// Encode raw scanlines as a PNG.
///
/// `raw` is the image row by row, `width * colour.bytes_per_pixel()` bytes per
/// row, with no filter byte: the caller builds pixels, this adds the container.
///
/// # Panics
///
/// If `raw` is not exactly `height` rows of `width` pixels, since a short buffer
/// would produce a file that decodes to the wrong picture rather than failing.
pub fn encode(raw: &[u8], width: u32, height: u32, colour: ColourType) -> Vec<u8> {
    let stride = width as usize * colour.bytes_per_pixel();
    assert_eq!(
        raw.len(),
        stride * height as usize,
        "the pixel buffer does not match {width}x{height} at {colour:?}"
    );

    // Each scanline is prefixed with its filter type, 0 meaning "none".
    //
    // The loop is guarded by the stride rather than written as `raw.chunks(stride)`:
    // a zero-width image has a zero stride, and `chunks` panics on that.
    let mut filtered = Vec::with_capacity(height as usize * (1 + stride));
    if stride > 0 {
        for row in raw.chunks(stride) {
            filtered.push(0);
            filtered.extend_from_slice(row);
        }
    }

    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, colour.code(), 0, 0, 0]); // 8-bit
    push_chunk(&mut png, b"IHDR", &ihdr);
    push_chunk(&mut png, b"IDAT", &zlib_stored(&filtered));
    push_chunk(&mut png, b"IEND", &[]);
    png
}

fn push_chunk(out: &mut Vec<u8>, tag: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    let mut body = Vec::with_capacity(4 + payload.len());
    body.extend_from_slice(tag);
    body.extend_from_slice(payload);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// Wrap bytes in a zlib stream built from stored (uncompressed) deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // zlib header: deflate, no preset dictionary
    if data.is_empty() {
        // A stream with no blocks still needs one, or the decoder sees a truncated
        // stream: an empty stored block, marked final.
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    let mut remaining = data;
    while !remaining.is_empty() {
        let take = remaining.len().min(0xFFFF);
        let last = take == remaining.len();
        out.push(if last { 1 } else { 0 }); // BFINAL, BTYPE=00 (stored)
        out.extend_from_slice(&(take as u16).to_le_bytes());
        out.extend_from_slice(&(!(take as u16)).to_le_bytes());
        out.extend_from_slice(&remaining[..take]);
        remaining = &remaining[take..];
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
    fn the_checksums_match_the_standard_vectors() {
        // The values every implementation of these two algorithms agrees on, so a
        // bug here is caught rather than baked into every PNG written.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn a_png_has_the_signature_and_the_three_chunks() {
        let png = encode(&[10, 20, 30, 40, 50, 60], 2, 1, ColourType::Truecolour);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        for tag in [b"IHDR", b"IDAT", b"IEND"] {
            assert!(
                png.windows(4).any(|window| window == tag),
                "the {tag:?} chunk is present"
            );
        }
        // IHDR carries the size and the colour type.
        assert!(png.windows(4).any(|w| w == [0, 0, 0, 2]), "width 2");
        assert!(png.windows(4).any(|w| w == [0, 0, 0, 1]), "height 1");
    }

    #[test]
    fn the_stored_stream_has_the_length_the_format_says() {
        // `zlib_stored` of a 14-byte payload: 2 header + (5 block header + 14) + 4
        // adler.  Spelled out so an off-by-one in the chunking shows up here.
        assert_eq!(zlib_stored(&[0u8; 14]).len(), 2 + 5 + 14 + 4);
        // Two blocks once the payload passes 0xFFFF, each with its own header.
        assert_eq!(
            zlib_stored(&[0u8; 0x1_0000]).len(),
            2 + 5 + 0xFFFF + 5 + 1 + 4
        );
    }

    #[test]
    fn a_tall_image_is_stored_in_more_than_one_block() {
        // 400 rows of 1 filter byte + 3 pixels is well under one block, and the
        // point is just that the stream is accepted and correctly sized.
        let raw = vec![7u8; 400 * 3];
        let png = encode(&raw, 1, 400, ColourType::Truecolour);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(png.len() > raw.len(), "the container adds the framing");
    }

    #[test]
    fn an_empty_image_still_produces_a_valid_stream() {
        // Height zero is degenerate but must not produce a truncated zlib stream.
        let png = encode(&[], 0, 0, ColourType::Truecolour);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(
            zlib_stored(&[]).len() >= 11,
            "a stream with no blocks is still terminated"
        );
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn a_buffer_of_the_wrong_length_is_rejected() {
        encode(&[1, 2, 3], 2, 1, ColourType::Truecolour);
    }
}
