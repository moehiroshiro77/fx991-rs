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

//! Golden-image tests for the renderer.
//!
//! Each test compares a whole composed frame against a recorded FNV-1a digest.
//! That catches any change to the compositing -- a blend that rounds differently,
//! an indicator drawn at the wrong offset, the dot matrix off by a row -- that a
//! per-pixel assertion would miss.
//!
//! The digests are a change detector, not a proof of correctness: they were
//! recorded from this renderer once its output had been checked against the
//! documented LCD values.  When a change moves one, look at the affected pixels
//! before updating it.

use fx991_chipset::screen::BUFFER_SIZE;
use fx991_ui::layout::{INDICATORS, SKIN_H, SKIN_SOURCE_H, SKIN_SOURCE_W, SKIN_W};
use fx991_ui::{alpha_levels, Renderer};

/// A renderer with the repo's face texture, skipping when it is absent.
fn renderer() -> Option<Renderer> {
    fx991_ui::Skin::from_file(testkit::skin_path())
        .ok()
        .map(Renderer::new)
}

/// Boot a calculator, or `None` when the ROM is absent.
fn boot() -> Option<fx991::Calculator> {
    testkit::rom_bytes()
        .ok()
        .map(|rom| fx991::Calculator::from_rom(rom).expect("boot"))
}

/// Bail out of a test whose data is missing.
///
/// The ROM and the face texture are not distributed with this repository, so a
/// fresh clone has neither.  Returning early keeps the suite green there instead
/// of reporting failures the user cannot fix.
macro_rules! data_or_skip {
    ($value:expr) => {
        match $value {
            Some(value) => value,
            None => {
                eprintln!("skipping: the ROM or the face texture is not present");
                return;
            }
        }
    };
}

/// A screen buffer with every indicator lit and the dot matrix blank.
fn indicators_on() -> Vec<u8> {
    let mut buffer = vec![0u8; BUFFER_SIZE];
    for indicator in INDICATORS {
        buffer[indicator.offset as usize] |= indicator.mask;
    }
    buffer
}

fn digest_of(buffer: &[u8], mode: u8, contrast: u8) -> Option<u64> {
    renderer().map(|r| r.render_buffer(buffer, mode, contrast).digest())
}

// The baselines, from  at contrast 20.
const GOLDEN_BLANK: u64 = 0x28A8_2513_6DDE_BD78;
const GOLDEN_INDICATORS_ON: u64 = 0x16AC_B3E3_A206_D757;
const GOLDEN_OFF_MODE: u64 = 0xC071_7585_4B00_4399;
const GOLDEN_MODE_4: u64 = 0x6CFE_6634_9674_5B99;
const GOLDEN_MODE_6: u64 = 0xCCD3_9CF2_271E_E505;

#[test]
fn a_pressed_button_is_highlighted_over_the_skin() {
    // `Keyboard::Frame` fills the pressed button's rectangle after the chipset
    // has drawn, so the highlight covers the skin.
    let mut calculator = data_or_skip!(boot());
    let renderer = data_or_skip!(renderer());

    let before = renderer.render(&mut calculator.emu.chipset);
    // '7' sits at (29, 404) 41x29.
    let untouched = before.pixel(49, 418);

    calculator.emu.chipset.press_key(0x02);
    let after = renderer.render(&mut calculator.emu.chipset);
    let pressed = after.pixel(49, 418);

    assert_ne!(untouched, pressed, "pressing '7' did not change its pixels");
    // Black at 50% over the skin: each channel roughly halves (235 -> ~118).
    for channel in 0..3 {
        assert!(
            (pressed[channel] as u32) < (untouched[channel] as u32) * 3 / 4,
            "highlight too weak on channel {channel}: {untouched:?} -> {pressed:?}"
        );
    }

    // Outside the button is untouched.
    assert_eq!(after.pixel(49, 400), before.pixel(49, 400));
}

