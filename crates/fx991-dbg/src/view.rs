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

//! The panels a person looks at: disassembly, registers, memory, stack,
//! interrupts, peripherals, and the screen.
//!
//! Everything here *returns strings*.  Nothing prints, and nothing holds a
//! reference to the machine past the call, so the same functions serve the
//! terminal REPL, a script's `echo`, and a future TUI or window without change.
//! That is the whole reason this module is separate from [`crate::session`].
//!
//! # Reading memory the debugger's way
//!
//! Every panel reads through the side-effect-free accessors
//! ([`fx991::Emu::peek_quiet`], [`fx991::Emu::read_code_quiet`]).  An ordinary read
//! would add to the bus's fault counters and trip watchpoints, so looking at memory
//! would change the machine's observable history -- and the fault log is something
//! this project's tests assert on.
//!
//! # Conventions
//!
//! Addresses print as `{:07X}h` (physical) or `{:05X}h` (a bare PC), matching how
//! the plans write them and how [`crate::breakpoint::Breakpoint::address_text`]
//! prints them, so a user can copy an address from one panel into a command.
//! Register values are uppercase hex with an explicit width.

use fx991::Emu;

use crate::session::Debugger;

/// The width of the mnemonic column in the disassembly view.
const TEXT_WIDTH: usize = 8;

/// Render `count` instructions from `address`, with the PC marked.
///
/// The current instruction is marked `>` so the view can be read at a glance; a
/// `DSR<-` prefix and the instruction it applies to are shown as two lines with the
/// second indented, because that is what the bytes say and what the CPU treats as
/// one step.
pub fn disassembly(debugger: &mut Debugger, emu: &mut Emu, address: u32, count: usize) -> String {
    let pc = emu.pc();
    let text = debugger.disassemble(emu, address, count);
    let mut out = String::new();
    for line in text.lines() {
        let line_address = u32::from_str_radix(&line[..6], 16).unwrap_or(0);
        let marker = if line_address == pc { '>' } else { ' ' };
        out.push(marker);
        out.push_str(&line[1..]);
        out.push('\n');
    }
    out
}

/// The disassembly around the current PC.
pub fn disassembly_at_pc(
    debugger: &mut Debugger,
    emu: &mut Emu,
    before: usize,
    after: usize,
) -> String {
    let pc = emu.pc();
    // Walking backwards has to test both possible lengths: the previous
    // instruction is 2 or 4 bytes, so the one that ends exactly at `start` is the
    // right answer and simply stepping back by 4 would land mid-instruction.
    let mut start = pc;
    for _ in 0..before {
        let mut found = None;
        for back in [2u32, 4] {
            let candidate = start.saturating_sub(back) & !1;
            if candidate == start {
                continue;
            }
            if candidate + instruction_length(emu, candidate) as u32 == start {
                found = Some(candidate);
                break;
            }
        }
        match found {
            Some(candidate) => start = candidate,
            // The beginning of the space: nothing further back to show.
            None => break,
        }
    }
    disassembly(debugger, emu, start, before + 1 + after)
}

fn instruction_length(emu: &mut Emu, address: u32) -> usize {
    let mut read = |at: u32| emu.chipset.bus.read_code_quiet(at);
    nxu8_asm::disasm::step_length(&mut read, address)
}

/// The register file, with the PSW bits decoded and the exception banks listed.
pub fn registers(emu: &Emu) -> String {
    let regs = &emu.chipset.cpu.regs;
    let mut out = String::new();

    out.push_str(&format!(
        "PC  {:07X}h   CSR {:X}   SP  {:04X}h   EA  {:04X}h\n",
        regs.physical_pc(),
        regs.csr,
        regs.sp,
        regs.ea
    ));
    out.push_str(&format!(
        "PSW {:02X}h  {}   LR  {:04X}h   LCSR {:X}   DSR {:02X}h\n",
        regs.psw(),
        psw_text(regs.psw()),
        regs.lr(),
        regs.lcsr(),
        regs.dsr
    ));

    // The sixteen byte registers, four to a row, with their ER pairing shown.
    for row in 0..4 {
        out.push_str("  ");
        for column in 0..4 {
            let index = row * 4 + column;
            out.push_str(&format!("R{index:<2}={:02X} ", regs.r[index]));
        }
        out.push_str("   ");
        for column in 0..2 {
            let index = row * 4 + column * 2;
            out.push_str(&format!("ER{index:<3}={:04X} ", regs.er(index)));
        }
        out.push('\n');
    }

    // The wider aliases, which are not separate storage but are what the code
    // actually names.
    out.push_str(&format!(
        "  XR0={:08X}  XR4={:08X}  XR8={:08X}  XR12={:08X}\n",
        regs.xr(0),
        regs.xr(4),
        regs.xr(8),
        regs.xr(12)
    ));
    out.push_str(&format!(
        "  QR0={:016X}  QR8={:016X}\n",
        regs.reg(0, 8),
        regs.reg(8, 8)
    ));

    // The exception banks: an interrupt landing here is the thing a bad-looking PC
    // usually turns out to be.
    out.push_str("  level  ELR     ECSR  EPSW\n");
    for level in 0..4 {
        out.push_str(&format!(
            "  {level}      {:04X}h   {:<4}  {:02X}h{}\n",
            regs.elr[level],
            regs.ecsr[level],
            regs.epsw[level],
            if level == regs.elevel() {
                "  <- active"
            } else {
                ""
            }
        ));
    }
    out
}

