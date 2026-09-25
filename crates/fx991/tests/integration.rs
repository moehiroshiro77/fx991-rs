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

//! Integration tests against the real ROM.
//!
//! Boot, instruction semantics, the bus map, the keyboard, and the end-to-end
//! "type an expression and read the answer" path.
//!
//! They need `data/rom_verF.bin`; a checkout without it skips them rather than
//! failing, so the crate-local unit tests still run.

use fx991::Calculator;
use fx991_chipset::{Chipset, RunMode, TickOutcome};
use fx991_model::address;

fn rom() -> Option<Vec<u8>> {
    testkit::rom_bytes().ok()
}

macro_rules! rom_or_skip {
    () => {
        match rom() {
            Some(bytes) => bytes,
            None => {
                eprintln!("skipping: data/rom_verF.bin is not present");
                return;
            }
        }
    };
}

// ---------------------------------------------------------------- M1: boot

#[test]
fn the_rom_image_is_the_one_the_plans_document() {
    let Some(bytes) = rom() else { return };
    assert_eq!(bytes.len(), testkit::ROM_LEN);
    assert_eq!(&bytes[0x3FFEE..0x3FFF5], b"CY-239F");
}

#[test]
fn the_reset_vector_matches_the_plans() {
    let Some(bytes) = rom() else { return };
    let word = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
    assert_eq!(word(0x0000), 0xF000, "stack pointer initial value");
    assert_eq!(word(0x0002), 0x946A, "reset entry");
    assert_eq!(word(0x0004), 0x946C, "second reset entry");
    assert_eq!(word(0x0080), 0x4A00, "SWI #0 vector");
}

#[test]
fn boot_lands_on_the_reset_entry_and_then_past_its_trampoline() {
    let mut machine = Chipset::new(fx991_bus::Bus::new(fx991_bus::Rom::new(rom_or_skip!())));
    machine.reset();

    let mut entry_seen = None;
    machine.tick(Some(&mut |pc, _bus| {
        entry_seen = Some(pc);
        false
    }));
    assert_eq!(entry_seen, Some(0x0_946A));
    // `0x946A` is `BC AL,0946Eh`, so one instruction past it is the init code.
    assert_eq!(machine.cpu.regs.pc, 0x946E);
}

#[test]
fn the_stack_starts_at_the_top_of_ram() {
    let mut machine = Chipset::new(fx991_bus::Bus::new(fx991_bus::Rom::new(rom_or_skip!())));
    machine.reset();
    assert_eq!(machine.cpu.regs.sp, 0xF000);
    // RAM ends at 0xEFFF, so the first push lands at 0xEFFE.
    machine.cpu.regs.sp = machine.cpu.regs.sp.wrapping_sub(2);
    let sp = machine.cpu.regs.sp as u32;
    machine.poke(sp, 0xAB);
    assert_eq!(machine.bus.ram()[0x1FFE], 0xAB);
}

// -------------------------------------------------------------- the bus map

#[test]
fn segment_zero_is_rom_then_ram_then_sfr() {
    let mut machine = Chipset::new(fx991_bus::Bus::new(fx991_bus::Rom::new(rom_or_skip!())));
    // ROM below 0xD000.
    assert_eq!(machine.peek(0x0_00946A), machine.bus.rom().data[0x00946A]);
    // RAM from 0xD000.
    machine.poke(address::INPUT_BUFFER, 0x41);
    assert_eq!(machine.peek(address::INPUT_BUFFER), 0x41);
    assert_eq!(
        machine.bus.ram()[(address::INPUT_BUFFER - 0xD000) as usize],
        0x41
    );
}

#[test]
fn the_fifth_segment_is_a_window_onto_the_bottom_of_the_rom() {
    let mut machine = Chipset::new(fx991_bus::Bus::new(fx991_bus::Rom::new(rom_or_skip!())));
    assert_eq!(machine.peek(0x5_0000), machine.bus.rom().data[0x0000]);
    assert_eq!(machine.peek(0x2_21B8), machine.bus.rom().data[0x2_21B8]);
}

#[test]
fn only_known_sfr_holes_are_touched_over_a_long_run() {
    // Two holes are expected:
    //
    // * `0xF050` (Keyboard/PdValue) is only populated on the non-hardware model,
    //   yet `rom:08D14` reads it;
    // * `0xF012` is outside the miscellaneous SFR block, and the interrupt fields
    //   there are 13 bits wide, so the high byte of a 16-bit write lands partly
    //   outside the mapped field.
    //
    // Anything else is a bus bug.
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    emu.run(200_000);
    for fault in emu.chipset.bus.fault_kinds() {
        let known = matches!(
            (fault.kind.label(), fault.address),
            ("sfr-read", 0xF050)
                | ("sfr-read", 0xF012)
                | ("sfr-write", 0xF012)
                | ("sfr-read", 0xF011)
                | ("sfr-write", 0xF011)
        );
        assert!(known, "unexpected bus fault {fault:?}");
    }
}

