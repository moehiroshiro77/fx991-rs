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

//! Shift and bit-access handlers.
//!
//! Derived from the hardware's documented behaviour:(shifts) and `:196-241`
//! (`OP_BITMOD`, `OP_EXTBW`).
//!
//! `SRLC` reads the *next* byte register as the shift-in buffer and `SLLC` the
//! previous one; both wrap modulo 16, so e.g. `SLLC R0` shifts in `R15`.

use crate::alu::{shift_left8, shift_right8, zs_check};
use crate::decode::H_TI;
use crate::regs::PSW_Z;
use crate::{Cpu, Memory};

// `OP_BITMOD` dispatches on the low nibble of the opcode.
const BITMOD_OR: u16 = 0; // SB
const BITMOD_TEST: u16 = 1; // TB
const BITMOD_AND: u16 = 2; // RB

pub fn op_sll(cpu: &mut Cpu) {
    cpu.shift_buffer = 0;
    shift_left8(cpu);
}

pub fn op_sllc(cpu: &mut Cpu) {
    let external = (cpu.operands[0].register_index + 15) & 15;
    cpu.shift_buffer = cpu.regs.r[external];
    shift_left8(cpu);
}

pub fn op_sra(cpu: &mut Cpu) {
    let shift_by = (cpu.operands[1].value & 7) as u8;
    let msb = cpu.operands[0].value & 0x80 != 0;
    cpu.shift_buffer = 0;
    shift_right8(cpu);
    if msb {
        cpu.operands[0].value |= ((0xFFu16 >> shift_by) ^ 0xFF) as u64;
    }
}

pub fn op_srl(cpu: &mut Cpu) {
    cpu.shift_buffer = 0;
    shift_right8(cpu);
}

pub fn op_srlc(cpu: &mut Cpu) {
    let external = (cpu.operands[0].register_index + 1) & 15;
    cpu.shift_buffer = cpu.regs.r[external];
    shift_right8(cpu);
}

/// `SB` / `RB` / `TB`, register or memory form.
pub fn op_bitmod<B: Memory>(cpu: &mut Cpu, bus: &mut B) {
    let bit_in = 1u64 << cpu.operands[1].value;

    let src_index;
    if cpu.hint & H_TI != 0 {
        src_index = cpu.long_imm as usize;
        cpu.operands[0].value =
            bus.read_data(((cpu.regs.dsr as u32) << 16) | src_index as u32) as u64;
    } else {
        src_index = cpu.operands[0].value as usize;
        cpu.operands[0].value = cpu.regs.r[src_index & 15] as u64;
    }

    cpu.flags_changed |= PSW_Z;
    cpu.flags_out = if cpu.operands[0].value & bit_in != 0 {
        0
    } else {
        PSW_Z
    };

    let kind = cpu.opcode & 0x000F;
    if kind == BITMOD_OR {
        cpu.operands[0].value |= bit_in;
    } else if kind == BITMOD_AND {
        cpu.operands[0].value &= !bit_in;
    }

    if kind != BITMOD_TEST {
        if cpu.hint & H_TI != 0 {
            let address = ((cpu.regs.dsr as u32) << 16) | src_index as u32;
            bus.write_data(address, cpu.operands[0].value as u8);
        } else {
            cpu.regs.r[src_index & 15] = cpu.operands[0].value as u8;
        }
    }
}

/// `EXTBW Rn` -- sign-extend Rn into Rn+1.
pub fn op_extbw(cpu: &mut Cpu) {
    let index = ((cpu.opcode & 0x00E0) >> 4) as usize;
    let value = if cpu.regs.r[index] & 0x80 != 0 {
        0xFF
    } else {
        0x00
    };
    cpu.operands[0].value = value;
    cpu.regs.r[(index + 1) & 15] = value as u8;
    zs_check(cpu);
}
