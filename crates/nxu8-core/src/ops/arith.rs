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

//! Arithmetic, logical and ALU handlers.
//!
//! Derived from the hardware's documented behaviour:(`OP_ADD`. `OP_NEG`, plus the
//! two EA-counter instructions at the bottom of that file).  All flag work goes
//! through [`crate::alu`].

use crate::alu::{add8, zs_check};
use crate::decode::H_IE;
use crate::regs::{PSW_C, PSW_HC, PSW_OV, PSW_Z};
use crate::{Cpu, Memory};

/// `ADD` clears C and sets Z, then behaves as `ADDC`.
pub fn op_add(cpu: &mut Cpu) {
    cpu.flags_in &= !PSW_C;
    cpu.flags_in |= PSW_Z;
    op_addc(cpu);
}

pub fn op_add16(cpu: &mut Cpu) {
    if cpu.hint & H_IE != 0 {
        let v = cpu.operands[1].value;
        cpu.operands[1].value = v | if v & 0x40 != 0 { 0xFF80 } else { 0 };
    }

    cpu.flags_in &= !PSW_C;

    let high0 = (cpu.operands[0].value >> 8) & 0xFF;
    let high1 = (cpu.operands[1].value >> 8) & 0xFF;
    add8(cpu);
    zs_check(cpu);

    cpu.flags_in = (cpu.flags_in & !PSW_C) | (cpu.flags_out & PSW_C);

    let low0 = cpu.operands[0].value & 0xFF;
    cpu.operands[0].value = high0;
    cpu.operands[1].value = high1;
    add8(cpu);
    zs_check(cpu);

    cpu.operands[0].value = ((cpu.operands[0].value & 0xFF) << 8) | low0;
}

pub fn op_addc(cpu: &mut Cpu) {
    add8(cpu);
    if cpu.flags_in & PSW_Z == 0 {
        cpu.flags_out &= !PSW_Z;
    }
    zs_check(cpu);
}

pub fn op_and(cpu: &mut Cpu) {
    cpu.operands[0].value &= cpu.operands[1].value & 0xFF;
    zs_check(cpu);
}

pub fn op_mov16(cpu: &mut Cpu) {
    if cpu.hint & H_IE != 0 {
        let v = cpu.operands[1].value;
        cpu.operands[1].value = v | if v & 0x40 != 0 { 0xFF80 } else { 0 };
    }

    cpu.operands[0].value = cpu.operands[1].value & 0xFF;
    zs_check(cpu);

    let low0 = cpu.operands[0].value & 0xFF;
    cpu.operands[0].value = (cpu.operands[1].value >> 8) & 0xFF;
    zs_check(cpu);

    cpu.operands[0].value = ((cpu.operands[0].value & 0xFF) << 8) | low0;
}

pub fn op_mov(cpu: &mut Cpu) {
    cpu.operands[0].value = cpu.operands[1].value & 0xFF;
    zs_check(cpu);
}

pub fn op_or(cpu: &mut Cpu) {
    cpu.operands[0].value |= cpu.operands[1].value & 0xFF;
    zs_check(cpu);
}

pub fn op_xor(cpu: &mut Cpu) {
    cpu.operands[0].value ^= cpu.operands[1].value & 0xFF;
    zs_check(cpu);
}

/// A 16-bit subtract without writeback.
pub fn op_cmp16(cpu: &mut Cpu) {
    cpu.flags_in &= !PSW_C;

    let high0 = (cpu.operands[0].value >> 8) & 0xFF;
    let high1 = (cpu.operands[1].value >> 8) & 0xFF;
    cpu.operands[0].value ^= 0xFF;
    add8(cpu);
    cpu.operands[0].value ^= 0xFF;
    zs_check(cpu);

    cpu.flags_in = (cpu.flags_in & !PSW_C) | (cpu.flags_out & PSW_C);

    let low0 = cpu.operands[0].value & 0xFF;
    cpu.operands[0].value = high0;
    cpu.operands[1].value = high1;
    cpu.operands[0].value ^= 0xFF;
    add8(cpu);
    cpu.operands[0].value ^= 0xFF;
    zs_check(cpu);

    cpu.operands[0].value = ((cpu.operands[0].value & 0xFF) << 8) | low0;
}

pub fn op_sub(cpu: &mut Cpu) {
    cpu.flags_in &= !PSW_C;
    cpu.flags_in |= PSW_Z;
    op_subc(cpu);
}