// -------------------------------------------------------------- end to end

#[test]
fn the_rom_reaches_its_key_wait_loop_on_its_own() {
    // This is the check that would have caught the timer-rate bug: with a
    // per-instruction timer the ROM wedges in STOP at tick 74 691 and never arms the
    // filter.  With the two-stage divider it wakes on the timer, cycles the main
    // loop a few times, and arms the filter.
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    assert!(calculator.emu.chipset.ready_for_key());
    assert!(
        calculator.emu.chipset.has_slept(),
        "the ROM should have parked at least once on the way here"
    );
}

#[test]
fn typing_an_expression_lands_the_tokens_in_the_input_area() {
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    calculator.press("1+2").expect("press");
    assert_eq!(calculator.input(), "1+2");
    assert_eq!(calculator.input_bytes(3), vec![0x31, 0xA6, 0x32]);
}

#[test]
fn arithmetic_answers_are_correct() {
    for (expression, expected) in [
        ("1+2", "3"),
        ("7-2", "5"),
        ("9+9", "18"),
        ("2*3", "6"),
        ("8/2", "4"),
        ("0.5", "0.5"),
        ("1+2*3", "7"),
        ("12+30", "42"),
    ] {
        let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
        let got = calculator.eval(expression).expect("eval");
        assert_eq!(got, expected, "{expression}");
    }
}

#[test]
fn ans_is_also_readable_by_variable_name() {
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    calculator.eval("1+2").expect("eval");
    assert_eq!(calculator.variable("Ans").as_deref(), Some("3"));
    assert_eq!(calculator.variable("M").as_deref(), Some("0"));
}

#[test]
fn the_display_lights_up_when_something_is_typed() {
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    assert_eq!(calculator.lit_pixels(), 0, "blank at boot");
    calculator.press("1").expect("press");
    assert!(calculator.lit_pixels() > 0, "a digit is on screen");
}

#[test]
fn the_display_renders_to_a_png() {
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    calculator.press("8").expect("press");
    let png = calculator.screen_png(2);
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(png.ends_with(b"IEND\xaeB`\x82"));
}

// ------------------------------------------------------------ scripting API

#[test]
fn run_until_reports_a_missed_address_instead_of_hanging() {
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    let err = emu.run_until(0x0_001234, 1_000).unwrap_err();
    assert_eq!(err.address, 0x0_001234);
    assert_eq!(err.budget, 1_000);
}

#[test]
fn run_until_reaches_an_address_that_is_already_current() {
    // PC sits at 0 before the reset request is dispatched, so this arrives with no
    // ticking at all.
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    assert_eq!(emu.pc(), 0);
    emu.run_until(0x0_0000, 100).expect("already there");
}

#[test]
fn run_until_reaches_a_later_address() {
    // One tick after the reset the machine is on the trampoline.
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    emu.run_until(0x0_946E, 10)
        .expect("the trampoline is reached");
}

#[test]
fn a_stopping_breakpoint_exposes_the_reset_vector() {
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    let index = emu.breakpoint(fx991::Breakpoint::auto(0x0_946A).stopping());
    // The vector is dispatched inside the first tick and, being a stopping
    // breakpoint, the instruction is suppressed so PC stays put.
    emu.step();
    assert_eq!(emu.pc(), 0x0_946A, "PC is held on the vector");
    assert_eq!(emu.hits(index), 1);

    // Without the breakpoint the trampoline runs and PC moves on.
    emu.clear_breakpoints();
    emu.step();
    assert_ne!(emu.pc(), 0x0_946A);
}

#[test]
fn an_observing_breakpoint_on_a_single_visit_address_fires_once() {
    // The main-loop entry at 0x24E40 is reached once and left.  An *observing*
    // breakpoint therefore fires exactly once, while a *stopping* one would hold PC
    // there and re-fire every tick -- which is the whole point of the two modes.
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    let index = emu.breakpoint(fx991::Breakpoint::auto(0x2_4E40));
    emu.run(200);
    assert_eq!(emu.hits(index), 1, "one visit, one hit");
    assert_ne!(emu.pc(), 0x2_4E40, "observation does not hold PC");
}

#[test]
fn an_observing_breakpoint_fires_once_per_arrival() {
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    let index = emu.breakpoint(fx991::Breakpoint::auto(address::ROM_IDLE));
    emu.run(1_000);
    assert!(emu.breakpoints()[index].hits >= 2, "the idle loop repeats");
}

