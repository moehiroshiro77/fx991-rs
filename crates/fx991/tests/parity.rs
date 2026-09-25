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

//! Regression baselines for the machine state after a fixed number of ticks.
//!
//! A fingerprint is the cheapest way to notice that emulation has drifted: run for
//! a known number of instructions and compare the register file, the whole RAM, and
//! the interrupt bookkeeping.  Any change to the CPU, the bus or the peripherals
//! moves one of these numbers, and the failure names the field that moved rather
//! than just saying "the goldens changed".
//!
//! The values in `baseline` were recorded from this emulator and are pinned here on
//! purpose: they are a change detector, not a proof of correctness.  If a change is
//! intended, update the numbers *and* say why in the commit message.

use fx991::Emu;
use fx991_chipset::RunMode;
use fx991_model::address;
use nxu8_core::Memory;

fn rom() -> Option<Vec<u8>> {
    testkit::rom_bytes().ok()
}

/// FNV-1a 64 over the RAM, plus an index-weighted byte sum.
///
/// Both must match: a hash alone can collide, and a sum alone hides a transposition.
fn ram_fingerprint(ram: &[u8]) -> (u64, u64) {
    let mut hash: u64 = 0xCBF2_9CE4_8422_2325;
    let mut sum: u64 = 0;
    for (index, byte) in ram.iter().enumerate() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01B3);
        sum += (*byte as u64) * (index as u64 + 1);
    }
    (hash, sum)
}

/// The state after 500 000 instructions.
mod baseline {
    /// Physical PC.
    pub const PC: u32 = 0x9216;
    /// Stack pointer.
    pub const SP: u16 = 0xEE34;
    /// Program status word.
    pub const PSW: u8 = 0x00;
    /// Interrupt mask SFR (`0xF010`).
    pub const INT_MASK: u16 = 0x0022;
    /// Keyboard input filter (`0xF042`).
    pub const KEY_FILTER: u8 = 0xFF;
    /// Whether the machine is parked.
    pub const STOPPED: bool = true;
    /// FNV-1a 64 over the 0x2000 bytes of battery-backed RAM.
    pub const RAM_FNV: u64 = 0x3AE3_3B0F_6676_EB64;
    /// Index-weighted byte sum over the same RAM.
    pub const RAM_SUM: u64 = 225_461_387;
}

#[test]
fn the_machine_state_matches_the_recorded_baseline() {
    let Some(bytes) = rom() else { return };
    let mut emu = Emu::from_rom(bytes);
    emu.run(500_000);

    let regs = &emu.chipset.cpu.regs;
    assert_eq!(emu.pc(), baseline::PC, "physical PC");
    assert_eq!(regs.sp, baseline::SP, "stack pointer");
    assert_eq!(regs.psw(), baseline::PSW, "PSW");
    assert_eq!(
        emu.chipset.interrupts.borrow().mask,
        baseline::INT_MASK,
        "interrupt mask"
    );
    assert_eq!(
        emu.chipset.bus.read_data(address::KEY_INPUT_FILTER),
        baseline::KEY_FILTER,
        "key filter"
    );
    assert_eq!(
        emu.chipset.run_mode() != RunMode::Run,
        baseline::STOPPED,
        "run mode"
    );

    // The RAM fingerprint is the strong check: it covers every byte the ROM wrote,
    // not just the handful of registers above.
    let (hash, sum) = ram_fingerprint(emu.ram());
    assert_eq!(hash, baseline::RAM_FNV, "RAM fingerprint");
    assert_eq!(sum, baseline::RAM_SUM, "RAM weighted sum");
}

#[test]
fn the_boot_path_reaches_the_same_milestones() {
    let Some(bytes) = rom() else { return };
    let mut emu = Emu::from_rom(bytes);

    // The reset entry, seen through the pre-execute hook.
    let mut entry = None;
    emu.step_with(&mut |pc| {
        entry = Some(pc);
        false
    });
    assert_eq!(entry, Some(0x0_946A), "reset vector");

    assert!(
        emu.run_until_or_budget(0x2_4E40, 100),
        "the main loop entry should be reached within a few dozen instructions"
    );
    assert!(
        emu.run_until_or_budget(0x0_9216, 5_000),
        "the first STOP should happen well before the five-thousandth tick"
    );

    // The key filter eventually arms, which is the check that catches a
    // per-instruction timer: with one, the ROM wedges at tick 74 691 and never gets
    // here.
    let mut armed_at = None;
    for _ in 0..2_000_000 {
        emu.step();
        if emu.chipset.ready_for_key() {
            armed_at = Some(emu.ticks);
            break;
        }
    }
    let armed_at = armed_at.expect("the ROM should eventually arm the key filter");
    assert!(
        (300_000..1_500_000).contains(&armed_at),
        "the key filter armed at {armed_at}, outside the expected band"
    );
}

#[test]
fn the_input_area_uses_the_measured_token_layout() {
    // `1+2` produces `31 A6 32` in both implementations, and `+` is 0xA6 because
    // 's "=" label for that key is wrong.
    let Some(bytes) = rom() else { return };
    let mut calculator = fx991::Calculator::from_rom(bytes).expect("boot");
    calculator.press("1+2").expect("press");
    assert_eq!(calculator.input_bytes(3), vec![0x31, 0xA6, 0x32]);
}

#[test]
fn evaluating_one_plus_two_produces_the_expected_ans_bytes() {
    let Some(bytes) = rom() else { return };
    let mut calculator = fx991::Calculator::from_rom(bytes).expect("boot");
    calculator.eval("1+2").expect("eval");
    assert_eq!(
        calculator.answer_bytes(),
        vec![0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]
    );
}