pub fn op_subc(cpu: &mut Cpu) {
    cpu.operands[0].value ^= 0xFF;
    add8(cpu);
    cpu.operands[0].value ^= 0xFF;
    if cpu.flags_in & PSW_Z == 0 {
        cpu.flags_out &= !PSW_Z;
    }
    zs_check(cpu);
}

/// `DAA`.
pub fn op_daa(cpu: &mut Cpu) {
    let op0 = cpu.operands[0].value & 0xFF;
    let mut add = 0u64;
    if (op0 & 0x0F) > 0x09 || cpu.flags_in & PSW_HC != 0 {
        add |= 0x06;
    }
    if (op0 & 0xF0) > 0x90 || cpu.flags_in & PSW_C != 0 {
        add |= 0x60;
    }
    if (op0 & 0xF0) == 0x90 && (op0 & 0x0F) > 0x09 && cpu.flags_in & PSW_HC == 0 {
        add |= 0x60;
    }
    cpu.operands[1].value = add;
    let flags_in_backup = cpu.flags_in;
    op_add(cpu);
    cpu.flags_out |= flags_in_backup & PSW_C;
    cpu.flags_changed &= !PSW_OV;
}

/// `DAS`.
pub fn op_das(cpu: &mut Cpu) {
    let op0 = cpu.operands[0].value & 0xFF;
    let mut sub = 0u64;
    if (op0 & 0x0F) > 0x09 || cpu.flags_in & PSW_HC != 0 {
        sub |= 0x06;
    }
    if (op0 & 0xF0) > 0x90 || cpu.flags_in & PSW_C != 0 {
        sub |= 0x60;
    }
    cpu.operands[1].value = sub;
    let flags_in_backup = cpu.flags_in;
    op_sub(cpu);
    cpu.flags_out |= flags_in_backup & PSW_C;
    cpu.flags_changed &= !PSW_OV;
}

pub fn op_neg(cpu: &mut Cpu) {
    cpu.operands[1].value = cpu.operands[0].value;
    cpu.operands[0].value = 0;
    op_sub(cpu);
}

/// 8x8 -> 16 multiply; Z comes from the result.
pub fn op_mul(cpu: &mut Cpu) {
    cpu.operands[0].value &= 0xFF;
    cpu.operands[0].value *= cpu.operands[1].value & 0xFF;
    cpu.flags_changed |= PSW_Z;
    cpu.flags_out = if cpu.operands[0].value == 0 { PSW_Z } else { 0 };
}

/// Quotient into operand 0, remainder into the register in opcode bits 4-7.
pub fn op_div(cpu: &mut Cpu) {
    cpu.flags_changed |= PSW_Z | PSW_C;
    let divisor = cpu.operands[1].value;
    if divisor == 0 {
        cpu.flags_out |= PSW_C;
        return;
    }

    let quotient = cpu.operands[0].value / divisor;
    let remainder = cpu.operands[0].value % divisor;

    cpu.operands[0].value = quotient;
    if cpu.operands[0].value != 0 {
        cpu.flags_out &= !PSW_Z;
    }

    let remainder_reg_index = ((cpu.opcode >> 4) & 0x000F) as usize;
    cpu.regs.r[remainder_reg_index] = remainder as u8;
}

pub fn op_inc_ea<B: Memory>(cpu: &mut Cpu, bus: &mut B) {
    let address = ((cpu.regs.dsr as u32) << 16) | cpu.regs.ea as u32;
    cpu.operands[0].value = bus.read_data(address) as u64;
    cpu.operands[1].value = 1;
    op_add(cpu);
    cpu.flags_changed &= !PSW_C;
    let address = ((cpu.regs.dsr as u32) << 16) | cpu.regs.ea as u32;
    bus.write_data(address, cpu.operands[0].value as u8);
}

pub fn op_dec_ea<B: Memory>(cpu: &mut Cpu, bus: &mut B) {
    let address = ((cpu.regs.dsr as u32) << 16) | cpu.regs.ea as u32;
    cpu.operands[0].value = bus.read_data(address) as u64;
    cpu.operands[1].value = 1;
    op_sub(cpu);
    cpu.flags_changed &= !PSW_C;
    let address = ((cpu.regs.dsr as u32) << 16) | cpu.regs.ea as u32;
    bus.write_data(address, cpu.operands[0].value as u8);
}
