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

//! The debugger driven the way the window drives it.
//!
//! These go through the panel view and the engine rather than a window, so they
//! cover the glue a window would supply: composing a frame at the size the window
//! asks for, and arming a breakpoint by clicking the row it is on.
//!
//! They need the ROM and a font, so they skip rather than fail when either is
//! missing -- the convention the rest of the workspace uses for user-supplied
//! data.

use fx991::Emu;
use fx991_dbg::Debugger;
use fx991_dbgui::{DebuggerUi, FontSet, MIN_HEIGHT, MIN_WIDTH};

/// The pieces a window would hold, without a window.
struct Harness {
    emu: Emu,
    ui: DebuggerUi,
}

fn harness() -> Option<Harness> {
    let rom = std::fs::read(testkit::rom_path()).ok()?;
    let font = FontSet::from_file(testkit::data_file("font.ttf")).ok()?;
    Some(Harness {
        emu: Emu::from_rom(rom),
        ui: DebuggerUi::with_font(font),
    })
}

#[test]
fn a_frame_composes_at_the_size_the_window_asks_for() {
    let Some(mut harness) = harness() else {
        return;
    };
    let mut debugger = Debugger::new();
    // The size the window opens the debugger at.
    let frame = harness
        .ui
        .render(&mut harness.emu, &mut debugger, 1280, 800)
        .expect("the font is loaded");
    assert_eq!((frame.width, frame.height), (1280, 800));
}

#[test]
fn the_panels_compose_for_a_booted_machine() {
    let Some(mut harness) = harness() else {
        return;
    };
    // Boot to the key-wait loop, which is where a user would first press F12.
    for _ in 0..8_000_000 {
        harness.emu.step();
        if harness.emu.chipset.ready_for_key() {
            break;
        }
    }
    assert!(
        harness.emu.chipset.ready_for_key(),
        "the ROM reached its key-wait loop"
    );

    let mut debugger = Debugger::new();
    let frame = harness
        .ui
        .render(&mut harness.emu, &mut debugger, MIN_WIDTH, MIN_HEIGHT)
        .expect("the font is loaded");
    assert_eq!(frame.rgba.len(), (MIN_WIDTH * MIN_HEIGHT * 4) as usize);
}

#[test]
fn a_breakpoint_set_by_clicking_stops_the_machine() {
    let Some(mut harness) = harness() else {
        return;
    };
    let mut debugger = Debugger::new();

    // Click the disassembly row the PC is on, which is what the window does when
    // a user clicks there.
    let layout = harness.ui.layout(MIN_WIDTH, MIN_HEIGHT);
    let rect = layout.disassembly;
    let content = rect.content(harness.ui.metrics());
    let row = rect.rows(harness.ui.metrics()) / 2;
    let y = content.y + row as u32 * harness.ui.metrics().cell_h + 2;
    harness.ui.click(
        &mut harness.emu,
        &mut debugger,
        MIN_WIDTH,
        MIN_HEIGHT,
        rect.x + 40,
        y,
    );
    let armed = debugger
        .breakpoints()
        .next()
        .map(|(_, bp)| bp.address)
        .expect("the click armed a breakpoint");

    // Put the machine exactly on the armed address and run: it must stop there
    // rather than run past.  This is the whole point of the click, and it is
    // independent of which row the window happened to be centred on.
    harness.emu.chipset.cpu.regs.csr = (armed >> 16) as u16;
    harness.emu.chipset.cpu.regs.pc = armed as u16;
    let reason = debugger.run(&mut harness.emu, 100_000);
    assert!(
        matches!(reason, fx991_dbg::StopReason::Breakpoint { .. }),
        "the run stopped at the breakpoint, not {reason:?}"
    );
    assert_eq!(harness.emu.pc(), armed, "it stopped on the armed address");
}