#[test]
fn a_stuck_button_highlights_in_a_different_colour() {
    // A right-click latches the key and it draws dark
    // red instead of black, so the two states are distinguishable on screen.
    let mut calculator = data_or_skip!(boot());
    let renderer = data_or_skip!(renderer());

    calculator.emu.chipset.press_key(0x02);
    let held = renderer.render(&mut calculator.emu.chipset).pixel(49, 418);

    calculator.emu.chipset.keyboard.borrow_mut().release(None);
    calculator
        .emu
        .chipset
        .keyboard
        .borrow_mut()
        .press_with(0x02, true);
    let stuck = renderer.render(&mut calculator.emu.chipset).pixel(49, 418);

    assert_ne!(held, stuck, "stuck and held look identical");
    // Held is neutral black; stuck keeps a red bias.
    assert!(
        stuck[0] > held[0],
        "stuck should be redder (less black) than held: {held:?} vs {stuck:?}"
    );
}

#[test]
fn a_stuck_key_survives_release_all_but_a_held_one_does_not() {
    // This is what makes right-click useful for SHIFT-then-click.
    // (`calculator` needs no `mut`: the keyboard is behind an `Rc<RefCell<_>>`,
    // so `borrow_mut` takes the interior mutability, not the binding.)
    let calculator = data_or_skip!(boot());
    {
        let mut keyboard = calculator.emu.chipset.keyboard.borrow_mut();
        keyboard.press_with(0x07, true); // SHIFT, latched
        assert!(keyboard.is_pressed(0x07));
        keyboard.press(0x00); // '1', held normally
        assert!(keyboard.is_pressed(0x00));

        keyboard.release(None); // the mouse came up
        assert!(keyboard.is_pressed(0x07), "the latched key was released");
        assert!(!keyboard.is_pressed(0x00), "the held key was not released");

        // A second right-click clears the latch.
        keyboard.press_with(0x07, true);
        assert!(!keyboard.is_pressed(0x07));
    }
}

#[test]
fn the_skin_blob_is_the_one_the_generator_produced() {
    let renderer = data_or_skip!(renderer());
    assert_eq!(renderer.skin().width(), SKIN_SOURCE_W);
    assert_eq!(renderer.skin().height(), SKIN_SOURCE_H);
    // 307 * 615 * 4
    assert_eq!(
        renderer.skin().width() * renderer.skin().height() * 4,
        755_220
    );
}

#[test]
fn an_indicator_keeps_its_glyph_shape() {
    // The indicator sprites are antialiased glyphs: their alpha varies per
    // pixel (89 at an edge, 255 in a stem).  `SetTextureAlphaMod` *modulates*
    // that, so the rendered glyph must still show the shape.  Replacing the
    // texture alpha with the mode alpha instead smears the glyph into a solid
    // block -- every pixel nearly the same colour -- which is what this pins.
    let renderer = data_or_skip!(renderer());
    let frame = renderer.render_buffer(&indicators_on(), 5, 20);

    // rsd_s lands at (47, 102) and is 8x8.
    let mut shades: Vec<u8> = Vec::new();
    for y in 102..110 {
        for x in 47..55 {
            shades.push(frame.pixel(x, y)[0]);
        }
    }
    let darkest = *shades.iter().min().unwrap();
    let lightest = *shades.iter().max().unwrap();
    assert!(
        lightest as i32 - darkest as i32 > 100,
        "rsd_s rendered flat (min {darkest}, max {lightest}); the sprite's own \
         alpha is being discarded instead of modulated"
    );
    // And the ink really is dark where the glyph is solid.
    assert!(darkest < 40, "no solid ink in rsd_s: min {darkest}");
}

#[test]
fn a_frame_is_the_visible_crop() {
    let frame = data_or_skip!(renderer()).render_buffer(&vec![0u8; BUFFER_SIZE], 5, 20);
    assert_eq!(frame.width, SKIN_W);
    assert_eq!(frame.height, SKIN_H);
    assert_eq!(frame.rgba.len() as u32, SKIN_W * SKIN_H * 4);
}