#[test]
fn a_breakpoint_records_its_hits_up_to_the_limit() {
    // The idle loop is where the ROM repeats, so a breakpoint there fires on every
    // parked tick.  `hits` counts them all; `recorded` keeps only the first few, so a
    // long run cannot grow the record without bound.
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    let index = emu.breakpoint(fx991::Breakpoint::auto(address::ROM_IDLE).limit(4));
    emu.run_until(0x0_9216, 10_000).expect("idle reached");
    emu.run(50);
    assert!(emu.hits(index) > 4, "the parked PC repeats");
    let hits = emu.recorded_hits(index).to_vec();
    assert_eq!(hits.len(), 4, "the record limit is honoured");
    assert_eq!(hits[0].pc, 0x0_9216);
    assert!(hits[0].tick <= hits[3].tick, "records are in order");
}

#[test]
fn a_per_tick_hook_can_borrow_local_state() {
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    let mut sp_seen = Vec::new();
    emu.run_with(50, &mut |pc| {
        if pc == 0x0_946A {
            sp_seen.push(pc);
        }
        false
    });
    assert_eq!(sp_seen, vec![0x0_946A]);
}

#[test]
fn a_hook_returning_true_suppresses_the_instruction() {
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    let outcome = emu.step_with(&mut |pc| pc == 0x0_946A);
    assert_eq!(outcome, TickOutcome::Suppressed);
    assert_eq!(emu.pc(), 0x0_946A, "PC stayed on the vector");
}

#[test]
fn a_trace_records_one_line_per_step() {
    let mut trace = fx991::Trace::new(&["pc", "sp"]);
    trace.record(1, &["946A".to_string(), "F000".to_string()]);
    trace.record(2, &["946E".to_string(), "F000".to_string()]);
    let csv = trace.to_csv();
    assert_eq!(csv.lines().count(), 3);
    assert!(csv.starts_with("tick,pc,sp\n"));
}

#[test]
fn the_ram_can_be_snapshotted_and_restored() {
    // A battery-backed calculator keeps its RAM across a power cycle, and the ROM's
    // integrity table lives there -- so this is how a "warm boot" is modelled.
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    emu.run(500_000);
    let snapshot = emu.ram().to_vec();

    let mut restored = fx991::Emu::from_rom(rom_or_skip!());
    restored.set_ram(&snapshot);
    assert_eq!(restored.ram(), snapshot.as_slice());
}

#[test]
fn a_full_snapshot_round_trips_and_replays_the_rom() {
    // The property a debugger's rollback rests on: restoring must put the machine
    // back somewhere it *behaves* identically, not merely somewhere that compares
    // equal.  A snapshot missing the timer divider would drift only here.
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    emu.run(300_000);
    let snapshot = emu.snapshot();

    emu.run(50_000);
    let after_first = state_fingerprint(&emu);
    assert_ne!(after_first, fingerprint_of(&snapshot));

    emu.restore(&snapshot);
    assert_eq!(emu.snapshot(), snapshot, "the state came back exactly");
    emu.run(50_000);
    assert_eq!(
        state_fingerprint(&emu),
        after_first,
        "the replayed run diverges"
    );
}

/// The RAM-and-registers fingerprint the parity tests use, so a snapshot is
/// judged on observable behaviour rather than on equal fields.
fn state_fingerprint(emu: &fx991::Emu) -> (u32, u16, u8, u64, u64) {
    let mut hash: u64 = 0xCBF29CE484222325;
    let mut sum: u64 = 0;
    for (index, byte) in emu.ram().iter().enumerate() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001B3);
        sum = sum.wrapping_add((*byte as u64).wrapping_mul(index as u64 + 1));
    }
    (emu.pc(), emu.sp(), emu.psw(), hash, sum)
}

fn fingerprint_of(snapshot: &fx991::EmuSnapshot) -> (u32, u16, u8, u64, u64) {
    let mut hash: u64 = 0xCBF29CE484222325;
    let mut sum: u64 = 0;
    for (index, byte) in snapshot.chipset.bus.ram.iter().enumerate() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001B3);
        sum = sum.wrapping_add((*byte as u64).wrapping_mul(index as u64 + 1));
    }
    let regs = &snapshot.chipset.cpu.regs;
    (regs.physical_pc(), regs.sp, regs.psw(), hash, sum)
}

#[test]
fn a_quiet_read_does_not_disturb_the_machine() {
    // Scrolling a memory view must not look like the guest reading memory, or the
    // fault counters the fault-reporting tests assert on would move under them.
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    emu.run(10_000);
    let faults_before = emu.chipset.bus.fault_count;

    // 0x6_0000 is unmapped, so an ordinary read records a fault.
    let quiet = emu.peek_quiet(0x6_0000);
    assert_eq!(quiet, 0);
    assert_eq!(
        emu.chipset.bus.fault_count, faults_before,
        "a debugger's read is not a guest fault"
    );

    emu.peek(0x6_0000);
    assert_eq!(emu.chipset.bus.fault_count, faults_before + 1);
}

