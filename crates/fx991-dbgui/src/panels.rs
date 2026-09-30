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

//! What each panel shows, as rows of styled text.
//!
//! A panel is built in two steps: this module turns machine state into [`Row`]s,
//! and [`crate::ui`] draws them.  Keeping the content separate is what makes it
//! testable without a font or a GPU -- a test asserts on the text of a row, and
//! the drawing is a loop over the same rows.
//!
//! Every read goes through the side-effect-free accessors (`peek_quiet`,
//! `read_code_quiet`), so scrolling a panel cannot trip a watchpoint or add a
//! fault to the log.  A debugger's own inspection must not look like the guest's.

use fx991::Emu;
use fx991_dbg::{Breakpoint, Debugger, StopReason};

use crate::theme::{self, Rgba};

/// One run of text in a row, in one colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// The text to draw.
    pub text: String,
    /// What colour to draw it in.
    pub colour: Rgba,
}

impl Span {
    /// A span of one colour.
    pub fn new(text: impl Into<String>, colour: Rgba) -> Self {
        Self {
            text: text.into(),
            colour,
        }
    }
}

/// How a row is marked, which the panel draws in a gutter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// No mark.
    None,
    /// The instruction the PC is on.
    Current,
    /// An armed breakpoint.
    Breakpoint,
    /// A row the user has selected.
    Selected,
}

/// One line of a panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The styled runs, drawn left to right.
    pub spans: Vec<Span>,
    /// The gutter mark.
    pub marker: Marker,
}

impl Row {
    /// A row of one colour.
    pub fn plain(text: impl Into<String>, colour: Rgba) -> Self {
        Self {
            spans: vec![Span::new(text, colour)],
            marker: Marker::None,
        }
    }

    /// A row built from several runs.
    pub fn spans(spans: Vec<Span>) -> Self {
        Self {
            spans,
            marker: Marker::None,
        }
    }

    /// The row's text, ignoring colour -- what a test asserts on.
    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }

    /// The row's text padded to `width` columns.
    ///
    /// Panels pad so a background band spans the whole panel; without it a
    /// highlighted row would stop at its last character.
    pub fn text_padded(&self, width: usize) -> String {
        let mut text = self.text();
        let len = text.chars().count();
        if len < width {
            text.extend(std::iter::repeat_n(' ', width - len));
        }
        text
    }

    /// Set the gutter mark.
    pub fn with_marker(mut self, marker: Marker) -> Self {
        self.marker = marker;
        self
    }
}

/// A panel's worth of rows, and where the interesting row is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Content {
    /// The rows, top to bottom.
    pub rows: Vec<Row>,
    /// The row index that should be scrolled into view, if any.
    pub focus: Option<usize>,
}

impl Content {
    /// Content from rows with no particular focus.
    pub fn new(rows: Vec<Row>) -> Self {
        Self { rows, focus: None }
    }