#[test]
fn blank_screen_matches_the_recorded_digest() {
    let digest = data_or_skip!(digest_of(&vec![0u8; BUFFER_SIZE], 5, 20));
    assert_eq!(
        digest, GOLDEN_BLANK,
        "the blank mode-5 frame no longer matches the recorded digest"
    );
}

#[test]
fn indicators_on_matches_the_recorded_digest() {
    let digest = data_or_skip!(digest_of(&indicators_on(), 5, 20));
    assert_eq!(
        digest, GOLDEN_INDICATORS_ON,
        "indicator rendering no longer matches the recorded digest"
    );
}

#[test]
fn the_three_lcd_states_are_exactly_right() {
    // P4/P7/P8 of, the values §3.0.1 predicts.
    let renderer = data_or_skip!(renderer());

    // State 1: mode outside {4,5,6} -> the skin's own LCD colour, nothing drawn.
    // Lights are present in the buffer and must be ignored.
    let lit = indicators_on();
    let off = renderer.render_buffer(&lit, 3, 20);
    assert_eq!(&off.pixel(100, 150)[..3], &[214, 227, 214]);

    // States 2 and 3: one dot lit, its neighbour not.
    let mut buffer = vec![0u8; BUFFER_SIZE];
    buffer[32] = 0x80; // row 0, first visible byte, MSB
    let on = renderer.render_buffer(&buffer, 5, 20);
    assert_eq!(&on.pixel(47, 112)[..3], &[25, 46, 75], "lit dot");
    assert_eq!(&on.pixel(48, 112)[..3], &[152, 167, 168], "unlit dot");
}

#[test]
fn mode_4_and_6_clear_the_dots_without_reading_the_buffer() {
    let renderer = data_or_skip!(renderer());
    let mut buffer = vec![0u8; BUFFER_SIZE];
    buffer[32] = 0xFF; // eight lit dots on row 0

    // mode 5 draws them.
    let five = renderer.render_buffer(&buffer, 5, 20);
    assert_eq!(&five.pixel(47, 112)[..3], &[25, 46, 75]);

    // mode 4 clears them even though the buffer says lit.
    let four = renderer.render_buffer(&buffer, 4, 20);
    assert_eq!(&four.pixel(47, 112)[..3], &[152, 167, 168]);

    // mode 6 clears them too, and uses the 80/20 alphas.
    let six = renderer.render_buffer(&buffer, 6, 20);
    assert_eq!(alpha_levels(20, 6), (80, 20));
    let lit6 = six.pixel(47, 112);
    assert_ne!(
        &lit6[..3],
        &[25, 46, 75],
        "mode 6 should not use the full ink"
    );
}

#[test]
fn mode_gating_covers_every_other_value() {
    // P4: anything outside {4,5,6} draws nothing, including indicators.
    let lit = indicators_on();
    for mode in [0u8, 1, 2, 3, 7] {
        let frame = data_or_skip!(renderer()).render_buffer(&lit, mode, 20);
        // The indicator strip sits at y=102.109; it must be untouched skin.
        let skin_pixel = data_or_skip!(renderer())
            .render_buffer(&vec![0u8; BUFFER_SIZE], mode, 20)
            .pixel(50, 105);
        assert_eq!(
            frame.pixel(50, 105),
            skin_pixel,
            "mode {mode} should not draw indicators"
        );
    }
}

#[test]
fn every_golden_digest_holds() {
    // One place listing all five, so a change to any of them is visible here.
    let blank = vec![0u8; BUFFER_SIZE];
    let lit = indicators_on();
    for (label, buffer, mode, expected) in [
        ("blank", &blank, 5u8, GOLDEN_BLANK),
        ("indicators on", &lit, 5, GOLDEN_INDICATORS_ON),
        ("off mode", &lit, 3, GOLDEN_OFF_MODE),
        ("mode 4", &lit, 4, GOLDEN_MODE_4),
        ("mode 6", &lit, 6, GOLDEN_MODE_6),
    ] {
        let digest = data_or_skip!(digest_of(buffer, mode, 20));
        assert_eq!(digest, expected, "{label} (mode {mode}) diverged");
    }
}

