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

//! Control-register, branch and PSW handlers.
//!
//! Control-flow instructions.  Notable points:  Notable points:
//!
//! * `OP_CTRL` dispatches on `impl_hint >> 8`, the 1.=11 selector packed into the
//!   table's hint field.  Selector 5 is `MOV ERn,SP`, 11 is `MOV SP,ERn`.
//! * `MOV SP,ERn` and `ADD SP,#imm` both mask SP with `0xFFFE`.
//! * `MOV Rn,PSW` (selector 9) and `MOV PSW,obj` (6/7) address `epsw[ELEVEL]`, not
//!   the current PSW, unless ELEVEL is 0.
//! * `BC`: `le = Z | C`, `lts = OV ^ S`, `les = lts | Z`; the displacement is a
//!   signed 7-bit **word** offset applied to the already advanced PC.
//! * `BL` sets LR/LCSR to the already advanced PC/CSR, then performs `B`.  `RT`
//!   restores PC/CSR from LR/LCSR and uses no stack.

use crate::regs::{PSW_C, PSW_OV, PSW_S, PSW_Z};
use crate::Cpu;

/// Interrupt hooks the host must provide so the chipset can see `SWI` and `BRK`.
///
/// Keeping this as a trait rather than a callback keeps `nxu8-core` free of any
/// knowledge about a particular chipset, which is what lets it be a standalone
/// library.
pub trait ControlSink {
    fn raise_software(&mut self, index: u32);
    fn break_(&mut self);
}

pub fn op_addsp(cpu: &mut Cpu) {
    let mut value = cpu.operands[0].value as u16;
    if value & 0x80 != 0 {
        value |= 0xFF00;
    }
    cpu.regs.sp = cpu.regs.sp.wrapping_add(value);
    cpu.regs.sp &= 0xFFFE;
}

pub fn op_ctrl(cpu: &mut Cpu) {
    let selector = cpu.hint >> 8;
    let level = cpu.regs.elevel();

    match selector {
        1 => cpu.regs.ecsr[level] = cpu.operands[1].value as u16,
        2 => cpu.regs.elr[level] = cpu.operands[1].value as u16,
        3 => {
            if level != 0 {
                cpu.regs.epsw[level] = cpu.operands[1].value as u8;
            }
        }
        4 => cpu.operands[0].value = cpu.regs.elr[level] as u64,
        5 => cpu.operands[0].value = cpu.regs.sp as u64,
        6 | 7 => cpu.regs.set_psw(cpu.operands[1].value as u8),
        8 => cpu.operands[0].value = cpu.regs.ecsr[level] as u64,
        9 => {
            if level != 0 {
                cpu.operands[0].value = cpu.regs.epsw[level] as u64;
            }
        }
        10 => cpu.operands[0].value = cpu.regs.psw() as u64,
        11 => {
            cpu.regs.sp = cpu.operands[1].value as u16;
            cpu.regs.sp &= 0xFFFE;
        }
        _ => {}
    }
}

pub fn op_lea(cpu: &mut Cpu) {
    cpu.regs.ea = 0;
    if cpu.operands[1].register_size != 0 {
        cpu.regs.ea = cpu.regs.ea.wrapping_add(cpu.operands[1].value as u16);
    }
    if cpu.hint & crate::decode::H_TI != 0 {
        cpu.regs.ea = cpu.regs.ea.wrapping_add(cpu.long_imm);
    }
}

pub fn op_psw_or(cpu: &mut Cpu) {
    cpu.regs.set_psw(cpu.regs.psw() | (cpu.opcode & 0xFF) as u8);
}

pub fn op_psw_and(cpu: &mut Cpu) {
    cpu.regs.set_psw(cpu.regs.psw() & (cpu.opcode & 0xFF) as u8);
}

pub fn op_cplc(cpu: &mut Cpu) {
    cpu.regs.set_psw(cpu.regs.psw() ^ PSW_C);
}

pub fn op_bc(cpu: &mut Cpu) {
    let c = cpu.flags_in & PSW_C != 0;
    let z = cpu.flags_in & PSW_Z != 0;
    let s = cpu.flags_in & PSW_S != 0;
    let ov = cpu.flags_in & PSW_OV != 0;
    let le = z | c;
    let lts = ov ^ s;
    let les = lts | z;

    let branch = match (cpu.opcode >> 8) & 0x000F {
        0 => !c,
        1 => c,
        2 => !le,
        3 => le,
        4 => !lts,
        5 => lts,
        6 => !les,
        7 => les,
        8 => !z,
        9 => z,
        10 => !ov,
        11 => ov,
        12 => !s,
        13 => s,
        _ => true, // 14 = AL, 15 unused-but-unconditional
    };

    if branch {
        let mut disp = cpu.operands[0].value as u16;
        if disp & 0x80 != 0 {
            disp |= 0x7F00;
        }
        cpu.regs.pc = cpu.regs.pc.wrapping_add(disp << 1);
    }
}

pub fn op_swi<S: ControlSink>(cpu: &mut Cpu, sink: &mut S) {
    sink.raise_software(cpu.operands[0].value as u32);
}

pub fn op_brk<S: ControlSink>(_cpu: &mut Cpu, sink: &mut S) {
    sink.break_();
}

pub fn op_b(cpu: &mut Cpu) {
    if cpu.hint & crate::decode::H_TI != 0 {
        cpu.regs.csr = cpu.operands[1].value as u16;
        cpu.regs.pc = cpu.long_imm;
    } else {
        cpu.regs.pc = cpu.operands[1].value as u16;
    }
}

pub fn op_bl(cpu: &mut Cpu) {
    cpu.regs.set_lr(cpu.regs.pc);
    cpu.regs.set_lcsr(cpu.regs.csr);
    op_b(cpu);
}

pub fn op_rt(cpu: &mut Cpu) {
    cpu.regs.csr = cpu.regs.lcsr();
    cpu.regs.pc = cpu.regs.lr();
}

pub fn op_rti(cpu: &mut Cpu) {
    let level = cpu.regs.elevel();
    cpu.regs.csr = cpu.regs.ecsr[level];
    cpu.regs.pc = cpu.regs.elr[level];
    cpu.regs.set_psw(cpu.regs.epsw[level]);
}

pub fn op_nop(_cpu: &mut Cpu) {}

pub fn op_dsr(cpu: &mut Cpu) {
    if cpu.hint & crate::decode::H_DW != 0 {
        cpu.pending_dsr = cpu.operands[0].value as u8;
    }
    cpu.regs.dsr = cpu.pending_dsr;
}

/// The `ControlSink` a bare CPU uses: interrupts go nowhere.
///
/// Useful for unit tests that execute a hand-written program without a chipset.
#[derive(Debug, Default, Clone)]
pub struct NullSink {
    pub software_interrupts: Vec<u32>,
    pub breaks: u32,
}

impl ControlSink for NullSink {
    fn raise_software(&mut self, index: u32) {
        self.software_interrupts.push(index);
    }

    fn break_(&mut self) {
        self.breaks += 1;
    }
}
