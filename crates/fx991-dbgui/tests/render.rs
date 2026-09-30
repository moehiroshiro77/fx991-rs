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

//! The panels drawn for real, with a font.
//!
//! These need a font file the user supplies, so they skip rather than fail when
//! it is absent -- the convention the rest of the workspace uses for the ROM and
//! the skin.
//!
//! The assertions are about *structure*, not pixels: a pixel digest would change
//! with the user's choice of face, which would make the test a test of their font
//! rather than of this crate.  What is pinned here is that the panels actually
//! put ink where their rectangles say, that the marks land on the right rows, and
//! that nothing escapes its own panel.

use fx991::Emu;
use fx991_dbg::{Debugger, Watch, WatchKind};
use fx991_dbgui::{DebuggerUi, FontSet, Panel, MIN_HEIGHT, MIN_WIDTH};

/// The debugger's font, or `None` when the user has not supplied one.
fn font() -> Option<FontSet> {
    FontSet::from_file(testkit::data_file("font.ttf")).ok()
}

/// A machine with a small program, without needing a ROM file.
fn emu() -> Emu {
    let mut rom = vec![0u8; 0x4_0000];
    rom[0x0000] = 0x00;
    rom[0x0001] = 0xF0;
    rom[0x0002] = 0x00;
    rom[0x0003] = 0x90;
    // A couple of instructions at the entry, so the disassembly has content.
    rom[0x1_0000] = 0x00; // MOV R0, #0x11
    rom[0x1_0001] = 0x11;
    rom[0x1_0002] = 0x10; // MOV R1, #0x22
    rom[0x1_0003] = 0x22;
    let mut emu = Emu::from_rom(rom);
    {
        let mut interrupts = emu.chipset.interrupts.borrow_mut();
        interrupts.active = [false; fx991_chipset::INT_COUNT];
        interrupts.pending_count = 0;
    }
    emu.chipset.cpu.regs.csr = 1;
    emu.chipset.cpu.regs.pc = 0x0000;
    emu
}

/// Whether any pixel in a rectangle differs from the panel's own background.
fn has_ink(frame: &fx991_ui::Frame, rect: fx991_dbgui::Rect) -> bool {
    let background = fx991_dbgui::theme::PANEL;
    for y in rect.y..rect.y + rect.h {
        for x in rect.x..rect.x + rect.w {
            if x >= frame.width || y >= frame.height {
                continue;
            }
            let pixel = frame.pixel(x, y);
            // The chrome and the text are both unlike the flat panel colour.
            if pixel != background && pixel != fx991_dbgui::theme::PANEL_FOCUS {
                return true;
            }
        }
    }
    false
}

#[test]
fn a_rendered_frame_is_the_size_it_was_asked_for() {
    let Some(font) = font() else {
        return;
    };
    let mut emu = emu();
    let mut debugger = Debugger::new();
    let mut ui = DebuggerUi::with_font(font);

    let frame = ui
        .render(&mut emu, &mut debugger, MIN_WIDTH, MIN_HEIGHT)
        .expect("a font is loaded");
    assert_eq!(frame.width, MIN_WIDTH);
    assert_eq!(frame.height, MIN_HEIGHT);
    assert_eq!(
        frame.rgba.len(),
        (MIN_WIDTH * MIN_HEIGHT * 4) as usize,
        "the buffer is exactly the frame"
    );
}

#[test]
fn every_panel_draws_something_in_its_own_rectangle() {
    let Some(font) = font() else {
        return;
    };
    let mut emu = emu();
    let mut debugger = Debugger::new();
    debugger.add_breakpoint_at(0x1_0002);
    debugger.add_watch(&mut emu, Watch::new(0x0_D180, WatchKind::ReadWrite));
    let mut ui = DebuggerUi::with_font(font);

    let frame = ui
        .render(&mut emu, &mut debugger, 1280, 800)
        .expect("a font is loaded");
    let layout = ui.layout(1280, 800);

    for panel in Panel::ALL {
        let rect = layout.rect(*panel);
        assert!(has_ink(&frame, rect), "{panel:?} drew nothing in {rect:?}");
    }
}