    /// How many rows there are.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there is nothing to show.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// How the machine came to be stopped, for the toolbar.
pub fn stop_text(reason: &StopReason) -> String {
    match reason {
        StopReason::Breakpoint { id, address } => {
            format!("breakpoint {} at {address:07X}h", id.0)
        }
        StopReason::Watch {
            id,
            address,
            write,
            value,
            at,
        } => format!(
            "watch {} {} {address:05X}h = {value:02X} (at {at:07X}h)",
            id.0,
            if *write { "wrote" } else { "read" },
        ),
        StopReason::Stopped => "stopped (waiting for an interrupt)".to_string(),
        StopReason::Budget { budget, at } => {
            format!("budget {budget} spent at {at:07X}h")
        }
        StopReason::Paused => "paused".to_string(),
    }
}

/// The toolbar: where the machine is, and how it stopped.
pub fn toolbar(emu: &mut Emu, stop: Option<&StopReason>, tracing: bool) -> Row {
    let pc = emu.pc();
    let regs = &emu.chipset.cpu.regs;
    let mut spans = vec![
        Span::new(format!("PC {pc:07X}h"), theme::CURRENT),
        Span::new(format!("   tick {}", emu.ticks), theme::TEXT_DIM),
        Span::new(format!("   SP {:04X}h", regs.sp), theme::TEXT),
    ];
    if tracing {
        spans.push(Span::new("   TRACE", theme::WARN));
    }
    if let Some(reason) = stop {
        spans.push(Span::new(
            format!("   {}", stop_text(reason)),
            theme::TEXT_DIM,
        ));
    }
    Row::spans(spans)
}

/// The disassembly, with the current instruction and any breakpoint marked.
///
/// `rows` is how many lines fit; the window is centred on the PC, so stepping
/// does not scroll the view out from under the reader.
pub fn disassembly(debugger: &mut Debugger, emu: &mut Emu, rows: usize, offset: isize) -> Content {
    if rows == 0 {
        return Content::default();
    }
    // Half the window above the PC, so the instruction being executed sits near
    // the middle where the eye already is.
    let before = (rows / 2) as isize + offset;
    let before = before.max(0) as usize;
    let after = rows.saturating_sub(before + 1);

    let pc = emu.pc();
    let insns = debugger.disassemble_insns_at_pc(emu, before, after);

    // Which addresses carry a breakpoint, so the gutter can mark them.
    let armed: Vec<u32> = debugger
        .breakpoints()
        .filter(|(_, bp)| bp.enabled && bp.stops())
        .map(|(_, bp)| bp.address)
        .collect();

    let mut out = Vec::with_capacity(insns.len());
    let mut focus = None;
    for (index, insn) in insns.iter().enumerate() {
        if insn.address == pc {
            focus = Some(index);
        }
        let marker = if insn.address == pc {
            Marker::Current
        } else if armed.contains(&insn.address) {
            Marker::Breakpoint
        } else {
            Marker::None
        };
        let colour = if insn.address == pc {
            theme::CURRENT
        } else {
            theme::TEXT
        };
        let mut spans = vec![
            Span::new(format!("{:07X}h ", insn.address), theme::TEXT_DIM),
            Span::new(format!("{:<9}", insn.byte_text()), theme::TEXT_DIM),
            Span::new(insn.text.clone(), colour),
        ];
        // A call is worth marking: it is what "step over" behaves differently on.
        if insn.is_call {
            spans.push(Span::new("   call", theme::WARN));
        }
        out.push(Row::spans(spans).with_marker(marker));
    }
    Content { rows: out, focus }
}

/// A hex and ASCII dump of the data space.
///
/// `address` is the first byte shown; the dump is 16 bytes to a row, so a row's
/// address is `address + row * 16`.
pub fn memory(emu: &mut Emu, address: u32, rows: usize) -> Content {
    const PER_ROW: u32 = 16;
    if rows == 0 {
        return Content::default();
    }
    let mut out = Vec::with_capacity(rows);
    for row in 0..rows {
        // Masked to the 24-bit space so the address shown is the one the bytes
        // came from: the bus wraps a read at the top of the space, and a row
        // labelled `1000000h` would be showing `0000000h`'s bytes.
        let base = address.wrapping_add(row as u32 * PER_ROW) & 0x00FF_FFFF;
        let bytes = emu.peek_quiet_block(base, PER_ROW as usize);
        let hex: String = bytes.iter().map(|byte| format!("{byte:02X} ")).collect();
        let ascii: String = bytes
            .iter()
            .map(|byte| {
                if (0x20..0x7F).contains(byte) {
                    *byte as char
                } else {
                    '.'
                }
            })
            .collect();
        out.push(Row::spans(vec![
            Span::new(format!("{base:07X}h  "), theme::TEXT_DIM),
            Span::new(hex, theme::TEXT),
            Span::new(format!(" {ascii}"), theme::TEXT_DIM),
        ]));
    }
    Content::new(out)
}

/// The register file.
pub fn registers(emu: &Emu) -> Content {
    let regs = &emu.chipset.cpu.regs;
    let mut out = Vec::new();

    out.push(Row::spans(vec![Span::new(
        format!("{:<4}{:07X}h", "PC", regs.physical_pc()),
        theme::CURRENT,
    )]));
    out.push(Row::spans(vec![
        Span::new("SP  ", theme::TEXT_DIM),
        Span::new(format!("{:04X}h", regs.sp), theme::TEXT),
        Span::new("   EA ", theme::TEXT_DIM),
        Span::new(format!("{:04X}h", regs.ea), theme::TEXT),
    ]));
    out.push(Row::spans(vec![
        Span::new("PSW ", theme::TEXT_DIM),
        Span::new(format!("{:02X}", regs.psw()), theme::TEXT),
        Span::new(format!(" {}", psw_flags(regs.psw())), theme::CHANGED),
    ]));
    out.push(Row::spans(vec![
        Span::new("CSR ", theme::TEXT_DIM),
        Span::new(format!("{}", regs.csr), theme::TEXT),
        Span::new("   LR ", theme::TEXT_DIM),
        Span::new(format!("{:04X}h", regs.lr()), theme::TEXT),
    ]));
    out.push(Row::spans(vec![
        Span::new("DSR ", theme::TEXT_DIM),
        Span::new(format!("{:02X}", regs.dsr), theme::TEXT),
        Span::new("   ELEVEL ", theme::TEXT_DIM),
        Span::new(format!("{}", regs.elevel()), theme::TEXT),
    ]));

    // The sixteen byte registers, four to a row.
    for group in 0..4 {
        let mut spans = Vec::new();
        for index in 0..4 {
            let reg = group * 4 + index;
            spans.push(Span::new(format!("R{reg:<2} "), theme::TEXT_DIM));
            spans.push(Span::new(format!("{:02X}  ", regs.r[reg]), theme::TEXT));
        }
        out.push(Row::spans(spans));
    }

    // The 16-bit view, which is what the ROM mostly uses.
    for group in 0..4 {
        let mut spans = Vec::new();
        for index in 0..2 {
            let reg = (group * 2 + index) * 2;
            spans.push(Span::new(format!("ER{reg:<2} "), theme::TEXT_DIM));
            spans.push(Span::new(format!("{:04X}h  ", regs.er(reg)), theme::TEXT));
        }
        out.push(Row::spans(spans));
    }

    Content::new(out)
}

/// The PSW's set bits, as flag letters.
pub fn psw_flags(psw: u8) -> String {
    use nxu8_core::{PSW_C, PSW_HC, PSW_MIE, PSW_OV, PSW_S, PSW_Z};
    let mut text = String::new();
    for (bit, name) in [
        (PSW_C, 'C'),
        (PSW_Z, 'Z'),
        (PSW_S, 'S'),
        (PSW_OV, 'V'),
        (PSW_HC, 'H'),
        (PSW_MIE, 'I'),
    ] {
        if psw & bit != 0 {
            text.push(name);
        }
    }
    if text.is_empty() {
        "-".to_string()
    } else {
        text
    }
}

/// The stack, from the stack pointer down.
///
/// Each 4-byte group that looks like a code address is labelled with the
/// instruction there, because a ROP chain is a list of addresses and reading it
/// as bare hex is what the labels exist to avoid.
pub fn stack(debugger: &mut Debugger, emu: &mut Emu, rows: usize) -> Content {
    if rows == 0 {
        return Content::default();
    }
    let sp = emu.sp();
    let mut out = Vec::with_capacity(rows);
    out.push(Row::spans(vec![Span::new(
        format!("SP {sp:04X}h"),
        theme::CURRENT,
    )]));

    // Two rows per slot: the bytes, then the address they make.
    let slots = rows.saturating_sub(1).div_ceil(2);
    for slot in 0..slots {
        let address = sp.wrapping_add((slot * 4) as u16);
        let bytes = emu.peek_quiet_block(address as u32, 4);
        let hex: String = bytes.iter().map(|byte| format!("{byte:02X} ")).collect();
        out.push(Row::spans(vec![
            Span::new(format!("{address:04X}h  "), theme::TEXT_DIM),
            Span::new(hex, theme::TEXT),
        ]));

        if out.len() >= rows {
            break;
        }
        // The little-endian word, labelled only when it is a plausible code
        // pointer: the address must land in a ROM window and decode to an
        // instruction the table knows.  Without both checks a zeroed slot would
        // be labelled with whatever the empty RAM happens to decode to, which
        // reads as noise rather than as information.
        let value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let target = value & 0x00FF_FFFF;
        let label = code_slot_label(debugger, emu, target).unwrap_or_default();
        out.push(Row::spans(vec![
            Span::new("      ", theme::TEXT_DIM),
            Span::new(format!("{value:08X}h  "), theme::TEXT_DIM),
            Span::new(label, theme::CHANGED),
        ]));
    }
    Content::new(out)
}

/// What a stack slot points at, when it points at code.
///
/// `None` for anything that is not a ROM address holding a recognised
/// instruction: most of a stack is data, and labelling data is worse than
/// labelling nothing.
fn code_slot_label(debugger: &mut Debugger, emu: &mut Emu, target: u32) -> Option<String> {
    let fx991_bus::Region::Rom { .. } = emu.chipset.bus.region(target) else {
        return None;
    };
    let insn = debugger
        .disassemble_insns(emu, target, 1)
        .into_iter()
        .next()?;
    if insn.is_unknown() {
        return None;
    }
    Some(insn.text)
}

/// Breakpoints and watches, with their conditions.
pub fn breakpoints(debugger: &Debugger) -> Content {
    let mut out = Vec::new();

    let mut any = false;
    for (id, bp) in debugger.breakpoints() {
        any = true;
        out.push(breakpoint_row(id.0, bp));
    }
    if !any {
        out.push(Row::plain("(no breakpoints)", theme::TEXT_DIM));
    }

    out.push(Row::plain("", theme::TEXT));
    let mut any = false;
    for (id, watch) in debugger.watches() {
        any = true;
        let condition = match &watch.condition {
            Some(condition) => format!(" if {condition}"),
            None => String::new(),
        };
        out.push(Row::spans(vec![
            Span::new(format!("W{:<3} ", id.0), theme::TEXT_DIM),
            Span::new(
                format!("{:05X}h {}", watch.address, watch.kind.label()),
                theme::TEXT,
            ),
            Span::new(format!("  {} hits{condition}", watch.hits), theme::TEXT_DIM),
        ]));
    }
    if !any {
        out.push(Row::plain("(no watches)", theme::TEXT_DIM));
    }

    Content::new(out)
}

/// One breakpoint's row.
pub fn breakpoint_row(id: u64, bp: &Breakpoint) -> Row {
    let mut spans = vec![
        Span::new(format!("B{:<3} ", id), theme::TEXT_DIM),
        Span::new(
            format!("{} ", if bp.enabled { "on " } else { "off" }),
            if bp.enabled {
                theme::BREAKPOINT
            } else {
                theme::TEXT_DIM
            },
        ),
        Span::new(format!("{:>9}  ", bp.address_text()), theme::TEXT),
        Span::new(format!("{} hits", bp.hits), theme::TEXT_DIM),
    ];
    if bp.once {
        spans.push(Span::new("  once", theme::WARN));
    }
    if bp.log_only {
        spans.push(Span::new("  log-only", theme::TEXT_DIM));
    }
    if let Some(condition) = &bp.condition {
        spans.push(Span::new(format!("  if {condition}"), theme::CHANGED));
    }
    Row::spans(spans).with_marker(Marker::Breakpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fx991_dbg::WatchKind;

    /// A machine with a small program, without needing a ROM file.
    fn emu() -> Emu {
        let mut rom = vec![0u8; 0x4_0000];
        rom[0x0000] = 0x00;
        rom[0x0001] = 0xF0; // SP = 0xF000
        rom[0x0002] = 0x00;
        rom[0x0003] = 0x90; // reset entry 0x9000
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

    #[test]
    fn the_disassembly_marks_the_current_instruction() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        let content = disassembly(&mut debugger, &mut emu, 5, 0);
        assert_eq!(content.rows.len(), 5);
        let focus = content.focus.expect("the PC is in the window");
        assert_eq!(
            content.rows[focus].marker,
            Marker::Current,
            "the row at the PC is marked"
        );
        assert!(
            content.rows[focus].text().contains("0010000h"),
            "{}",
            content.rows[focus].text()
        );
    }

    #[test]
    fn the_disassembly_marks_an_armed_breakpoint() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        debugger.add_breakpoint_at(0x1_0002);

        let content = disassembly(&mut debugger, &mut emu, 6, 0);
        let marked: Vec<&Row> = content
            .rows
            .iter()
            .filter(|row| row.marker == Marker::Breakpoint)
            .collect();
        assert_eq!(marked.len(), 1, "exactly one row is a breakpoint");
        assert!(
            marked[0].text().contains("0010002h"),
            "{}",
            marked[0].text()
        );
    }

    #[test]
    fn a_disabled_breakpoint_is_not_marked() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        let id = debugger.add_breakpoint_at(0x1_0002);
        debugger.breakpoint_mut(id).unwrap().enabled = false;

        let content = disassembly(&mut debugger, &mut emu, 6, 0);
        assert!(
            !content
                .rows
                .iter()
                .any(|row| row.marker == Marker::Breakpoint),
            "a disabled breakpoint does not stop, so it is not marked"
        );
    }

