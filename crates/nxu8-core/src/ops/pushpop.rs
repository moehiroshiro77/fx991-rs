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

//! PUSH/POP handlers.
//!
//! Stack instructions.  The stack rules here are the whole
//! foundation of the ROP work, so they are quoted rather than paraphrased:
//!
//! `text
//! OP_PUSH:  size = register_size;  if size == 1 { size = 2 }
//!           SP -= size
//!           for ix in (0.register_size).rev { MEM[SP+ix] = value >> (8*ix) }
//! OP_POP:   size = register_size;  if size == 1 { size = 2 }
//!           value = little-endian read of register_size bytes at SP
//!           SP += size
//! `
//!
//! i.e. **`PUSH R0` moves SP by 2 but writes only one byte**; the dummy half of
//! the word keeps whatever was already on the stack, and `POP ERn` reads that
//! stale byte back.  The manual calls the dummy cycle "indeterminate", so the
//! behaviour was settled by running the ROM: a push leaves the upper byte
//! untouched, which is what ROP chain layout depends on.
//!
//! `POPL` walks its list bits low to high: EA, PC, PSW, LR.  That is the reverse
//! of the push order, which looks like a mistake and is not: the encoding numbers
//! the fields so that this order pops them in the right sequence.  Do not "tidy"
//! it into ascending order.

use crate::{Cpu, Memory};

/// `PUSH Rn/ERn/XRn/QRn`.
pub fn op_push<B: Memory>(cpu: &mut Cpu, bus: &mut B) {
    let register_size = cpu.operands[1].register_size;
    let value = cpu.operands[1].value;
    let push_size = if register_size == 1 { 2 } else { register_size };
    cpu.regs.sp = cpu.regs.sp.wrapping_sub(push_size as u16);
    for ix in (0..register_size).rev() {
        bus.write_data(
            cpu.regs.sp.wrapping_add(ix as u16) as u32,
            (value >> (8 * ix)) as u8,
        );
    }
}

fn push16<B: Memory>(cpu: &mut Cpu, bus: &mut B, data: u16) {
    cpu.regs.sp = cpu.regs.sp.wrapping_sub(2);
    bus.write_data(cpu.regs.sp.wrapping_add(1) as u32, (data >> 8) as u8);
    bus.write_data(cpu.regs.sp as u32, data as u8);
}

fn pop16<B: Memory>(cpu: &mut Cpu, bus: &mut B) -> u16 {
    let low = bus.read_data(cpu.regs.sp as u32) as u16;
    let high = bus.read_data(cpu.regs.sp.wrapping_add(1) as u32) as u16;
    cpu.regs.sp = cpu.regs.sp.wrapping_add(2);
    low | (high << 8)
}

/// `PUSH register_list`: bit1 = ELR/ECSR, bit2 = EPSW, bit3 = LR/LCSR, bit0 = EA,
/// pushed in that order.
pub fn op_pushl<B: Memory>(cpu: &mut Cpu, bus: &mut B) {
    let mask = cpu.operands[1].value as u8;
    let level = cpu.regs.elevel();

    if mask & 2 != 0 {
        if cpu.memory_model_large {
            let value = cpu.regs.ecsr[level];
            push16(cpu, bus, value);
        }
        let value = cpu.regs.elr[level];
        push16(cpu, bus, value);
    }
    if mask & 4 != 0 {
        let value = u16::from(cpu.regs.epsw[level]);
        push16(cpu, bus, value);
    }
    if mask & 8 != 0 {
        if cpu.memory_model_large {
            let value = cpu.regs.lcsr();
            push16(cpu, bus, value);
        }
        let value = cpu.regs.lr();
        push16(cpu, bus, value);
    }
    if mask & 1 != 0 {
        let value = cpu.regs.ea;
        push16(cpu, bus, value);
    }
}

/// `POP Rn/ERn/XRn/QRn`, writeback handled by the caller via `H_WB`.
pub fn op_pop<B: Memory>(cpu: &mut Cpu, bus: &mut B) {
    let register_size = cpu.operands[0].register_size;
    let pop_size = if register_size == 1 { 2 } else { register_size };
    let mut value = 0u64;
    for ix in 0..register_size {
        value |= (bus.read_data(cpu.regs.sp.wrapping_add(ix as u16) as u32) as u64) << (8 * ix);
    }
    cpu.operands[0].value = value;
    cpu.regs.sp = cpu.regs.sp.wrapping_add(pop_size as u16);
}

/// `POP register_list` in the source's own order: EA, LR, PSW, PC.
pub fn op_popl<B: Memory>(cpu: &mut Cpu, bus: &mut B) {
    let mask = cpu.operands[0].value as u8;

    if mask & 1 != 0 {
        cpu.regs.ea = pop16(cpu, bus);
    }
    if mask & 8 != 0 {
        let value = pop16(cpu, bus);
        cpu.regs.set_lr(value);
        if cpu.memory_model_large {
            let csr = pop16(cpu, bus) & 0x000F;
            cpu.regs.set_lcsr(csr);
        }
    }
    if mask & 4 != 0 {
        let value = pop16(cpu, bus) as u8;
        cpu.regs.set_psw(value);
    }
    if mask & 2 != 0 {
        cpu.regs.pc = pop16(cpu, bus);
        if cpu.memory_model_large {
            cpu.regs.csr = pop16(cpu, bus) & 0x000F;
        }
    }
}

/// `POP PC` is the one list form the ROM actually contains, and it
/// is what every ROP gadget ends with, so the masks get named constants.
pub const LIST_EA: u8 = 1;
pub const LIST_PC: u8 = 2;
pub const LIST_PSW: u8 = 4;
pub const LIST_LR: u8 = 8;