/// The PSW bits as a short list, e.g. `[Z C]`.
pub fn psw_text(psw: u8) -> String {
    use nxu8_core::{PSW_C, PSW_HC, PSW_MIE, PSW_OV, PSW_S, PSW_Z};
    let mut flags = Vec::new();
    if psw & PSW_Z != 0 {
        flags.push("Z");
    }
    if psw & PSW_C != 0 {
        flags.push("C");
    }
    if psw & PSW_S != 0 {
        flags.push("S");
    }
    if psw & PSW_OV != 0 {
        flags.push("OV");
    }
    if psw & PSW_HC != 0 {
        flags.push("HC");
    }
    if psw & PSW_MIE != 0 {
        flags.push("MIE");
    }
    if flags.is_empty() {
        return "[-]".to_string();
    }
    format!("[{}]", flags.join(" "))
}

/// A hex dump with an ASCII column, the shape `emu dump` already uses.
pub fn memory(emu: &mut Emu, address: u32, length: usize) -> String {
    let bytes = emu.peek_quiet_block(address, length);
    let mut out = String::new();
    for (row, chunk) in bytes.chunks(16).enumerate() {
        let row_address = address + (row * 16) as u32;
        out.push_str(&format!("{row_address:05X}h  "));
        for index in 0..16 {
            match chunk.get(index) {
                Some(byte) => out.push_str(&format!("{byte:02X} ")),
                None => out.push_str("   "),
            }
            if index == 7 {
                out.push(' ');
            }
        }
        out.push_str(" |");
        for byte in chunk {
            out.push(if (0x20..0x7F).contains(byte) {
                *byte as char
            } else {
                '.'
            });
        }
        out.push_str("|\n");
    }
    out
}

/// The stack, from `SP` upwards, with code-looking slots labelled.
///
/// The labelling is the point: in this project the stack is where a ROP chain
/// lives, so a word that points at executable ROM is worth naming.  It uses the
/// memory disassembler, so a patched address shows the patched text.
pub fn stack(emu: &mut Emu, length: usize) -> String {
    let sp = emu.chipset.cpu.regs.sp;
    let mut out = format!("SP = {sp:04X}h\n");
    let bytes = emu.peek_quiet_block(sp as u32, length);
    for (row, chunk) in bytes.chunks(16).enumerate() {
        let row_address = sp as u32 + (row * 16) as u32;
        out.push_str(&format!("{row_address:05X}h  "));
        for byte in chunk {
            out.push_str(&format!("{byte:02X} "));
        }
        out.push('\n');

        // A 4-byte slot is what `POP PC` consumes, so those are the candidates.
        for offset in (0..chunk.len()).step_by(4) {
            if offset + 4 > chunk.len() {
                break;
            }
            let slot = u32::from_le_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
            let address = row_address + offset as u32;
            if let Some(text) = code_slot_text(emu, slot) {
                out.push_str(&format!("        {address:05X}h: {slot:07X}h  {text}\n"));
            }
        }
    }
    out
}