    #[test]
    fn the_memory_dump_is_sixteen_bytes_to_a_row_and_shows_ascii() {
        let mut emu = emu();
        emu.poke(0x0_D180, b'A');
        emu.poke(0x0_D181, 0x00);
        emu.poke(0x0_D182, b'Z');

        let content = memory(&mut emu, 0x0_D180, 2);
        assert_eq!(content.rows.len(), 2);
        let first = content.rows[0].text();
        assert!(first.contains("D180h"), "{first}");
        assert!(first.contains("41 00 5A"), "the hex: {first}");
        assert!(first.contains("A.Z"), "the ASCII column: {first}");
        // The second row starts 16 bytes on.
        assert!(content.rows[1].text().contains("D190h"));
    }

    #[test]
    fn the_memory_dump_wraps_at_the_top_of_the_address_space() {
        // A 24-bit space, so a dump starting near the top wraps rather than
        // running off the end.
        let mut emu = emu();
        let content = memory(&mut emu, 0x00FF_FFF0, 2);
        assert!(content.rows[0].text().contains("0FFFFF0h"));
        assert!(content.rows[1].text().contains("0000000h"));
    }

    #[test]
    fn the_register_panel_shows_every_register() {
        let mut emu = emu();
        emu.set_reg(3, 0xAB);
        emu.set_reg(15, 0xCD);
        let content = registers(&emu);
        let all: String = content
            .rows
            .iter()
            .map(|row| row.text())
            .collect::<Vec<_>>()
            .join("\n");

        for name in ["PC", "SP", "PSW", "CSR", "LR", "DSR"] {
            assert!(all.contains(name), "{name} is missing from:\n{all}");
        }
        assert!(all.contains("AB"), "R3's value:\n{all}");
        assert!(all.contains("CD"), "R15's value:\n{all}");
        // Every byte register is listed.
        for index in 0..16 {
            assert!(all.contains(&format!("R{index}")), "R{index} is missing");
        }
    }