// ----------------------------------------------------- instruction semantics

/// Run a hand-written program on a machine whose ROM is a scratch image.
struct Scratch {
    machine: Chipset,
}

impl Scratch {
    /// `program` is planted at `0x10000`, an otherwise unused ROM bank.
    fn new(program: &[u8]) -> Self {
        let mut rom = vec![0u8; testkit::ROM_LEN];
        rom[0x0000] = 0x00; // SP
        rom[0x0001] = 0xF0;
        rom[0x10000..0x10000 + program.len()].copy_from_slice(program);
        // Drop the reset request: the program is the whole world here.
        let mut machine = Chipset::new(fx991_bus::Bus::new(fx991_bus::Rom::new(rom)));
        machine.reset();
        machine.interrupts.borrow_mut().active = [false; fx991_chipset::INT_COUNT];
        machine.interrupts.borrow_mut().pending_count = 0;
        // SP is already 0xF000 from the vector; point PC at the program.
        machine.cpu.regs.csr = 0x1;
        machine.cpu.regs.pc = 0x0000;
        Self { machine }
    }

    fn run(&mut self, count: u64) {
        for _ in 0..count {
            self.machine.tick(None);
        }
    }
}

#[test]
fn push_r0_writes_one_byte_but_moves_sp_by_two() {
    use nxu8_asm::asm;
    let mut scratch = Scratch::new(&[asm::push_r(0).as_slice(), &asm::RT].concat());
    scratch.machine.cpu.regs.sp = 0xE000;
    scratch.machine.cpu.regs.r[0] = 0x5C;
    // Pre-fill both bytes of the destination word so the untouched half is visible.
    scratch.machine.poke(0xDFFE, 0xAA);
    scratch.machine.poke(0xDFFF, 0xBB);
    scratch.run(1);
    assert_eq!(scratch.machine.cpu.regs.sp, 0xDFFE);
    // The low byte is written; the dummy half of the word keeps the old content.
    assert_eq!(scratch.machine.peek(0xDFFE), 0x5C);
    assert_eq!(scratch.machine.peek(0xDFFF), 0xBB);
}

#[test]
fn pop_pc_consumes_four_bytes_and_restores_csr() {
    use nxu8_asm::asm;
    let mut scratch = Scratch::new(&asm::pop_pc());
    scratch.machine.cpu.regs.sp = 0xE000;
    // [lo, mid, 0x30 | csr, 0x30] --
    scratch
        .machine
        .poke_block(0xE000, &[0xB8, 0x21, 0x32, 0x30]);
    scratch.run(1);
    assert_eq!(scratch.machine.cpu.regs.pc, 0x21B8);
    assert_eq!(scratch.machine.cpu.regs.csr, 2);
    assert_eq!(scratch.machine.cpu.regs.sp, 0xE004);
}

#[test]
fn the_pop_er0_gadget_costs_six_bytes_of_chain() {
    use nxu8_asm::asm;
    let program: Vec<u8> = [asm::pop_er(0), asm::pop_pc().to_vec()].concat();
    let mut scratch = Scratch::new(&program);
    scratch.machine.cpu.regs.sp = 0xE000;
    scratch
        .machine
        .poke_block(0xE000, &[0x11, 0x22, 0xB8, 0x21, 0x32, 0x30]);
    scratch.run(2);
    assert_eq!(scratch.machine.cpu.regs.er(0), 0x2211);
    assert_eq!(scratch.machine.cpu.regs.sp, 0xE006);
}

#[test]
fn push_lr_moves_four_bytes_and_writes_three() {
    use nxu8_asm::asm;
    let mut scratch = Scratch::new(&[asm::push_lr().as_slice(), &asm::RT].concat());
    scratch.machine.cpu.regs.sp = 0xE000;
    scratch.machine.cpu.regs.set_lr(0x1234);
    scratch.machine.cpu.regs.set_lcsr(5);
    scratch.run(1);
    assert_eq!(scratch.machine.cpu.regs.sp, 0xDFFC);
    // LCSR first through Push16, so byte 3 is its zero high byte -- POP PC only
    // reads bytes 0.2, which is why the 4-byte encoding can treat the last byte
    // as a don't-care.
    assert_eq!(scratch.machine.peek(0xDFFC), 0x34);
    assert_eq!(scratch.machine.peek(0xDFFD), 0x12);
    assert_eq!(scratch.machine.peek(0xDFFE), 0x05);
    assert_eq!(scratch.machine.peek(0xDFFF), 0x00);
}