/// The disassembly of a pointer's target, if it looks like executable ROM.
///
/// `None` for anything outside the ROM windows, so a stack full of BCD variables
/// does not produce a page of false positives.
fn code_slot_text(emu: &mut Emu, target: u32) -> Option<String> {
    let region = emu.chipset.bus.region(target);
    let fx991_bus::Region::Rom { .. } = region else {
        return None;
    };
    let mut read = |at: u32| emu.chipset.bus.read_code_quiet(at);
    let insn = nxu8_asm::disasm::disassemble_one(&mut read, target);
    // An unrecognized word is much more likely to be data that happens to sit in
    // a ROM window than an instruction, so it is not reported.
    if insn.is_unknown() {
        return None;
    }
    let _ = TEXT_WIDTH;
    Some(insn.text)
}

/// The interrupt controller, its pending sources, and the run mode.
pub fn interrupts(emu: &Emu) -> String {
    let interrupts = emu.chipset.interrupts.borrow();
    let mut out = format!(
        "run mode    {:?}\nIMR  {:04X}h   IRR {:04X}h   pending {}\n",
        interrupts.run_mode, interrupts.mask, interrupts.pending, interrupts.pending_count
    );

    // Only the managed sources are named; the rest of the 13-bit field is not
    // documented anywhere this project has found.
    let names = [
        "checkflag",
        "reset",
        "break",
        "emulator",
        "nonmaskable",
        "maskable",
        "source6",
        "source7",
        "keyboard",
        "timer",
        "source10",
        "source11",
        "source12",
    ];
    out.push_str("  index name         enabled pending active\n");
    for (index, name) in names.iter().enumerate() {
        if !emu.chipset.interrupt_enabled(index) && !emu.chipset.interrupt_pending(index) {
            continue;
        }
        out.push_str(&format!(
            "  {index:<5} {name:<12} {:<7} {:<7} {}\n",
            yes_no(interrupts.enabled(index)),
            yes_no(interrupts.is_pending(index)),
            yes_no(interrupts.active.get(index).copied().unwrap_or(false))
        ));
    }
    out
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

/// The peripherals a debugger usually wants: screen control, timer, keyboard.
pub fn peripherals(emu: &mut Emu) -> String {
    let mut out = String::new();

    let screen = emu.chipset.screen.borrow();
    out.push_str(&format!(
        "screen    mode {} contrast {} range {} lit {} px\n",
        screen.mode,
        screen.contrast,
        screen.range,
        screen.lit_pixels()
    ));
    drop(screen);

    let timer = emu.chipset.timer.borrow();
    out.push_str(&format!(
        "timer     interval {:04X}h counter {:04X}h control {} raise {}\n",
        timer.data_interval,
        timer.data_counter,
        timer.data_control & 0x01,
        yes_no(timer.raise_required)
    ));
    out.push_str(&format!(
        "          divider {}/{} instructions\n",
        timer.instructions_since_divide(),
        timer.ticks_per_divide()
    ));
    drop(timer);

    let keyboard = emu.chipset.keyboard.borrow();
    out.push_str(&format!(
        "keyboard  KI {:02X}h  KO {:03X}h  KOMask {:03X}h  filter {:02X}h\n",
        keyboard.keyboard_in,
        keyboard.keyboard_out & 0x3FF,
        keyboard.keyboard_out_mask & 0x3FF,
        keyboard.input_filter
    ));
    let pressed = keyboard.pressed_buttons();
    drop(keyboard);
    if !pressed.is_empty() {
        let names: Vec<String> = pressed
            .iter()
            .map(|(code, stuck)| format!("{code:02X}h{}", if *stuck { "(latched)" } else { "" }))
            .collect();
        out.push_str(&format!("          held {}\n", names.join(" ")));
    }

    let fault_count = emu.chipset.bus.fault_count;
    out.push_str(&format!(
        "faults    {fault_count} ({})\n",
        emu.chipset
            .bus
            .fault_kinds()
            .iter()
            .take(4)
            .map(|fault| format!("{} {:05X}h", fault.kind.label(), fault.address))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out
}

/// The LCD as `#` and `.`, which is what `Screen::render_ascii` already produces.
pub fn screen(emu: &Emu) -> String {
    let screen = emu.chipset.screen.borrow();
    if !matches!(screen.mode, 4..=6) {
        // The panel draws nothing outside those modes, which is a real state and
        // worth saying rather than showing a blank screen as if it were content.
        return format!(
            "mode {} -- the panel draws nothing outside modes 4-6\n",
            screen.mode
        );
    }
    let mut out = screen.render_ascii().join("\n");
    out.push('\n');
    out
}

/// The breakpoints, watches, patches, snapshots, and trace that are set.
pub fn state(debugger: &Debugger, emu: &Emu) -> String {
    let mut out = String::new();

    out.push_str("breakpoints:\n");
    let mut any = false;
    for (id, breakpoint) in debugger.breakpoints() {
        any = true;
        let mut flags = String::new();
        if breakpoint.log_only {
            flags.push_str(" log-only");
        }
        if breakpoint.once {
            flags.push_str(" once");
        }
        if let Some(condition) = &breakpoint.condition {
            flags.push_str(&format!(" if {condition:?}"));
        }
        out.push_str(&format!(
            "  {} {} {} hits {} stops {}{flags}\n",
            id.0,
            if breakpoint.enabled { "on " } else { "off" },
            breakpoint.address_text(),
            breakpoint.hits,
            breakpoint.stops
        ));
    }
    if !any {
        out.push_str("  (none)\n");
    }

    out.push_str("watches:\n");
    any = false;
    for (id, watch) in debugger.watches() {
        any = true;
        let condition = match &watch.condition {
            Some(condition) => format!(" if {condition:?}"),
            None => String::new(),
        };
        out.push_str(&format!(
            "  {} {} {:05X}h {} hits {} stops {}{condition}\n",
            id.0,
            if watch.enabled { "on " } else { "off" },
            watch.address,
            watch.kind.label(),
            watch.hits,
            watch.stops
        ));
    }
    if !any {
        out.push_str("  (none)\n");
    }

    // What is *planted* comes from the bus, so a snapshot restore is reflected;
    // the source text comes from the record, so it is shown when available.
    out.push_str("patches:\n");
    let planted = debugger.planted(emu);
    if planted.is_empty() {
        out.push_str("  (none)\n");
    }
    for patch in planted.iter() {
        let source = debugger
            .patches()
            .iter()
            .find(|record| record.address == patch.address)
            .map(|record| record.source.clone())
            .unwrap_or_default();
        out.push_str(&format!(
            "  {:07X}h  {}  {source}\n",
            patch.address,
            patch.byte_text()
        ));
    }

    out.push_str("snapshots:\n");
    let names = debugger.snapshot_names();
    if names.is_empty() {
        out.push_str("  (none)\n");
    } else {
        out.push_str(&format!("  {}\n", names.join(", ")));
    }

    match debugger.trace() {
        Some(trace) => out.push_str(&format!(
            "trace: {} of {} entries\n",
            trace.len(),
            trace.capacity()
        )),
        None => out.push_str("trace: off\n"),
    }
    out
}

/// The most recent traced instructions, oldest first.
pub fn trace_tail(debugger: &Debugger, count: usize) -> String {
    let Some(trace) = debugger.trace() else {
        return "no trace is running\n".to_string();
    };
    let mut out = String::from("tick      pc        handler      sp     psw   changed\n");
    for entry in trace.tail(count) {
        out.push_str(&format!(
            "{:<9} {:07X}h  {:<12} {:04X}h  {:02X}h   {}\n",
            entry.tick,
            entry.pc,
            entry.handler_text(),
            entry.sp,
            entry.psw,
            entry.changed_text()
        ));
    }
    out
}

/// A one-line summary of where the machine is, for a prompt or a script.
pub fn summary(emu: &mut Emu) -> String {
    let stopped = if emu.chipset.run_mode() == fx991_chipset::RunMode::Run {
        String::new()
    } else {
        format!("  [{:?}]", emu.chipset.run_mode())
    };
    let (ticks, pc, sp, psw) = (emu.ticks, emu.pc(), emu.sp(), emu.psw());
    let mut read = |at: u32| emu.chipset.bus.read_code_quiet(at);
    let insn = nxu8_asm::disasm::disassemble_one(&mut read, pc);
    format!(
        "tick {ticks}  PC {pc:07X}h  SP {sp:04X}h  PSW {psw:02X}h{stopped}   {}\n",
        insn.text
    )
}

/// The delta between two states, as a report.
pub fn diff(diff: &crate::snapshot::Diff) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "PC  {:07X}h -> {:07X}h\nSP  {:04X}h -> {:04X}h\nPSW {:02X}h -> {:02X}h\nticks {} -> {}\n",
        diff.pc.0,
        diff.pc.1,
        diff.sp.0,
        diff.sp.1,
        diff.psw.0,
        diff.psw.1,
        diff.ticks.0,
        diff.ticks.1
    ));

    if !diff.registers.is_empty() {
        out.push_str("registers:\n");
        for (name, before, after) in &diff.registers {
            out.push_str(&format!("  {name:<4} {before:04X}h -> {after:04X}h\n"));
        }
    }
    if !diff.ram.is_empty() {
        out.push_str(&format!("RAM: {} bytes differ\n", diff.ram.len()));
        for (address, before, after) in diff.ram.iter().take(32) {
            out.push_str(&format!(
                "  {address:05X}h  {before:02X}h -> {after:02X}h\n"
            ));
        }
        if diff.ram.len() > 32 {
            out.push_str(&format!("  ... {} more\n", diff.ram.len() - 32));
        }
    }
    if diff.screen_bytes > 0 {
        out.push_str(&format!(
            "screen: {} buffer bytes differ\n",
            diff.screen_bytes
        ));
    }
    if !diff.peripherals.is_empty() {
        out.push_str(&format!("peripherals: {}\n", diff.peripherals.join(", ")));
    }
    if out.lines().count() <= 4 && diff.is_empty() {
        out.push_str("(no differences)\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::breakpoint::{Breakpoint, Watch, WatchKind};
    use crate::condition::Expr;

    /// A machine whose program is a handful of real instructions, so the panels
    /// have something to show without needing the ROM.
    fn emu() -> Emu {
        use nxu8_asm::asm;
        let mut rom = vec![0u8; 0x4_0000];
        rom[0x0000] = 0x00;
        rom[0x0001] = 0xF0; // SP = 0xF000
        let mut program = Vec::new();
        program.extend_from_slice(&asm::mov_r_imm(0, 0x41)); // 0x10000
        program.extend_from_slice(&asm::mov_r_imm(1, 0x42)); // 0x10002
        program.extend_from_slice(&asm::pop_pc()); // 0x10004
        rom[0x1_0000..0x1_0000 + program.len()].copy_from_slice(&program);
        let mut emu = Emu::from_rom(rom);
        {
            let mut interrupts = emu.chipset.interrupts.borrow_mut();
            interrupts.active = [false; fx991_chipset::INT_COUNT];
            interrupts.pending_count = 0;
        }
        emu.chipset.cpu.regs.csr = 1;
        emu.chipset.cpu.regs.pc = 0;
        emu
    }

    #[test]
    fn the_disassembly_marks_the_current_instruction() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        let text = disassembly(&mut debugger, &mut emu, 0x1_0000, 3);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with('>'), "PC is here: {:?}", lines[0]);
        assert!(lines[1].starts_with(' '), "and not here: {:?}", lines[1]);
        assert!(lines[0].contains("MOV"), "{}", lines[0]);
    }

    #[test]
    fn disassembling_around_the_pc_stays_on_instruction_boundaries() {
        // Instructions are 2 or 4 bytes, so walking back by a byte offset would land
        // in the middle of one.
        let mut emu = emu();
        let mut debugger = Debugger::new();
        emu.chipset.cpu.regs.pc = 4; // the POP PC
        let text = disassembly_at_pc(&mut debugger, &mut emu, 2, 0);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        for line in &lines {
            assert!(
                line.contains("MOV") || line.contains("POP") || line.contains("Unrecognized"),
                "every line is a whole instruction: {line:?}"
            );
        }
        assert!(lines[2].starts_with('>'), "the PC line is last: {text}");
    }

    #[test]
    fn the_register_view_decodes_the_psw_and_the_banks() {
        let mut emu = emu();
        emu.set_reg(0, 0xEF);
        emu.set_reg(1, 0xBE);
        emu.chipset.cpu.regs.sp = 0xF000;
        let text = registers(&emu);

        assert!(text.contains("R0 =") || text.contains("R0="), "{text}");
        assert!(text.contains("EF"), "{text}");
        assert!(text.contains("BEEF"), "the ER pairing is shown: {text}");
        assert!(
            text.contains("level"),
            "the exception banks are listed: {text}"
        );
        assert!(text.contains("<- active"), "the active level is marked");
    }

    #[test]
    fn the_psw_text_names_the_set_bits_only() {
        assert_eq!(psw_text(0), "[-]");
        assert_eq!(psw_text(nxu8_core::PSW_Z), "[Z]");
        assert_eq!(psw_text(nxu8_core::PSW_Z | nxu8_core::PSW_C), "[Z C]");
        assert!(psw_text(nxu8_core::PSW_MIE).contains("MIE"));
    }

    #[test]
    fn memory_is_dumped_with_an_ascii_column() {
        let mut emu = emu();
        emu.poke(0x0_D180, 0x41); // 'A'
        emu.poke(0x0_D181, 0x00);
        let text = memory(&mut emu, 0x0_D180, 16);
        assert!(text.contains("D180h"), "{text}");
        assert!(text.contains("41 00"), "{text}");
        assert!(text.contains("|A."), "the ASCII column: {text}");
    }

    #[test]
    fn a_dump_walks_past_the_end_without_panicking() {
        let mut emu = emu();
        let text = memory(&mut emu, 0x0_FFFF, 32);
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn looking_at_memory_does_not_disturb_the_machine() {
        // The panels read through the quiet accessors: a debugger's own inspection
        // must not appear in the fault log.
        let mut emu = emu();
        let before = emu.chipset.bus.fault_count;
        memory(&mut emu, 0x6_0000, 64); // unmapped
        stack(&mut emu, 64);
        let mut debugger = Debugger::new();
        disassembly(&mut debugger, &mut emu, 0x1_0000, 8);
        assert_eq!(emu.chipset.bus.fault_count, before, "no faults were added");
    }

    #[test]
    fn the_stack_view_labels_a_slot_that_points_at_code() {
        // A ROP chain is a stack full of code addresses, so naming them is the
        // point of this panel.
        let mut emu = emu();
        emu.chipset.cpu.regs.sp = 0x0_E000;
        // A little-endian pointer to `0x10000`, where `MOV R0,#0x41` sits.
        emu.poke(0x0_E000, 0x00);
        emu.poke(0x0_E001, 0x00);
        emu.poke(0x0_E002, 0x01);
        emu.poke(0x0_E003, 0x00);
        let text = stack(&mut emu, 16);
        assert!(text.contains("SP = E000h"), "{text}");
        assert!(
            text.contains("E000h: 0010000h"),
            "the pointer is picked out: {text}"
        );
        assert!(text.contains("MOV"), "and its target disassembled: {text}");
    }

    #[test]
    fn a_stack_slot_holding_data_is_not_labelled_as_code() {
        let mut emu = emu();
        emu.chipset.cpu.regs.sp = 0x0_E000;
        // A small number, as a BCD variable would be.
        for offset in 0..4u32 {
            emu.poke(0x0_E000 + offset, [0x00, 0x00, 0x00, 0x00][offset as usize]);
        }
        let text = stack(&mut emu, 16);
        assert!(!text.contains("MOV"), "0 is not a code pointer: {text}");
    }

    #[test]
    fn the_interrupt_view_lists_the_run_mode_and_the_named_sources() {
        let emu = emu();
        // Enable the keyboard source (index 5, the first maskable bit).
        let bit = 5 - fx991_chipset::INT_MASKABLE;
        emu.chipset.interrupts.borrow_mut().mask = 1 << bit;
        let text = interrupts(&emu);
        assert!(text.contains("run mode"), "{text}");
        assert!(text.contains("IMR"), "{text}");
    }

    #[test]
    fn the_interrupt_view_survives_every_source_being_set() {
        // A machine with everything pending must not panic or print nonsense; the
        // unnamed sources are labelled generically rather than skipped.
        let emu = emu();
        {
            let mut interrupts = emu.chipset.interrupts.borrow_mut();
            interrupts.mask = 0x1FFF;
            interrupts.pending = 0x1FFF;
            interrupts.active = [true; fx991_chipset::INT_COUNT];
        }
        let text = interrupts(&emu);
        assert!(text.contains("keyboard"), "{text}");
        assert!(text.contains("timer"), "{text}");
        assert!(
            text.contains("source12"),
            "the unnamed tail is still listed"
        );
    }

    #[test]
    fn the_peripheral_view_shows_the_divider_and_the_held_keys() {
        let mut emu = emu();
        let text = peripherals(&mut emu);
        assert!(text.contains("screen"), "{text}");
        assert!(
            text.contains("divider"),
            "the divider progress is shown: {text}"
        );
        assert!(text.contains("209"), "with its period: {text}");
        assert!(text.contains("keyboard"), "{text}");
        assert!(text.contains("faults"), "{text}");
    }

    #[test]
    fn the_peripheral_view_names_the_keys_that_are_held() {
        let mut emu = emu();
        emu.chipset.press_key(0x11); // '5'
        let text = peripherals(&mut emu);
        assert!(text.contains("held"), "{text}");
        assert!(text.contains("11h"), "{text}");
    }

    #[test]
    fn the_screen_view_says_so_when_the_panel_draws_nothing() {
        // Mode outside 4-6 really does draw nothing, and showing a blank screen
        // without saying why would look like a bug.
        let mut emu = emu();
        emu.poke(fx991_chipset::screen::CONTROL_MODE, 0);
        let text = screen(&emu);
        assert!(text.contains("draws nothing"), "{text}");

        emu.poke(fx991_chipset::screen::CONTROL_MODE, 5);
        // A visible column is the first byte of a row's 24-byte display area, which
        // starts at OFFSET inside the 32-byte row.
        emu.poke(
            fx991_chipset::screen::BUFFER_BASE + fx991_chipset::screen::OFFSET as u32,
            0xFF,
        );
        let text = screen(&emu);
        assert!(text.contains('#'), "the dot matrix is drawn: {text}");
    }

    #[test]
    fn the_state_view_lists_everything_that_is_set() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        debugger.add_breakpoint(
            Breakpoint::auto(0x1_0002).with_condition(Expr::parse("r0 == 1").unwrap()),
        );
        debugger.add_watch(&mut emu, Watch::new(0xD180, WatchKind::ReadWrite));
        debugger
            .apply_patch(&mut emu, 0x1_0004, &nxu8_asm::asm::pop_pc(), "POP PC")
            .unwrap();
        debugger.save_snapshot("here", &emu);

        let text = state(&debugger, &emu);
        assert!(text.contains("breakpoints:"), "{text}");
        assert!(text.contains("0010002h"), "the breakpoint address: {text}");
        assert!(!text.contains("log-only"), "{text}");
        assert!(text.contains("watches:"), "{text}");
        assert!(text.contains("D180h"), "{text}");
        assert!(text.contains("rw"), "{text}");
        assert!(text.contains("patches:"), "{text}");
        assert!(text.contains("POP PC"), "the patch source is shown: {text}");
        assert!(text.contains("snapshots:"), "{text}");
        assert!(text.contains("here"), "{text}");
        assert!(text.contains("trace: off"), "{text}");
    }

    #[test]
    fn the_state_view_says_when_nothing_is_set() {
        let emu = emu();
        let debugger = Debugger::new();
        let text = state(&debugger, &emu);
        assert!(text.contains("(none)"), "{text}");
    }

    #[test]
    fn the_trace_tail_shows_the_recent_instructions() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        debugger.start_trace(8);
        debugger.step(&mut emu);
        debugger.step(&mut emu);
        let text = trace_tail(&debugger, 5);
        assert!(text.contains("tick"), "{text}");
        assert!(text.contains("MOV"), "{text}");
        assert!(text.contains("0010002h"), "{text}");
        assert!(
            text.contains("R0=00>41") || text.contains("R1=00>42"),
            "{text}"
        );
    }

    #[test]
    fn the_trace_tail_says_so_when_no_trace_is_running() {
        let debugger = Debugger::new();
        assert!(trace_tail(&debugger, 5).contains("no trace"));
    }

    #[test]
    fn the_summary_line_shows_the_pc_and_the_instruction_there() {
        let mut emu = emu();
        let text = summary(&mut emu);
        assert!(text.contains("PC 0010000h"), "{text}");
        assert!(text.contains("MOV"), "{text}");
    }

    #[test]
    fn the_summary_flags_a_stopped_machine() {
        let mut emu = emu();
        emu.chipset.interrupts.borrow_mut().stop();
        let text = summary(&mut emu);
        assert!(text.contains("Stop"), "the run mode is shown: {text}");
    }

    #[test]
    fn a_diff_report_lists_the_registers_and_the_ram_that_moved() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        debugger.save_snapshot("before", &emu);
        emu.set_reg(0, 0x99);
        emu.poke(0x0_D180, 0x55);
        debugger.save_snapshot("after", &emu);

        let text = diff(&debugger.diff_snapshots("before", "after").unwrap());
        assert!(text.contains("R0"), "{text}");
        assert!(text.contains("D180h"), "{text}");
        assert!(text.contains("55"), "{text}");
    }

    #[test]
    fn an_empty_diff_says_so() {
        let emu = emu();
        let mut debugger = Debugger::new();
        debugger.save_snapshot("a", &emu);
        let text = diff(&debugger.diff_snapshots("a", "a").unwrap());
        assert!(text.contains("no differences"), "{text}");
    }
}
