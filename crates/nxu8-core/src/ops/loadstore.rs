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

//! Load/store handlers.
//!
//! The manual describes six addressing modes here, which share one implementation.
//! that all funnel into one `LoadStore(offset, length)` core; keeping that shape
//! beats writing twenty near-identical functions.
//!
//! Three details in the core that are easy to get wrong
//!:
//!
//! * even-length accesses round the offset down to a halfword boundary;
//! * stores go **from the highest byte down**;
//! * `H_IA` bumps EA by `length` afterwards, and `bump_ea` re-aligns EA to even
//!   for non-byte accesses.
//!
//! Also note the load path calls `zs_check` **per byte read**, so the final Z/S
//! reflect the last byte while C/OV/HC are left alone.

use crate::alu::zs_check;
use crate::decode::{H_IA, H_ST};
use crate::{Cpu, Memory};

#[inline]
fn bump_ea(cpu: &mut Cpu, value_size: usize) {
    cpu.regs.ea = cpu.regs.ea.wrapping_add(value_size as u16);
    if value_size != 1 {
        cpu.regs.ea &= !1;
    }
}

fn load_store<B: Memory>(cpu: &mut Cpu, bus: &mut B, offset: u16, length: usize) {
    let mut offset = offset;
    if length.is_multiple_of(2) {
        offset &= !1;
    }

    let reg_base = cpu.operands[0].value as usize;
    let dsr = (cpu.regs.dsr as u32) << 16;

    if cpu.hint & H_ST != 0 {
        for ix in (0..length).rev() {
            let address = dsr | ((offset.wrapping_add(ix as u16)) as u32);
            bus.write_data(address, cpu.regs.r[(reg_base + ix) & 15]);
        }
    } else {
        for ix in 0..length {
            let address = dsr | ((offset.wrapping_add(ix as u16)) as u32);
            cpu.operands[0].value = bus.read_data(address) as u64;
            zs_check(cpu);
            cpu.regs.r[(reg_base + ix) & 15] = cpu.operands[0].value as u8;
        }
    }

    if cpu.hint & H_IA != 0 {
        bump_ea(cpu, length);
    }
}

pub fn op_ls_ea<B: Memory>(cpu: &mut Cpu, bus: &mut B) {
    let length = (cpu.hint >> 8) as usize;
    load_store(cpu, bus, cpu.regs.ea, length);
}

pub fn op_ls_r<B: Memory>(cpu: &mut Cpu, bus: &mut B) {
    let length = (cpu.hint >> 8) as usize;
    let offset = cpu.operands[1].value as u16;
    load_store(cpu, bus, offset, length);
}

pub fn op_ls_i_r<B: Memory>(cpu: &mut Cpu, bus: &mut B) {
    let length = (cpu.hint >> 8) as usize;
    let offset = (cpu.operands[1].value as u16).wrapping_add(cpu.long_imm);
    load_store(cpu, bus, offset, length);
}

/// BP-relative: the displacement is a signed 6-bit value plus ER12.
pub fn op_ls_bp<B: Memory>(cpu: &mut Cpu, bus: &mut B) {
    let length = (cpu.hint >> 8) as usize;
    let mut value = cpu.operands[1].value as u16;
    if value & 0x20 != 0 {
        value |= 0xFFC0;
    }
    let bp = (cpu.regs.r[12] as u16) | ((cpu.regs.r[13] as u16) << 8);
    load_store(cpu, bus, value.wrapping_add(bp), length);
}

/// FP-relative: same shape, ER14 instead of ER12.
pub fn op_ls_fp<B: Memory>(cpu: &mut Cpu, bus: &mut B) {
    let length = (cpu.hint >> 8) as usize;
    let mut value = cpu.operands[1].value as u16;
    if value & 0x20 != 0 {
        value |= 0xFFC0;
    }
    let fp = (cpu.regs.r[14] as u16) | ((cpu.regs.r[15] as u16) << 8);
    load_store(cpu, bus, value.wrapping_add(fp), length);
}

/// Absolute address from the long immediate.
pub fn op_ls_i<B: Memory>(cpu: &mut Cpu, bus: &mut B) {
    let length = (cpu.hint >> 8) as usize;
    load_store(cpu, bus, cpu.long_imm, length);
}