#[test]
fn add_sp_masks_the_low_bit() {
    use nxu8_asm::asm;
    let mut scratch = Scratch::new(&asm::add_sp(3));
    scratch.machine.cpu.regs.sp = 0xE000;
    scratch.run(1);
    // 0xE003 rounded down to even.
    assert_eq!(scratch.machine.cpu.regs.sp, 0xE002);
}

#[test]
fn bl_keeps_the_return_address_in_lr_and_uses_no_stack() {
    use nxu8_asm::asm;
    let mut scratch = Scratch::new(&asm::bl(1, 0x1000));
    scratch.machine.cpu.regs.sp = 0xE000;
    let sp_before = scratch.machine.cpu.regs.sp;
    scratch.run(1);
    assert_eq!(scratch.machine.cpu.regs.pc, 0x1000);
    assert_eq!(scratch.machine.cpu.regs.csr, 1);
    assert_eq!(scratch.machine.cpu.regs.lr(), 4, "already-advanced PC");
    assert_eq!(scratch.machine.cpu.regs.sp, sp_before, "no stack use");
}

#[test]
fn a_bc_condition_uses_the_flags_snapshotted_before_the_instruction() {
    use nxu8_asm::asm;
    // MOV R0,#1; CMP R0,#1; BC EQ,+1; RT; NOP
    let program: Vec<u8> = [
        asm::mov_r_imm(0, 1),
        asm::sub_r_imm(0, 1),
        asm::bc(asm::cond::EQ, 1),
        asm::RT.to_vec(),
        asm::NOP.to_vec(),
    ]
    .concat();
    let mut scratch = Scratch::new(&program);
    scratch.run(3);
    // BC at offset 4, next PC 6, +1 word -> offset 8.
    assert_eq!(scratch.machine.cpu.regs.pc, 8);
}

#[test]
fn the_opcode_table_decodes_the_rom_the_way_the_listing_says() {
    // Cross-check the Rust decoder against the verified listing for a sample of
    // instructions: the listing's bytes must produce its text's handler family.
    let Some(text) = std::fs::read_to_string(testkit::disassembly_path()).ok() else {
        return;
    };
    let listing = fx991::listing::Listing::parse(&text);
    let bytes = rom_or_skip!();
    let mut checked = 0;
    for entry in listing.entries().iter().take(20_000) {
        let offset = entry.address as usize;
        if offset + 1 >= bytes.len() {
            continue;
        }
        let word = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        let decoded = nxu8_core::decode::dispatch()[word as usize];
        if decoded.is_none() {
            // Most of these are "Unrecognized command" in the listing.  The rest are
            // the halting vectors and their neighbours in the first 256 bytes, which
            // are vectors rather than instructions.
            // "Unrecognized" can appear after a mnemonic, as in
            // `BC      <Unrecognized>, 005E2h`, so look anywhere in the text.
            let listed_as_unrecognized =
                entry.text.contains("Unrecognized") || entry.text.contains("Wrong");
            assert!(
                listed_as_unrecognized || entry.address < 0x100,
                "the table cannot decode {:#06x}, which the listing calls {:?}",
                entry.address,
                entry.text
            );
            continue;
        }
        checked += 1;
    }
    assert!(
        checked > 15_000,
        "only {checked} instructions were decodable"
    );
}

#[test]
fn a_coprocessor_opcode_is_refused_loudly() {
    // `OP_CR_R` has no implementation on purpose; reaching one must be a located
    // failure rather than a plausible no-op.
    use nxu8_asm::{encode, word_of};
    let encoded = encode(0xA00E, &[], None).expect("CR transfer encodes");
    let mut rom = vec![0u8; testkit::ROM_LEN];
    rom[0x0001] = 0xF0;
    rom[0x10000] = encoded[0];
    rom[0x10001] = encoded[1];
    let mut machine = Chipset::new(fx991_bus::Bus::new(fx991_bus::Rom::new(rom)));
    machine.reset();
    machine.interrupts.borrow_mut().active = [false; fx991_chipset::INT_COUNT];
    machine.interrupts.borrow_mut().pending_count = 0;
    machine.cpu.regs.csr = 1;
    machine.cpu.regs.pc = 0;
    let mut sink = nxu8_core::ops::ctrl::NullSink::default();
    machine.cpu.step(&mut machine.bus, &mut sink);
    let fault = machine.cpu.unimplemented.expect("refused");
    assert_eq!(word_of(&encoded), 0xA00E);
    assert_eq!(fault.opcode, 0xA00E);
}