#[test]
fn a_click_arms_the_address_the_clicked_row_shows() {
    let Some(mut harness) = harness() else {
        return;
    };
    let mut debugger = Debugger::new();

    // Every row of the disassembly maps to the address drawn on it, which is the
    // property that makes a click meaningful.  Clicking each row in turn and
    // checking the armed address against the listing is what pins it.
    let layout = harness.ui.layout(MIN_WIDTH, MIN_HEIGHT);
    let rect = layout.disassembly;
    let content = rect.content(harness.ui.metrics());
    let metrics = *harness.ui.metrics();
    let rows = rect.rows(&metrics);
    let start = harness
        .ui
        .disassembly_start(&mut harness.emu, &mut debugger, rows);

    for row in 0..rows {
        let y = content.y + row as u32 * metrics.cell_h + 2;
        harness.ui.click(
            &mut harness.emu,
            &mut debugger,
            MIN_WIDTH,
            MIN_HEIGHT,
            rect.x + 40,
            y,
        );
        let armed: Vec<u32> = debugger.breakpoints().map(|(_, bp)| bp.address).collect();
        assert_eq!(armed.len(), 1, "row {row} armed one breakpoint: {armed:?}");

        // The address the panel drew on that row, from the same listing the
        // drawing used.
        let drawn = fx991_dbgui::panels::disassembly(&mut debugger, &mut harness.emu, start, rows);
        let expected = drawn.rows[row]
            .text()
            .split('h')
            .next()
            .and_then(|hex| u32::from_str_radix(hex.trim(), 16).ok())
            .expect("the row starts with an address");
        assert_eq!(
            armed[0], expected,
            "row {row} shows {expected:#07x} but armed {:#07x}",
            armed[0]
        );

        // Clear it again so the next row is measured on its own.
        harness.ui.click(
            &mut harness.emu,
            &mut debugger,
            MIN_WIDTH,
            MIN_HEIGHT,
            rect.x + 40,
            y,
        );
        assert_eq!(
            debugger.breakpoint_count(),
            0,
            "the second click cleared it"
        );
    }
}

#[test]
fn scrolling_the_disassembly_moves_the_listing_and_leaves_the_pc_behind() {
    let Some(mut harness) = harness() else {
        return;
    };
    let mut debugger = Debugger::new();

    // The window starts on the PC.
    assert!(harness.ui.is_following(), "it starts following the machine");
    let rows = harness
        .ui
        .layout(MIN_WIDTH, MIN_HEIGHT)
        .disassembly
        .rows(harness.ui.metrics());
    let start = harness
        .ui
        .disassembly_start(&mut harness.emu, &mut debugger, rows);

    // One row down moves the anchor to the next instruction, not further: an
    // address has to land on an instruction boundary, so the step is the length
    // of the instruction at the anchor rather than a fixed number of bytes.
    harness.ui.scroll(
        &mut harness.emu,
        &mut debugger,
        MIN_WIDTH,
        MIN_HEIGHT,
        fx991_dbgui::Panel::Disassembly,
        1,
    );
    assert!(!harness.ui.is_following(), "scrolling leaves the PC");
    let moved = harness
        .ui
        .disassembly_start(&mut harness.emu, &mut debugger, rows);
    let expected = fx991_dbgui::panels::next_row(&mut debugger, &mut harness.emu, start);
    assert_eq!(moved, expected, "one row down is one instruction");

    // And back up returns to where it was.
    harness.ui.scroll(
        &mut harness.emu,
        &mut debugger,
        MIN_WIDTH,
        MIN_HEIGHT,
        fx991_dbgui::Panel::Disassembly,
        -1,
    );
    let back = harness
        .ui
        .disassembly_start(&mut harness.emu, &mut debugger, rows);
    assert_eq!(back, start, "one row up undoes one row down");
}

#[test]
fn following_the_machine_re_anchors_the_disassembly() {
    let Some(mut harness) = harness() else {
        return;
    };
    let mut debugger = Debugger::new();
    let rows = harness
        .ui
        .layout(MIN_WIDTH, MIN_HEIGHT)
        .disassembly
        .rows(harness.ui.metrics());

    // Scroll far away, then ask to follow again.
    harness.ui.scroll(
        &mut harness.emu,
        &mut debugger,
        MIN_WIDTH,
        MIN_HEIGHT,
        fx991_dbgui::Panel::Disassembly,
        5,
    );
    assert!(!harness.ui.is_following());
    harness.ui.follow_pc();
    assert!(harness.ui.is_following());

    // The anchor is the PC's window again.
    let after = harness
        .ui
        .disassembly_start(&mut harness.emu, &mut debugger, rows);
    let expected =
        fx991_dbgui::panels::disassembly_start_at_pc(&mut debugger, &mut harness.emu, rows);
    assert_eq!(after, expected, "following puts the PC back in view");
}
