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

//! The CPU: instruction state and `Next`.
//!
//! The CPU's instruction dispatch and interrupt plumbing.  The plumbing that all handlers rely
//! on lives here:
//!
//! * `fetch` masks CSR with `csr_mask` and clears PC bit 0, and PC advances
//!   **before** the handler runs -- so `BL` and `BC` compute their targets from the
//!   already advanced PC.
//! * `H_TI` fetches a second halfword into `long_imm` up front.
//! * Operands are decoded from the opcode bits, then, when `register_size` is
//!   non-zero, replaced by the little-endian contents of that register bank.
//! * `flags_out` is seeded with `PSW_Z` and merged back through `flags_changed`
//!   after the handler returns.
//! * `H_WB` writes operand 0 back to its byte registers.
//! * `H_DS` loops: a DSR prefix decodes a further instruction.

use crate::decode::{dispatch, H_DS, H_TI, H_WB};
use crate::ops::{self, ctrl::ControlSink};
use crate::regs::{Regs, PSW_Z};
use crate::{Memory, Unimplemented};

/// One decoded operand slot.
#[derive(Debug, Clone, Copy, Default)]
pub struct Operand {
    /// Register contents (for a register operand) or the masked opcode bits.
    pub value: u64,
    /// Byte-register index the operand aliases.
    pub register_index: usize,
    /// Width in bytes; 0 means the operand is an immediate.
    pub register_size: usize,
}

/// The processor.
#[derive(Debug, Clone)]
pub struct Cpu {
    /// The register file.
    pub regs: Regs,
    /// `csr_mask` from the model file; 0x000F for fx-991CN X.
    pub csr_mask: u16,
    /// `CPU::MM_LARGE`, which makes POP/PUSH LR move CSR too.
    pub memory_model_large: bool,

    /// The two decoded operand slots.
    pub operands: [Operand; 2],
    /// Hint flags of the instruction being executed.
    pub hint: u32,
    /// The opcode being executed.
    pub opcode: u16,
    /// The long immediate, when `H_TI` is set.
    pub long_imm: u16,
    /// Shift-in byte used by `SLLC`/`SRLC`/`SRA`/`SRL`.
    pub shift_buffer: u8,
    /// PSW as snapshotted before the handler ran.
    pub flags_in: u8,
    /// Flags the handler produced.
    pub flags_out: u8,
    /// Which flag bits the handler is allowed to change.
    pub flags_changed: u8,
    /// A `DSR<-` prefix sets this; only the following instruction sees it.
    pub pending_dsr: u8,
    /// Set when a handler refuses to execute an opcode.
    pub unimplemented: Option<Unimplemented>,

    /// Instructions retired.
    pub ticks: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// A CPU in its reset-adjacent state: all registers clear.
    pub fn new() -> Self {
        Self {
            regs: Regs::new(),
            csr_mask: 0x000F,
            memory_model_large: true,
            operands: [Operand::default(); 2],
            hint: 0,
            opcode: 0,
            long_imm: 0,
            shift_buffer: 0,
            flags_in: 0,
            flags_out: 0,
            flags_changed: 0,
            pending_dsr: 0,
            unimplemented: None,
            ticks: 0,
        }
    }

    /// Fetch a halfword and advance PC.
    fn fetch<B: Memory>(&mut self, bus: &mut B) -> u16 {
        if self.regs.csr & !self.csr_mask != 0 {
            self.regs.csr &= self.csr_mask;
        }
        if self.regs.pc & 1 != 0 {
            self.regs.pc &= !1;
        }
        let opcode = bus.read_code(self.regs.physical_pc());
        self.regs.pc = self.regs.pc.wrapping_add(2);
        opcode
    }

    /// Execute one instruction.  Returns the handler name, or `None` when the
    /// opcode was unmapped and simply skipped.
    pub fn step<B: Memory, S: ControlSink>(
        &mut self,
        bus: &mut B,
        sink: &mut S,
    ) -> Option<&'static str> {
        // `reg_dsr` only affects the current instruction.
        self.regs.dsr = 0;

        let mut executed;
        loop {
            self.opcode = self.fetch(bus);
            let entry = match dispatch()[self.opcode as usize] {
                Some(entry) => entry,
                None => {
                    self.ticks += 1;
                    return None;
                }
            };

            self.long_imm = if entry.hint & H_TI != 0 {
                self.fetch(bus)
            } else {
                0
            };

            for ix in 0..2 {
                let (size, mask, shift) = entry.operands[ix];
                let op = &mut self.operands[ix];
                op.value = ((self.opcode >> shift) & mask) as u64;
                op.register_index = op.value as usize;
                op.register_size = size as usize;
                if size != 0 {
                    op.value = self.regs.reg(op.register_index, size as usize);
                }
            }

            self.hint = entry.hint;
            self.flags_changed = 0;
            self.flags_in = self.regs.psw();
            self.flags_out = PSW_Z;

            if !ops::run(self, bus, sink, entry.handler) {
                self.ticks += 1;
                return None;
            }
            executed = entry.handler;

            self.regs.set_psw(
                (self.regs.psw() & !self.flags_changed) | (self.flags_out & self.flags_changed),
            );

            if entry.hint & H_WB != 0 && self.operands[0].register_size != 0 {
                let op = self.operands[0];
                self.regs
                    .set_reg(op.register_index, op.register_size, op.value);
            }

            if entry.hint & H_DS == 0 {
                break;
            }
            // A DSR prefix decodes a further instruction, so the loop assigns
            // `executed` again on purpose.
        }

        self.ticks += 1;
        Some(executed)
    }

    /// `CPU::Reset`: SP comes from the word at ROM offset 0.
    pub fn reset<B: Memory>(&mut self, bus: &mut B) {
        self.regs.sp = bus.read_code(0);
        self.regs.dsr = 0;
        self.regs.set_psw(0);
        self.pending_dsr = 0;
        self.unimplemented = None;
        self.ticks = 0;
    }

    /// `CPU::Raise`.
    pub fn raise_interrupt<B: Memory>(&mut self, bus: &mut B, exception_level: u8, index: u32) {
        if exception_level == 1 {
            self.regs.set_psw(self.regs.psw() & !crate::regs::PSW_MIE);
        }
        let psw = (self.regs.psw() & !crate::regs::PSW_ELEVEL)
            | (exception_level & crate::regs::PSW_ELEVEL);
        self.regs.set_psw(psw);

        let level = self.regs.elevel();
        self.regs.elr[level] = self.regs.pc;
        self.regs.ecsr[level] = self.regs.csr;

        self.regs.csr = 0;
        self.regs.pc = bus.read_code(index * 2);
    }

    /// `PSW & PSW_ELEVEL`.
    #[inline]
    pub fn exception_level(&self) -> usize {
        self.regs.elevel()
    }

    /// Whether `PSW.MIE` is set, i.e. whether a maskable interrupt may be taken.
    #[inline]
    pub fn master_interrupt_enable(&self) -> bool {
        self.regs.psw() & crate::regs::PSW_MIE != 0
    }
}