#[test]
fn the_timer_rate_is_the_reference_s_not_one_per_instruction() {
    // A per-instruction timer wedges the ROM; this is the guard against
    // "simplifying" it back.
    let mut machine = Chipset::new(fx991_bus::Bus::new(fx991_bus::Rom::new(rom_or_skip!())));
    machine.reset();
    assert_eq!(
        machine.timer.borrow().ticks_per_divide(),
        fx991_chipset::timer::TICKS_PER_DIVIDE
    );
    let per_divide = fx991_chipset::timer::TICKS_PER_DIVIDE;
    assert!(per_divide >= 200, "per-divide is {per_divide}");
}

#[test]
fn the_rom_reaches_its_main_loop_within_three_ticks_of_the_vector() {
    // Cheap smoke test that the boot path still runs: reset, trampoline, then the
    // `B 2h:04E40h` that enters the main loop.
    let mut machine = Chipset::new(fx991_bus::Bus::new(fx991_bus::Rom::new(rom_or_skip!())));
    machine.reset();
    for _ in 0..400 {
        machine.tick(None);
        if machine.cpu.regs.physical_pc() == 0x2_4E40 {
            return;
        }
    }
    panic!("the ROM never reached its main loop entry");
}

#[test]
fn the_first_stop_happens_before_the_hundred_thousandth_tick() {
    let mut machine = Chipset::new(fx991_bus::Bus::new(fx991_bus::Rom::new(rom_or_skip!())));
    machine.reset();
    for tick in 0..200_000u64 {
        machine.tick(None);
        if machine.run_mode() == RunMode::Stop {
            assert!(tick < 200_000);
            return;
        }
    }
    panic!("the ROM never entered STOP");
}

#[test]
fn a_hand_built_rop_chain_redirects_control() {
    // The M5 demonstration from: park the ROM, plant a
    // `POP PC` where it is sitting, point SP at a chain, and watch PC move.
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    emu.run_until(0x0_9216, 200_000).expect("idle");

    let chain: Vec<u8> = [
        // #pop-er0 = 0x2211, as [lo, mid, 0x30|csr, 0x30]
        vec![0xA8, 0x21, 0x31, 0x30, 0x11, 0x22],
        // jump to #debug = rom:22110
        vec![0x10, 0x21, 0x32, 0x30],
    ]
    .concat();

    emu.poke_block(0xE900, &chain);
    emu.load_code(0x0_9216, &nxu8_asm::asm::pop_pc());
    emu.set_sp(0xE900);
    // The STOP was requested by a write and is applied on the next tick, so the
    // machine has to be forced running *after* that request would have landed.
    emu.step();
    emu.chipset.force_run();

    emu.run_until(0x2_2110, 100)
        .expect("the chain reached #debug");
    assert_eq!(emu.er(0), 0x2211, "pop-er0 ran before the jump");
    assert_eq!(emu.sp(), 0xE90A, "the chain consumed ten bytes");
}

// ------------------------------------------------- M5: the `an` trigger
//
// The mechanism, measured on the emulator: evaluating an input containing `an`
// (FD 20) at offset `n` copies the input area to 0xD522 and leaves SP at
// 0xD522 + n + 2, where the epilogue at `rom:013CD0`
// (`MOV SP, ER14; POP XR4; POP QR8; POP PC`) runs the bytes at post-`an` offset
// 12 as a chain.

/// `[lo, mid, 0x30|csr, 0x30]`, per
fn encode_address(address: u32) -> [u8; 4] {
    [
        (address & 0xFF) as u8,
        ((address >> 8) & 0xFF) as u8,
        0x30 | ((address >> 16) & 0x0F) as u8,
        0x30,
    ]
}

const PARSE_BUF: u32 = 0xD522;
const INPUT_BUF: u32 = 0xD180;
const POP_ER0: u32 = 0x1_21A8;
const DEBUG_GADGET: u32 = 0x2_2110;

/// `n` bytes before `an`; a single numeric literal caps at 100 digits.
fn trigger_prefix(n: usize) -> Vec<u8> {
    if n <= 100 {
        vec![0x31; n]
    } else {
        let mut v = vec![0x31; 100];
        v.push(0xA6); // "+"
        v.extend(std::iter::repeat(0x31).take(n - 101));
        v
    }
}

/// 12 bytes of epilogue padding, then `POP ER0; POP PC` to `target`.
fn trigger_chain(target: u32) -> Vec<u8> {
    let mut v = vec![0x30; 12];
    v.extend(encode_address(POP_ER0));
    v.extend([0x11, 0x22]);
    v.extend(encode_address(target));
    v
}

