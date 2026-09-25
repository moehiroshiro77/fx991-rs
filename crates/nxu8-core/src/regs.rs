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

//! nX-U8 register file.
//!
//! The register file and its PSW bit fields.
//! The core is little-endian and the register banks alias, so a single 16-byte
//! array covers every general-purpose register:
//!
//! `text
//! QR0 = {XR0, XR4} = {ER0, ER2, ER4, ER6} = R0.R7      (8 bytes)
//! QR8 = {XR8, XR12} = {ER8, ER10, ER12, ER14} = R8.R15
//!
//! ER0 = {R0, R1}  with the *high* byte in the higher-numbered register
//! ER12/ER13 = BP        ER14/ER15 = FP
//! `
//!
//! `lr`/`lcsr`/`psw` are element 0 of their ELEVEL-indexed banks, matching the
//! `reg_elr[0]`-style aliases in.

/// Carry / borrow.
pub const PSW_C: u8 = 0x80;
/// Zero.
pub const PSW_Z: u8 = 0x40;
/// Sign (bit 7 of the result).
pub const PSW_S: u8 = 0x20;
/// Signed overflow.
pub const PSW_OV: u8 = 0x10;
/// Master interrupt enable.
pub const PSW_MIE: u8 = 0x08;
/// Half carry (BCD).
pub const PSW_HC: u8 = 0x04;
/// Exception level mask, 0.=3.
pub const PSW_ELEVEL: u8 = 0x03;

/// General-purpose and control registers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Regs {
    /// The 16 byte registers; every wider register aliases into this.
    pub r: [u8; 16],
    /// Program counter within the current code segment.
    pub pc: u16,
    /// Code segment register; the physical address is `(csr << 16) | pc`.
    pub csr: u16,
    /// Exception link register, indexed by exception level.
    pub elr: [u16; 4],
    /// Exception code segment register, indexed by exception level.
    pub ecsr: [u16; 4],
    /// Exception program status word, indexed by exception level.
    pub epsw: [u8; 4],
    /// Stack pointer; always even, and the reset value is the word at ROM 0.
    pub sp: u16,
    /// Effective address, used by the `[EA]`/`[EA+]` access forms.
    pub ea: u16,
    /// Data segment register; only the instruction after a `DSR<-` prefix sees it.
    pub dsr: u8,
    /// The DSR carried by a `DSR<-` prefix, in effect for one instruction.
    pub last_dsr: u8,
}

impl Regs {
    /// A register file with everything zeroed, as after reset.
    pub fn new() -> Self {
        Self::default()
    }

    // --- ELEVEL-indexed aliases ---------------------------
    #[inline]
    /// LR, i.e. ELR for the current exception level.
    pub fn lr(&self) -> u16 {
        self.elr[0]
    }

    #[inline]
    /// Set LR.
    pub fn set_lr(&mut self, value: u16) {
        self.elr[0] = value;
    }

    #[inline]
    /// LCSR, i.e. ECSR for the current exception level.
    pub fn lcsr(&self) -> u16 {
        self.ecsr[0]
    }

    #[inline]
    /// Set LCSR.
    pub fn set_lcsr(&mut self, value: u16) {
        self.ecsr[0] = value;
    }

    #[inline]
    /// PSW, i.e. EPSW for the current exception level.
    pub fn psw(&self) -> u8 {
        self.epsw[0]
    }

    #[inline]
    /// Set PSW.
    pub fn set_psw(&mut self, value: u8) {
        self.epsw[0] = value;
    }

    /// `PSW & PSW_ELEVEL`, used as a bank index.
    #[inline]
    pub fn elevel(&self) -> usize {
        (self.epsw[0] & PSW_ELEVEL) as usize
    }

    // --- 16-bit register accessors -----------------------------------------
    #[inline]
    /// Read the even/odd byte pair starting at `index` as a 16-bit register.
    pub fn er(&self, index: usize) -> u16 {
        u16::from_le_bytes([self.r[index & 0xF], self.r[(index + 1) & 0xF]])
    }

    #[inline]
    /// Write a 16-bit register as two bytes, low byte first.
    pub fn set_er(&mut self, index: usize, value: u16) {
        let bytes = value.to_le_bytes();
        self.r[index & 0xF] = bytes[0];
        self.r[(index + 1) & 0xF] = bytes[1];
    }

    /// Read `size` bytes starting at byte register `index`, little-endian.
    ///
    /// `size == 0` means "immediate operand" and is handled by the caller.
    #[inline]
    pub fn reg(&self, index: usize, size: usize) -> u64 {
        let mut value = 0u64;
        for bx in 0..size {
            value |= (self.r[(index + bx) & 0xF] as u64) << (bx * 8);
        }
        value
    }

    #[inline]
    /// Write `size` bytes starting at byte register `index`, little-endian.
    pub fn set_reg(&mut self, index: usize, size: usize, value: u64) {
        for bx in 0..size {
            self.r[(index + bx) & 0xF] = (value >> (bx * 8)) as u8;
        }
    }

    /// The 20-bit physical program address, `(CSR << 16) | PC`.
    #[inline]
    pub fn physical_pc(&self) -> u32 {
        ((self.csr as u32) << 16) | self.pc as u32
    }

    /// A 16-bit XR value: `ERn | (ERn+2 << 16)`.
    #[inline]
    pub fn xr(&self, index: usize) -> u32 {
        (self.er(index) as u32) | ((self.er(index + 2) as u32) << 16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn er_aliases_two_byte_registers_low_first() {
        let mut regs = Regs::new();
        regs.set_er(0, 0xBEEF);
        assert_eq!(regs.r[0], 0xEF);
        assert_eq!(regs.r[1], 0xBE);
        assert_eq!(regs.er(0), 0xBEEF);
    }

    #[test]
    fn wider_registers_are_contiguous_banks() {
        let mut regs = Regs::new();
        regs.set_reg(0, 4, 0xDEAD_BEEF);
        assert_eq!(regs.r[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
        assert_eq!(regs.er(0), 0xBEEF);
        assert_eq!(regs.er(2), 0xDEAD);
        assert_eq!(regs.xr(0), 0xDEAD_BEEF);
    }

    #[test]
    fn register_indices_wrap_modulo_sixteen() {
        let mut regs = Regs::new();
        regs.set_er(14, 0x1234);
        // ER14 is R14/R15; a 4-byte read at 14 wraps into R0/R1.
        assert_eq!(regs.reg(14, 4) & 0xFFFF, 0x1234);
        assert_eq!(regs.reg(14, 4) >> 32, 0);
    }

    #[test]
    fn physical_pc_combines_csr_and_pc() {
        let mut regs = Regs::new();
        regs.csr = 2;
        regs.pc = 0x21B8;
        assert_eq!(regs.physical_pc(), 0x2_21B8);
    }

    #[test]
    fn elevel_indexes_the_banks() {
        let mut regs = Regs::new();
        assert_eq!(regs.elevel(), 0);
        regs.set_psw(2);
        regs.epsw[2] = 0x55;
        regs.elr[2] = 0x1234;
        assert_eq!(regs.elevel(), 2);
        assert_eq!(regs.elr[regs.elevel()], 0x1234);
        // The level-0 aliases still point at bank 0.
        assert_eq!(regs.psw(), 2);
    }
}