#[test]
fn the_indicator_atlas_is_outside_the_visible_crop() {
    // P9 /: if the skin were cropped to the body before
    // sampling, every indicator would read transparent pixels and the UI would
    // silently show no indicators at all.
    let renderer = data_or_skip!(renderer());
    let skin = renderer.skin();
    assert!(
        INDICATORS.iter().all(|i| i.sprite.src_y >= SKIN_H as u16),
        "an indicator's source row is inside the visible crop"
    );
    assert!(
        skin.height() > SKIN_H,
        "the skin must keep the rows below the crop"
    );
    // And the atlas really does have ink there.
    let on = renderer.render_buffer(&indicators_on(), 5, 20);
    let off = renderer.render_buffer(&vec![0u8; BUFFER_SIZE], 5, 20);
    let differing = (102..110)
        .flat_map(|y| (47..55).map(move |x| (x, y)))
        .filter(|&(x, y)| on.pixel(x, y) != off.pixel(x, y))
        .count();
    assert!(differing > 0, "rsd_s drew nothing");
}

#[test]
fn the_dot_matrix_is_63_by_192_pixels() {
    let renderer = data_or_skip!(renderer());

    // The buffer is 64 rows of 32 bytes: row 0 holds the indicators and rows
    // 1.63 hold dots, so a dot row `iy` reads from `iy*32 + 32`.
    // The last dot row is iy = 62, i.e. offsets 2016.2039, and its last
    // visible pixel is (238, 112 + 62).
    let mut buffer = vec![0u8; BUFFER_SIZE];
    buffer[(62 * 32) + 32 + 23] = 0x01; // row 62, byte 23, bit 0
    let frame = renderer.render_buffer(&buffer, 5, 20);
    assert_eq!(&frame.pixel(238, 112 + 62)[..3], &[25, 46, 75]);

    // There is no 64th dot row: the dots stop at y = 174, and the pixel below
    // is untouched skin.
    assert_eq!(&frame.pixel(238, 112 + 63)[..3], &[214, 227, 214]);

    // Row 0 of the buffer is the *indicator* row.  Lighting it must move the
    // indicators and leave the dot matrix alone -- they are different regions
    // of the same buffer, and confusing them would smear indicators into dots.
    let mut indicators = vec![0u8; BUFFER_SIZE];
    indicators[0] = 0x01; // rsd_s
    let frame = renderer.render_buffer(&indicators, 5, 20);
    assert_eq!(
        &frame.pixel(47, 112)[..3],
        &[152, 167, 168],
        "the indicator row must not light a dot"
    );
    let blank = renderer.render_buffer(&vec![0u8; BUFFER_SIZE], 5, 20);
    assert_ne!(
        frame.pixel(50, 105),
        blank.pixel(50, 105),
        "rsd_s should have drawn"
    );
}

#[test]
fn the_padding_bytes_of_a_row_are_not_displayed() {
    // Each row is 32 bytes but only the first 24 are visible, so the dot matrix
    // is 192 pixels wide (x 47.238) and the padding never reaches the screen.
    let renderer = data_or_skip!(renderer());
    let mut buffer = vec![0u8; BUFFER_SIZE];
    buffer[32 + 24] = 0xFF; // the first padding byte of row 0
    let frame = renderer.render_buffer(&buffer, 5, 20);

    // One past the last dot pixel: the skin's own LCD colour, no ink.
    assert_eq!(&frame.pixel(239, 112)[..3], &[214, 227, 214]);
    // While the last *visible* pixel of that row did get its light ink.
    assert_eq!(&frame.pixel(238, 112)[..3], &[152, 167, 168]);
}