/// Boot, wait for the key filter, plant the input, press EXE, and report
/// `(sp after the hijack, whether the chain reached its target)`.
fn fire_trigger(rom: Vec<u8>, n: usize, target: u32) -> (Option<u16>, bool) {
    let mut calculator = Calculator::from_rom(rom).expect("boot");
    let mut input = trigger_prefix(n);
    input.extend([0xFD, 0x20]); // `an`
    input.extend(trigger_chain(target));
    input.push(0x00);
    calculator.emu.poke_block(INPUT_BUF, &input);
    calculator.emu.chipset.keyboard.borrow_mut().press(0x60); // EXE

    let mut hijacked = None;
    let mut reached = false;
    for _ in 0..900_000 {
        calculator.emu.step();
        if hijacked.is_none() && calculator.emu.sp() < 0xE000 {
            hijacked = Some(calculator.emu.sp());
        }
        if calculator.emu.pc() == target {
            reached = true;
            break;
        }
    }
    (hijacked, reached)
}

#[test]
fn the_an_character_moves_sp_to_the_parse_buffer_plus_n_plus_two() {
    let rom = rom_or_skip!();
    for n in [0usize, 40, 76, 128] {
        let (sp, reached) = fire_trigger(rom.clone(), n, DEBUG_GADGET);
        assert_eq!(sp, Some((PARSE_BUF + n as u32 + 2) as u16), "n={n}");
        assert!(reached, "the chain did not run for n={n}");
    }
}

#[test]
fn without_an_the_stack_never_drops_into_low_ram() {
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    let mut input = vec![0x31u8; 40];
    input.extend(trigger_chain(DEBUG_GADGET));
    input.push(0x00);
    calculator.emu.poke_block(INPUT_BUF, &input);
    calculator.emu.chipset.keyboard.borrow_mut().press(0x60);
    for _ in 0..700_000 {
        calculator.emu.step();
        assert!(
            calculator.emu.sp() >= 0xE000,
            "SP moved without `an`: {:#06x}",
            calculator.emu.sp()
        );
    }
}

#[test]
fn a_numeric_prefix_caps_at_a_hundred_digits() {
    // 120 digits as one literal never fires; split at 100 it does.
    let (sp, _) = fire_trigger(rom_or_skip!(), 120, DEBUG_GADGET);
    assert_eq!(sp, Some((PARSE_BUF + 120 + 2) as u16));
}

#[test]
fn stepping_returns_the_handler_name() {
    let mut emu = fx991::Emu::from_rom(rom_or_skip!());
    // Consume the reset request: the vector's first instruction is `BC AL`.
    assert_eq!(emu.step(), TickOutcome::Executed("OP_BC"));
}

// ------------------------------------------- measured key semantics
//
// These pin the key names that the layout file gets wrong; the evidence is in the
// README's key table.

/// Boot and wait for the ROM's key-wait loop, then press and release a key by
/// name, waiting for the ROM to take it.
fn tap_and_settle(calculator: &mut Calculator, name: &str) {
    // `tap` already waits for ROM_KEY_ACCEPTED, but the power key never reaches
    // it (it resets instead), so that one goes through `press_key` directly.
    if name == "ON" {
        let code = calculator
            .emu
            .chipset
            .keyboard
            .borrow()
            .code_for_name(name)
            .expect("ON is in the table");
        calculator.emu.chipset.press_key(code);
        calculator.emu.run(120_000);
        calculator.emu.chipset.keyboard.borrow_mut().release(None);
        calculator.emu.run(120_000);
        return;
    }
    calculator.tap(name).expect("tap");
}

/// The screen buffer's row 0, which drives the indicator sprites.
fn status_row(calculator: &mut Calculator) -> Vec<u8> {
    calculator.emu.peek_block(0x0_F800, 32)
}

fn input_bytes(calculator: &mut Calculator, n: usize) -> Vec<u8> {
    calculator.emu.peek_block(0x0_D180, n)
}

#[test]
fn the_measured_modifiers_resolve_to_the_right_codes() {
    let calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    let table = calculator.emu.chipset.keyboard.borrow().table().clone();
    for (name, want) in [
        ("SHIFT", 0x07u8), // commonly labelled "F1"
        ("ALPHA", 0x17),   // commonly labelled "F2"
        ("STO", 0x03),     // commonly labelled "SHIFT"
        ("AC", 0x42),      // commonly labelled "Space"
        ("ON", 0xFF),      // commonly labelled "F4"
        ("DEL", 0x32),
    ] {
        assert_eq!(table.by_name(name).map(|k| k.code), Some(want), "{name}");
    }
}

#[test]
fn shift_lights_rsd_s_and_makes_sin_inverse() {
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    tap_and_settle(&mut calculator, "SHIFT");
    assert_eq!(status_row(&mut calculator)[0x00] & 0x01, 1, "rsd_s not lit");
    tap_and_settle(&mut calculator, "sin");
    assert_eq!(input_bytes(&mut calculator, 1)[0], 0x7A, "not sin^-1");
}