#[test]
fn nothing_is_drawn_outside_the_frame() {
    let Some(font) = font() else {
        return;
    };
    let mut emu = emu();
    let mut debugger = Debugger::new();
    let mut ui = DebuggerUi::with_font(font);

    // A frame at the minimum size, where every panel is as tight as it gets and
    // a long row is most likely to overrun.
    let frame = ui
        .render(&mut emu, &mut debugger, MIN_WIDTH, MIN_HEIGHT)
        .expect("a font is loaded");
    assert_eq!(
        frame.rgba.len(),
        (MIN_WIDTH * MIN_HEIGHT * 4) as usize,
        "the buffer is exactly the frame, so an overrun could not be written"
    );

    // The corner pixel belongs to the toolbar's border rather than to the
    // background: the toolbar is the full width of the frame.
    let border = fx991_dbgui::theme::BORDER;
    assert_eq!(frame.pixel(0, 0), border, "the frame's edge is the border");
    assert_eq!(
        frame.pixel(MIN_WIDTH - 1, MIN_HEIGHT - 1),
        border,
        "and so is the far corner"
    );
}

#[test]
fn the_current_instruction_is_marked_on_the_row_the_pc_is_on() {
    let Some(font) = font() else {
        return;
    };
    let mut emu = emu();
    let mut debugger = Debugger::new();
    let mut ui = DebuggerUi::with_font(font);
    let layout = ui.layout(MIN_WIDTH, MIN_HEIGHT);

    let frame = ui
        .render(&mut emu, &mut debugger, MIN_WIDTH, MIN_HEIGHT)
        .expect("a font is loaded");

    let metrics = *ui.metrics();
    let rect = layout.disassembly;
    let rows = rect.rows(&metrics);
    let content = rect.content(&metrics);

    // Which rows carry ink in the gutter, where the marker is drawn.  Glyph
    // coverage is antialiased, so a mark is "pixels unlike the panel", not "a
    // pixel exactly equal to the marker colour".
    let marked_rows: Vec<usize> = (0..rows)
        .filter(|row| {
            let y = content.y + *row as u32 * metrics.cell_h;
            (content.x..content.x + metrics.cell_w * 2).any(|x| {
                (0..metrics.cell_h).any(|dy| {
                    let pixel = frame.pixel(x, y + dy);
                    pixel != fx991_dbgui::theme::PANEL && pixel != fx991_dbgui::theme::PANEL_FOCUS
                })
            })
        })
        .collect();

    // Exactly one row is marked, and it is the one the window centres the PC on.
    assert_eq!(
        marked_rows,
        vec![rows / 2],
        "the only marked row should be the PC's"
    );
}

#[test]
fn a_wider_frame_shows_more_columns_without_moving_the_panels() {
    let Some(font) = font() else {
        return;
    };
    let ui = DebuggerUi::with_font(font);

    let narrow = ui.layout(MIN_WIDTH, MIN_HEIGHT);
    let wide = ui.layout(MIN_WIDTH * 2, MIN_HEIGHT);
    // The disassembly grows; the toolbar's height does not change with width.
    assert!(wide.disassembly.w > narrow.disassembly.w);
    assert_eq!(wide.toolbar.h, narrow.toolbar.h);
    assert_eq!(wide.disassembly.y, narrow.disassembly.y);
}

/// Write a frame to a PNG, so the layout can be looked at by eye.
///
/// Ignored by default: it needs a font *and* writes a file, so it is a manual
/// aid rather than a gate.  Run it with
/// `cargo test -p fx991-dbgui --test render -- --ignored --nocapture`.
#[test]
#[ignore]
fn write_a_sample_frame() {
    let Some(font) = font() else {
        eprintln!("no data/font.ttf, nothing to draw");
        return;
    };
    let mut emu = emu();
    let mut debugger = Debugger::new();
    debugger.add_breakpoint_at(0x1_0002);
    debugger
        .add_conditional_breakpoint(0x1_0000, "r0 == 1")
        .expect("parses");
    debugger.add_watch(&mut emu, Watch::new(0x0_D180, WatchKind::ReadWrite));
    emu.set_reg(0, 0x11);
    emu.set_reg(1, 0x22);
    emu.poke(0x0_D180, b'9');
    emu.poke(0x0_D181, 0xA8);
    emu.poke(0x0_D182, b'9');

    let mut ui = DebuggerUi::with_font(font);
    let frame = ui
        .render(&mut emu, &mut debugger, 1280, 800)
        .expect("a font is loaded");
    // Written inside this crate rather than into the workspace's build directory:
    // a test runs with the crate as its working directory, and a relative path
    // out of the crate would make the test depend on the checkout's shape.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sample-frame.png");
    std::fs::write(&path, frame.to_png()).expect("write the sample");
    println!("wrote {}", path.display());
}