    #[test]
    fn the_psw_is_shown_as_flag_letters() {
        use nxu8_core::{PSW_C, PSW_MIE, PSW_Z};
        assert_eq!(psw_flags(0), "-");
        assert_eq!(psw_flags(PSW_Z), "Z");
        assert_eq!(psw_flags(PSW_C | PSW_Z), "CZ");
        assert_eq!(psw_flags(PSW_C | PSW_Z | PSW_MIE), "CZI");
    }

    #[test]
    fn the_stack_shows_the_pointer_and_labels_code_addresses() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        // A return address to the program, so the label has something to find.
        emu.chipset.cpu.regs.sp = 0xD570;
        emu.poke(0x0_D570, 0x00);
        emu.poke(0x0_D571, 0x00);
        emu.poke(0x0_D572, 0x01);
        emu.poke(0x0_D573, 0x00);

        let content = stack(&mut debugger, &mut emu, 5);
        assert!(!content.is_empty());
        assert!(
            content.rows[0].text().contains("D570h"),
            "{}",
            content.rows[0].text()
        );
        let all: String = content
            .rows
            .iter()
            .map(|row| row.text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("00010000h"), "the slot value:\n{all}");
    }

    #[test]
    fn the_breakpoint_panel_lists_breakpoints_and_watches() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        debugger.add_breakpoint_at(0x1_0002);
        debugger.add_watch(
            &mut emu,
            fx991_dbg::Watch::new(0x0_D180, WatchKind::ReadWrite),
        );

        let content = breakpoints(&debugger);
        let all: String = content
            .rows
            .iter()
            .map(|row| row.text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("10002h"), "the breakpoint:\n{all}");
        assert!(all.contains("D180h"), "the watch:\n{all}");
        assert!(all.contains("rw"), "the watch direction:\n{all}");
    }

    #[test]
    fn the_breakpoint_panel_says_when_there_is_nothing() {
        let debugger = Debugger::new();
        let content = breakpoints(&debugger);
        let all: String = content
            .rows
            .iter()
            .map(|row| row.text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("(no breakpoints)"), "{all}");
        assert!(all.contains("(no watches)"), "{all}");
    }

    #[test]
    fn a_breakpoint_row_shows_its_condition_as_text() {
        let mut debugger = Debugger::new();
        let id = debugger
            .add_conditional_breakpoint(0x1_0002, "r0 == 1")
            .expect("the condition parses");
        let bp = debugger.breakpoint(id).unwrap();
        let text = breakpoint_row(id.0, bp).text();
        assert!(text.contains("if r0 == 1"), "{text}");
    }

    #[test]
    fn a_row_pads_to_the_panel_width() {
        let row = Row::plain("abc", theme::TEXT);
        assert_eq!(row.text_padded(6), "abc   ");
        // A row already at or past the width is left alone.
        assert_eq!(row.text_padded(3), "abc");
        assert_eq!(row.text_padded(1), "abc");
    }

    #[test]
    fn the_toolbar_reports_the_stop_reason() {
        let mut emu = emu();
        let row = toolbar(&mut emu, None, false);
        // Physical addresses are seven digits, matching the debugger's own view.
        assert!(row.text().contains("PC 0010000h"), "{}", row.text());

        let reason = StopReason::Breakpoint {
            id: fx991_dbg::BreakpointId(3),
            address: 0x1_0002,
        };
        let row = toolbar(&mut emu, Some(&reason), false);
        assert!(row.text().contains("breakpoint 3"), "{}", row.text());

        let row = toolbar(&mut emu, None, true);
        assert!(row.text().contains("TRACE"), "{}", row.text());
    }

    #[test]
    fn a_zero_row_panel_asks_for_nothing() {
        // A panel squeezed to nothing must not ask for a window of instructions.
        let mut emu = emu();
        let mut debugger = Debugger::new();
        assert!(disassembly(&mut debugger, &mut emu, 0, 0).is_empty());
        assert!(memory(&mut emu, 0, 0).is_empty());
        assert!(stack(&mut debugger, &mut emu, 0).is_empty());
    }
}