#[test]
fn alpha_lights_rsd_a_and_selects_the_variable() {
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    tap_and_settle(&mut calculator, "ALPHA");
    assert_eq!(status_row(&mut calculator)[0x01] & 0x01, 1, "rsd_a not lit");
    tap_and_settle(&mut calculator, "sin");
    assert_eq!(input_bytes(&mut calculator, 1)[0], 0x45, "not D");
}

#[test]
fn sto_lights_rsd_sto_and_is_not_shift() {
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    tap_and_settle(&mut calculator, "STO");
    let row = status_row(&mut calculator);
    assert_eq!(row[0x03] & 0x01, 1, "rsd_sto not lit");
    assert_eq!(row[0x00] & 0x01, 0, "0x03 behaves like SHIFT");
}

#[test]
fn ac_clears_the_input_area_and_del_removes_one_character() {
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    for name in ["1", "2", "3"] {
        tap_and_settle(&mut calculator, name);
    }
    assert_eq!(input_bytes(&mut calculator, 3), vec![0x31, 0x32, 0x33]);

    tap_and_settle(&mut calculator, "DEL");
    assert_eq!(input_bytes(&mut calculator, 3), vec![0x31, 0x32, 0x00]);

    tap_and_settle(&mut calculator, "AC");
    assert_eq!(input_bytes(&mut calculator, 1)[0], 0x00, "AC did not clear");
}

#[test]
fn the_power_key_resets_the_machine() {
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    for name in ["1", "2", "3"] {
        tap_and_settle(&mut calculator, name);
    }
    assert_ne!(calculator.emu.sp(), 0xF000);

    // `press_key` applies the power key's reset synchronously, so SP is back at
    // the top of RAM at that instant; the ROM then starts initialising.
    let code = 0xFFu8;
    assert!(calculator.emu.chipset.press_key(code));
    assert_eq!(calculator.emu.sp(), 0xF000, "power key did not reset SP");
    assert_eq!(
        calculator.emu.chipset.run_mode(),
        RunMode::Run,
        "a reset should leave the machine running"
    );
}

#[test]
fn shift_plus_the_two_argument_log_key_gives_the_ten_to_the_x_prefix() {
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    tap_and_settle(&mut calculator, "SHIFT");
    tap_and_settle(&mut calculator, "log(a,b)");
    assert_eq!(
        input_bytes(&mut calculator, 4),
        vec![0x73, 0x1A, 0x19, 0x1B]
    );
}

#[test]
fn a_key_string_types_a_multi_box_template_only_partly_and_that_is_the_caller_s_problem() {
    // `log(a,b)` is the *two-argument* key: its template is `7D 1A 19 1C 19 1B`, so
    // `press("log(a,b)4")` leaves the second box empty and the ROM evaluates an
    // undefined value.  Pinning it here because the failure is silent -- the answer
    // is simply `0`, not a syntax error.
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    tap_and_settle(&mut calculator, "log(a,b)");
    tap_and_settle(&mut calculator, "4");
    let bytes = input_bytes(&mut calculator, 8);
    assert_eq!(bytes[0], 0x7D, "the log token");
    assert_eq!(bytes[1], 0x1A, "box 1 opens");
    assert_eq!(bytes[3], 0x1C, "the two-argument separator");
    assert_eq!(bytes[4], 0x19, "box 2 is empty");
}

#[test]
fn the_single_argument_log_is_shift_plus_the_negate_key() {
    // `log` with one argument is SHIFT + `(-)`, and it writes a bare `7D` -- no
    // template, no boxes.  This is the key a person means when they say `lg`.
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    tap_and_settle(&mut calculator, "SHIFT");
    tap_and_settle(&mut calculator, "(-)");
    assert_eq!(input_bytes(&mut calculator, 2), vec![0x7D, 0x00]);
}

#[test]
fn take_frame_request_reports_changes_once() {
    // A renderer calls this each frame; it must report a change after something
    // visible happens, and not repeat itself once consumed.
    let mut calculator = Calculator::from_rom(rom_or_skip!()).expect("boot");
    let chipset = &mut calculator.emu.chipset;
    // Drain whatever boot left behind.
    while chipset.take_frame_request() {}

    chipset.poke(0x0_F800, 0xFF); // light some dots
    assert!(
        chipset.take_frame_request(),
        "a buffer write should request a frame"
    );
    assert!(
        !chipset.take_frame_request(),
        "the request should be consumed, not sticky"
    );

    // The keyboard has its own flag: a press changes the highlight even though
    // nothing in the screen buffer moved.
    assert!(chipset.press_key(0x00), "press '1'");
    assert!(
        chipset.take_frame_request(),
        "a key press should request a frame"
    );
    assert!(!chipset.take_frame_request());
}
