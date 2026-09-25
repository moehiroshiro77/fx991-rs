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

//! Table-driven nX-U8 encoder.
//!
//! Every helper here is *derived from the decode table's own operand masks* rather
//! than from a second reading of the encoding chart: `encode` starts from a table
//! row's base opcode and OR-s each slot's value in at the mask/shift the row
//! declares.  A helper therefore cannot disagree with the decoder about where a
//! field lives, and a wrong slot index fails loudly instead of producing a
//! plausible-looking instruction.
//!
//! Which slot a field occupies is not uniform -- `PUSH Rn` keeps its register in
//! operand 1 while `POP Rn` keeps it in operand 0, and `MOV PSW,#imm` puts the
//! immediate in operand 1 -- hence the explicit slot indices.
//!
//! `
//! use nxu8_asm:asm;
//!
//! // `POP PC` is the 0xF08E row with list bit 1 set: the `8E F2` pattern that
//! //  greps for.
//! assert_eq!(asm:pop_pc, [0x8E, 0xF2]);
//! `

#![warn(missing_docs)]

use nxu8_core::decode::{dispatch, H_TI};

/// Why an encoding request could not be satisfied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsmError {
    /// The base opcode does not decode at all.
    UnknownBase(u16),
    /// The base decodes, but to a different handler than the helper expects.
    WrongHandler {
        /// Base opcode that was used.
        base: u16,
        /// The handler it actually decodes to.
        actual: &'static str,
        /// The handler the caller expected.
        expected: &'static str,
    },
    /// A value does not fit its operand field, even as a signed quantity.
    ValueOutOfRange {
        /// Base opcode being encoded.
        base: u16,
        /// Which operand slot.
        slot: usize,
        /// The rejected value.
        value: i64,
        /// The field mask.
        mask: u16,
    },
    /// The row takes a long immediate and none was supplied.
    MissingLongImmediate(u16),
}

impl std::fmt::Display for AsmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AsmError::UnknownBase(base) => {
                write!(f, "opcode base {base:#06x} does not decode")
            }
            AsmError::WrongHandler {
                base,
                actual,
                expected,
            } => write!(
                f,
                "opcode base {base:#06x} decodes to {actual}, expected {expected}"
            ),
            AsmError::ValueOutOfRange {
                base,
                slot,
                value,
                mask,
            } => write!(
                f,
                "operand {slot} of {base:#06x} is {value:#x}, does not fit mask {mask:#x}"
            ),
            AsmError::MissingLongImmediate(base) => {
                write!(f, "opcode base {base:#06x} needs a long immediate")
            }
        }
    }
}

impl std::error::Error for AsmError {}

/// A value to place in an operand slot: `(slot_index, value)`.
///
/// The value may be a register index or a signed immediate; the mask width in the
/// table decides what fits.
pub type Slot = (usize, i64);

/// Encode one instruction from a base opcode, slot values, and an optional long
/// immediate.  This is the only function that touches the table.
pub fn encode(base: u16, slots: &[Slot], long_imm: Option<u16>) -> Result<Vec<u8>, AsmError> {
    encode_expecting(base, slots, long_imm, None)
}

/// Like [`encode`], but assert which handler the base must decode to.
pub fn encode_expecting(
    base: u16,
    slots: &[Slot],
    long_imm: Option<u16>,
    expect: Option<&'static str>,
) -> Result<Vec<u8>, AsmError> {
    let entry = dispatch()[base as usize].ok_or(AsmError::UnknownBase(base))?;

    if let Some(expected) = expect {
        if entry.handler != expected {
            return Err(AsmError::WrongHandler {
                base,
                actual: entry.handler,
                expected,
            });
        }
    }

    let mut word = base;
    for &(slot, value) in slots {
        let (_, mask, shift) = entry.operands[slot];
        let lo = -((mask as i64) + 1);
        if value < lo || value > mask as i64 {
            return Err(AsmError::ValueOutOfRange {
                base,
                slot,
                value,
                mask,
            });
        }
        word |= ((value as u16) & mask) << shift;
    }

    let mut out = word.to_le_bytes().to_vec();
    if entry.hint & H_TI != 0 {
        let immediate = long_imm.ok_or(AsmError::MissingLongImmediate(base))?;
        out.extend_from_slice(&immediate.to_le_bytes());
    }
    Ok(out)
}

/// The halfword of an encoded instruction, for assertions.
pub fn word_of(encoded: &[u8]) -> u16 {
    u16::from_le_bytes([encoded[0], encoded[1]])
}

/// Instruction encoders.
///
/// Names follow the disassembler's mnemonics.  The comment on each function is the
/// operand layout, not a restatement of the mnemonic.
pub mod asm {
    use super::{encode_expecting, Slot};

    fn one(base: u16, slot: usize, value: i64, expect: &'static str) -> Vec<u8> {
        encode_expecting(base, &[(slot, value)], None, Some(expect)).unwrap()
    }

    fn two(base: u16, a: Slot, b: Slot, expect: &'static str) -> Vec<u8> {
        encode_expecting(base, &[a, b], None, Some(expect)).unwrap()
    }

    fn imm(base: u16, value: u16, expect: &'static str) -> Vec<u8> {
        encode_expecting(base, &[], Some(value), Some(expect)).unwrap()
    }

    // ------------------------------------------------------------ data movement
    /// `MOV Rn,#imm8`
    pub fn mov_r_imm(n: u8, imm: u8) -> Vec<u8> {
        two(0x0000, (0, n as i64), (1, imm as i64), "OP_MOV")
    }
    /// `MOV ERn,#imm7` (sign-extended)
    pub fn mov_er_imm(er_index: u8, imm: i64) -> Vec<u8> {
        two(0xE000, (0, er_index as i64), (1, imm), "OP_MOV16")
    }
    /// `MOV ERn,ERm`
    pub fn mov_er_er(n: u8, m: u8) -> Vec<u8> {
        two(0xF005, (0, n as i64), (1, m as i64), "OP_MOV16")
    }
    /// `MOV Rn,Rm`
    pub fn mov_r_r(n: u8, m: u8) -> Vec<u8> {
        two(0x8000, (0, n as i64), (1, m as i64), "OP_MOV")
    }
    /// `MOV PSW,#imm8` -- the immediate is operand 1 of the CTRL row
    pub fn mov_psw_imm(imm: u8) -> Vec<u8> {
        one(0xE900, 1, imm as i64, "OP_CTRL")
    }
    /// `MOV Rn,PSW` -- OP_CTRL selector 10, register in operand 0
    pub fn mov_r_psw(n: u8) -> Vec<u8> {
        one(0xA003, 0, n as i64, "OP_CTRL")
    }
    /// `MOV SP,ERn` -- OP_CTRL selector 11, register in operand 1
    pub fn mov_sp_er(er_index: u8) -> Vec<u8> {
        one(0xA10A, 1, er_index as i64, "OP_CTRL")
    }
    /// `MOV ERn,SP` -- OP_CTRL selector 5, register in operand 0
    pub fn mov_er_sp(er_index: u8) -> Vec<u8> {
        one(0xA01A, 0, er_index as i64, "OP_CTRL")
    }

    // ------------------------------------------------------------------ memory
    /// `L Rn,[EA]`
    pub fn l_r_ea(n: u8) -> Vec<u8> {
        one(0x9030, 0, n as i64, "OP_LS_EA")
    }
    /// `ST Rn,[EA]`
    pub fn st_r_ea(n: u8) -> Vec<u8> {
        one(0x9031, 0, n as i64, "OP_LS_EA")
    }
    /// `L Rn,[EA+]`
    pub fn l_r_ea_inc(n: u8) -> Vec<u8> {
        one(0x9050, 0, n as i64, "OP_LS_EA")
    }
    /// `ST Rn,[EA+]`
    pub fn st_r_ea_inc(n: u8) -> Vec<u8> {
        one(0x9051, 0, n as i64, "OP_LS_EA")
    }
    /// `L ERn,[EA]`
    pub fn l_er_ea(er_index: u8) -> Vec<u8> {
        one(0x9032, 0, er_index as i64, "OP_LS_EA")
    }
    /// `ST ERn,[EA]`
    pub fn st_er_ea(er_index: u8) -> Vec<u8> {
        one(0x9033, 0, er_index as i64, "OP_LS_EA")
    }
    /// `L Rn,[ERm]`
    pub fn l_r_er(n: u8, m: u8) -> Vec<u8> {
        two(0x9000, (0, n as i64), (1, m as i64), "OP_LS_R")
    }
    /// `ST Rn,[ERm]`
    pub fn st_r_er(n: u8, m: u8) -> Vec<u8> {
        two(0x9001, (0, n as i64), (1, m as i64), "OP_LS_R")
    }
    /// `L ERn,[ERm]`
    pub fn l_er_er(n: u8, m: u8) -> Vec<u8> {
        two(0x9002, (0, n as i64), (1, m as i64), "OP_LS_R")
    }
    /// `ST ERn,[ERm]`
    pub fn st_er_er(n: u8, m: u8) -> Vec<u8> {
        two(0x9003, (0, n as i64), (1, m as i64), "OP_LS_R")
    }
    /// `LEA [ERn]` -- the pointer register is operand 1
    pub fn lea_ea(er_index: u8) -> Vec<u8> {
        one(0xF00A, 1, er_index as i64, "OP_LEA")
    }
    /// `LEA Dadr`
    pub fn lea_dadr(address: u16) -> Vec<u8> {
        imm(0xF00C, address, "OP_LEA")
    }
    /// `L Rn,Dadr`
    pub fn l_r_dadr(n: u8, address: u16) -> Vec<u8> {
        encode_expecting(0x9010, &[(0, n as i64)], Some(address), Some("OP_LS_I")).unwrap()
    }
    /// `ST Rn,Dadr`
    pub fn st_r_dadr(n: u8, address: u16) -> Vec<u8> {
        encode_expecting(0x9011, &[(0, n as i64)], Some(address), Some("OP_LS_I")).unwrap()
    }
    /// `L ERn,Dadr`
    pub fn l_er_dadr(er_index: u8, address: u16) -> Vec<u8> {
        encode_expecting(
            0x9012,
            &[(0, er_index as i64)],
            Some(address),
            Some("OP_LS_I"),
        )
        .unwrap()
    }
    /// `ST ERn,Dadr`
    pub fn st_er_dadr(er_index: u8, address: u16) -> Vec<u8> {
        encode_expecting(
            0x9013,
            &[(0, er_index as i64)],
            Some(address),
            Some("OP_LS_I"),
        )
        .unwrap()
    }

    // -------------------------------------------------------------- arithmetic
    /// `ADD Rn,#imm8`
    pub fn add_r_imm(n: u8, imm: u8) -> Vec<u8> {
        two(0x1000, (0, n as i64), (1, imm as i64), "OP_ADD")
    }
    /// `ADD Rn,Rm`
    pub fn add_r_r(n: u8, m: u8) -> Vec<u8> {
        two(0x8001, (0, n as i64), (1, m as i64), "OP_ADD")
    }
    /// `ADD ERn,#imm7` (sign-extended)
    pub fn add_er_imm(er_index: u8, imm: i64) -> Vec<u8> {
        two(0xE080, (0, er_index as i64), (1, imm), "OP_ADD16")
    }
    /// `ADDC Rn,#imm8`
    pub fn addc_r_imm(n: u8, imm: u8) -> Vec<u8> {
        two(0x6000, (0, n as i64), (1, imm as i64), "OP_ADDC")
    }
    /// `SUB Rn,#imm8`
    pub fn sub_r_imm(n: u8, imm: u8) -> Vec<u8> {
        two(0x7000, (0, n as i64), (1, imm as i64), "OP_SUB")
    }
    /// `CMP Rn,Rm` -- the hint-less row, so Rn survives
    pub fn cmp_r_r(n: u8, m: u8) -> Vec<u8> {
        two(0x8007, (0, n as i64), (1, m as i64), "OP_SUB")
    }
    /// `SUB Rn,Rm` -- the writeback row, so the difference lands in Rn
    pub fn sub_r_r(n: u8, m: u8) -> Vec<u8> {
        two(0x8008, (0, n as i64), (1, m as i64), "OP_SUB")
    }
    /// `AND Rn,Rm`
    pub fn and_r_r(n: u8, m: u8) -> Vec<u8> {
        two(0x8002, (0, n as i64), (1, m as i64), "OP_AND")
    }
    /// `OR Rn,Rm`
    pub fn or_r_r(n: u8, m: u8) -> Vec<u8> {
        two(0x8003, (0, n as i64), (1, m as i64), "OP_OR")
    }
    /// `XOR Rn,Rm`
    pub fn xor_r_r(n: u8, m: u8) -> Vec<u8> {
        two(0x8004, (0, n as i64), (1, m as i64), "OP_XOR")
    }
    /// `MUL ERn,Rm`
    pub fn mul_er_r(er_index: u8, m: u8) -> Vec<u8> {
        two(0xF004, (0, er_index as i64), (1, m as i64), "OP_MUL")
    }
    /// `DIV ERn,Rm`
    pub fn div_er_r(er_index: u8, m: u8) -> Vec<u8> {
        two(0xF009, (0, er_index as i64), (1, m as i64), "OP_DIV")
    }
    /// `ADD SP,#imm8` (sign-extended, then `SP &= 0xFFFE`)
    pub fn add_sp(imm: i64) -> Vec<u8> {
        one(0xE100, 0, imm, "OP_ADDSP")
    }

    // ------------------------------------------------------------------ shifts
    /// `SLL Rn,#w`
    pub fn sll_r_imm(n: u8, count: u8) -> Vec<u8> {
        two(0x900A, (0, n as i64), (1, count as i64), "OP_SLL")
    }
    /// `SRL Rn,#w`
    pub fn srl_r_imm(n: u8, count: u8) -> Vec<u8> {
        two(0x900C, (0, n as i64), (1, count as i64), "OP_SRL")
    }
    /// `SRLC Rn,#w`
    pub fn srlc_r_imm(n: u8, count: u8) -> Vec<u8> {
        two(0x900D, (0, n as i64), (1, count as i64), "OP_SRLC")
    }
    /// `SRA Rn,#w`
    pub fn sra_r_imm(n: u8, count: u8) -> Vec<u8> {
        two(0x900E, (0, n as i64), (1, count as i64), "OP_SRA")
    }

    // -------------------------------------------------------------------- bits
    /// `SB Rn.bit`
    pub fn sb(n: u8, bit: u8) -> Vec<u8> {
        two(0xA000, (0, n as i64), (1, bit as i64), "OP_BITMOD")
    }
    /// `TB Rn.bit`
    pub fn tb(n: u8, bit: u8) -> Vec<u8> {
        two(0xA001, (0, n as i64), (1, bit as i64), "OP_BITMOD")
    }
    /// `RB Rn.bit`
    pub fn rb(n: u8, bit: u8) -> Vec<u8> {
        two(0xA002, (0, n as i64), (1, bit as i64), "OP_BITMOD")
    }
    /// `SB Dadr.bit` -- 4 bytes
    pub fn sb_mem(address: u16, bit: u8) -> Vec<u8> {
        encode_expecting(0xA080, &[(1, bit as i64)], Some(address), Some("OP_BITMOD")).unwrap()
    }
    /// `TB Dadr.bit` -- 4 bytes
    pub fn tb_mem(address: u16, bit: u8) -> Vec<u8> {
        encode_expecting(0xA081, &[(1, bit as i64)], Some(address), Some("OP_BITMOD")).unwrap()
    }
    /// `RB Dadr.bit` -- 4 bytes
    pub fn rb_mem(address: u16, bit: u8) -> Vec<u8> {
        encode_expecting(0xA082, &[(1, bit as i64)], Some(address), Some("OP_BITMOD")).unwrap()
    }

    // ------------------------------------------------------------- control flow
    /// Condition codes for `BC` (manual chapter 4).
    pub mod cond {
        /// `GE`
        pub const GE: u8 = 0;
        /// `LT`
        pub const LT: u8 = 1;
        /// `GT`
        pub const GT: u8 = 2;
        /// `LE`
        pub const LE: u8 = 3;
        /// `GES`
        pub const GES: u8 = 4;
        /// `LTS`
        pub const LTS: u8 = 5;
        /// `GTS`
        pub const GTS: u8 = 6;
        /// `LES`
        pub const LES: u8 = 7;
        /// `NE`
        pub const NE: u8 = 8;
        /// `EQ`
        pub const EQ: u8 = 9;
        /// `NV`
        pub const NV: u8 = 10;
        /// `OV`
        pub const OV: u8 = 11;
        /// `PS`
        pub const PS: u8 = 12;
        /// `NS`
        pub const NS: u8 = 13;
        /// `AL` -- unconditional
        pub const AL: u8 = 14;
    }

    /// `BC cond,radr` -- signed 8-bit **word** displacement from the next PC
    pub fn bc(condition: u8, displacement_words: i64) -> Vec<u8> {
        encode_expecting(
            0xC000 | (((condition & 0xF) as u16) << 8),
            &[(0, displacement_words)],
            None,
            Some("OP_BC"),
        )
        .unwrap()
    }

    /// `B Csr::Adr`
    pub fn b(csr: u8, address: u16) -> Vec<u8> {
        encode_expecting(
            0xF000 | (((csr & 0xF) as u16) << 8),
            &[],
            Some(address),
            Some("OP_B"),
        )
        .unwrap()
    }
    /// `BL Csr::Adr`
    pub fn bl(csr: u8, address: u16) -> Vec<u8> {
        encode_expecting(
            0xF001 | (((csr & 0xF) as u16) << 8),
            &[],
            Some(address),
            Some("OP_BL"),
        )
        .unwrap()
    }
    /// `B ERn` -- indirect, the register is operand 1
    pub fn b_er(er_index: u8) -> Vec<u8> {
        one(0xF002, 1, er_index as i64, "OP_B")
    }
    /// `BL ERn` -- indirect
    pub fn bl_er(er_index: u8) -> Vec<u8> {
        one(0xF003, 1, er_index as i64, "OP_BL")
    }

    // ------------------------------------------------------------------- stack
    /// `PUSH Rn`
    pub fn push_r(n: u8) -> Vec<u8> {
        one(0xF04E, 1, n as i64, "OP_PUSH")
    }
    /// `PUSH ERn`
    pub fn push_er(er_index: u8) -> Vec<u8> {
        one(0xF05E, 1, er_index as i64, "OP_PUSH")
    }
    /// `PUSH XRn` -- 0, 4, 8 or 12
    pub fn push_xr(index: u8) -> Vec<u8> {
        one(0xF06E, 1, index as i64, "OP_PUSH")
    }
    /// `PUSH QRn` -- 0 or 8
    pub fn push_qr(index: u8) -> Vec<u8> {
        one(0xF07E, 1, index as i64, "OP_PUSH")
    }
    /// `POP Rn`
    pub fn pop_r(n: u8) -> Vec<u8> {
        one(0xF00E, 0, n as i64, "OP_POP")
    }
    /// `POP ERn`
    pub fn pop_er(er_index: u8) -> Vec<u8> {
        one(0xF01E, 0, er_index as i64, "OP_POP")
    }
    /// `POP XRn`
    pub fn pop_xr(index: u8) -> Vec<u8> {
        one(0xF02E, 0, index as i64, "OP_POP")
    }
    /// `POP QRn`
    pub fn pop_qr(index: u8) -> Vec<u8> {
        one(0xF03E, 0, index as i64, "OP_POP")
    }
    /// `PUSH register_list` (bit1 ELR, bit2 EPSW, bit3 LR, bit0 EA)
    pub fn push_list(mask: u8) -> Vec<u8> {
        one(0xF0CE, 1, mask as i64, "OP_PUSHL")
    }
    /// `POP register_list` (bit0 EA, bit1 PC, bit2 PSW, bit3 LR)
    pub fn pop_list(mask: u8) -> Vec<u8> {
        one(0xF08E, 0, mask as i64, "OP_POPL")
    }

    /// `POP PC` -- the terminator of every ROP gadget in the ROM
    pub fn pop_pc() -> [u8; 2] {
        [0x8E, 0xF2]
    }
    /// `PUSH LR`
    pub fn push_lr() -> [u8; 2] {
        [0xCE, 0xF8]
    }
    /// `POP LR`
    pub fn pop_lr() -> [u8; 2] {
        [0x8E, 0xF8]
    }

    // ----------------------------------------------------------- miscellaneous
    /// `RT`
    pub const RT: [u8; 2] = [0x1F, 0xFE];
    /// `RTI`
    pub const RTI: [u8; 2] = [0x0F, 0xFE];
    /// `NOP`
    pub const NOP: [u8; 2] = [0x8F, 0xFE];
    /// `BRK`
    pub const BRK: [u8; 2] = [0xFF, 0xFF];
    /// `INC [EA]`
    pub const INC_EA: [u8; 2] = [0x2F, 0xFE];
    /// `DEC [EA]`
    pub const DEC_EA: [u8; 2] = [0x3F, 0xFE];

    /// `SWI #i6`
    pub fn swi(index: u8) -> Vec<u8> {
        one(0xE500, 0, index as i64, "OP_SWI")
    }
    /// `EI`
    pub fn ei() -> Vec<u8> {
        encode_expecting(0xED08, &[], None, Some("OP_PSW_OR")).unwrap()
    }
    /// `DI`
    pub fn di() -> Vec<u8> {
        encode_expecting(0xEBF7, &[], None, Some("OP_PSW_AND")).unwrap()
    }
    /// `CPLC`
    pub fn cplc() -> Vec<u8> {
        encode_expecting(0xFECF, &[], None, Some("OP_CPLC")).unwrap()
    }
    /// `DSR<- #imm8` -- a prefix; the next instruction sees the new DSR
    pub fn dsr_prefix_imm(value: u8) -> Vec<u8> {
        one(0xE300, 0, value as i64, "OP_DSR")
    }

    /// Word displacement that lands `target` from the address after a `BC` at `pc`.
    pub fn bc_displacement(pc: u16, target: u16) -> i64 {
        ((target as i64) - (pc as i64) - 2) / 2
    }

    /// Encode a `BC` that jumps from `pc` to `target`.
    pub fn bc_to(pc: u16, target: u16, condition: u8) -> Vec<u8> {
        bc(condition, bc_displacement(pc, target))
    }

    // The alias is part of the public story even though no helper needs the type
    // name directly.
    #[allow(dead_code)]
    fn _slot_alias(slot: Slot) -> Slot {
        slot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pop_pc_encodes_to_the_pattern_the_scanner_greps() {
        //  searches for `1E F? 8E F2`, `2E F? 8E F2`,
        // `3E F? 8E F2`.  If this ever changes, the two tools disagree.
        assert_eq!(asm::pop_er(0), [0x1E, 0xF0]);
        assert_eq!(asm::pop_er(2), [0x1E, 0xF2]);
        assert_eq!(asm::pop_xr(0), [0x2E, 0xF0]);
        assert_eq!(asm::pop_qr(0), [0x3E, 0xF0]);
        assert_eq!(asm::pop_pc(), [0x8E, 0xF2]);
        assert_eq!(asm::push_lr(), [0xCE, 0xF8]);

        let chain: Vec<u8> = [asm::pop_er(2).as_slice(), &asm::pop_pc()].concat();
        assert_eq!(chain, [0x1E, 0xF2, 0x8E, 0xF2]);
    }

    #[test]
    fn list_forms_encode_from_the_list_mask() {
        assert_eq!(word_of(&asm::pop_list(2)), 0xF28E);
        assert_eq!(word_of(&asm::push_list(8)), 0xF8CE);
    }

    #[test]
    fn immediates_land_in_the_low_byte() {
        assert_eq!(asm::mov_r_imm(0, 1), [0x01, 0x00]);
        assert_eq!(asm::mov_r_imm(0, 0xFF), [0xFF, 0x00]);
        // The register index occupies opcode bits 8-11, i.e. the high nibble of
        // the second byte.  Check against the listing: `MOV R9,#8` is `08 09`.
        assert_eq!(asm::mov_r_imm(3, 1), [0x01, 0x03]);
        assert_eq!(asm::mov_r_imm(9, 8), [0x08, 0x09]);
    }

    #[test]
    fn long_immediates_are_appended_little_endian() {
        assert_eq!(asm::lea_dadr(0xD180), [0x0C, 0xF0, 0x80, 0xD1]);
        assert_eq!(asm::b(0, 0x946A), [0x00, 0xF0, 0x6A, 0x94]);
        assert_eq!(asm::b(2, 0x21B8), [0x00, 0xF2, 0xB8, 0x21]);
    }

    #[test]
    fn opcode_lengths_match_the_listing() {
        assert_eq!(asm::mov_r_imm(0, 1).len(), 2);
        assert_eq!(asm::lea_dadr(0).len(), 4);
        assert_eq!(asm::pop_pc().len(), 2);
    }

    #[test]
    fn the_branch_displacement_is_relative_to_the_already_advanced_pc() {
        // `OP_BC` adds `disp << 1` to the PC that has already moved past the
        // two-byte instruction, so target = pc + 2 + disp*2.
        assert_eq!(asm::bc_displacement(0x1000, 0x1000), -1);
        assert_eq!(asm::bc_displacement(0x1000, 0x0FFE), -2);
        assert_eq!(asm::bc_displacement(0x1000, 0x1004), 1);

        // The encoding of -2 is the two's complement byte FE.
        assert_eq!(asm::bc(asm::cond::AL, -2), [0xFE, 0xCE]);
        assert_eq!(word_of(&asm::bc(asm::cond::AL, -2)), 0xCEFE);
    }

    #[test]
    fn the_wrong_slot_index_is_caught_by_the_handler_check() {
        // `PUSH Rn` keeps its register in operand 1; using slot 0 would silently
        // encode a completely different instruction.
        let wrong = encode_expecting(0xF04E, &[(0, 3)], None, Some("OP_PUSH"));
        assert!(matches!(wrong, Err(AsmError::ValueOutOfRange { .. })));
    }

    #[test]
    fn an_out_of_range_immediate_is_rejected_rather_than_truncated() {
        let err = encode(0x0000, &[(0, 0), (1, 0x1FF)], None).unwrap_err();
        match err {
            AsmError::ValueOutOfRange { value, mask, .. } => {
                assert_eq!(value, 0x1FF);
                assert_eq!(mask, 0x00FF);
            }
            other => panic!("expected a range error, got {other:?}"),
        }
    }

    #[test]
    fn an_immediate_may_be_unsigned_or_two_s_complement_of_the_same_width() {
        // The table records only a field width, not a signedness, so the rule is
        // "fits in `mask` bits either way": 0..=mask, or -(mask+1)..=-1.  `ADD
        // SP,#-3` relies on it.
        assert_eq!(encode(0xE100, &[(0, -3)], None).unwrap(), [0xFD, 0xE1]);
        assert_eq!(encode(0xE100, &[(0, 253)], None).unwrap(), [0xFD, 0xE1]);
        assert!(encode(0xE100, &[(0, 255)], None).is_ok());
        assert!(encode(0xE100, &[(0, 256)], None).is_err());
        assert!(encode(0xE100, &[(0, -256)], None).is_ok());
        assert!(encode(0xE100, &[(0, -257)], None).is_err());
    }

    #[test]
    fn a_missing_long_immediate_is_an_error_not_a_silent_two_byte_instruction() {
        assert_eq!(
            encode(0xF00C, &[], None),
            Err(AsmError::MissingLongImmediate(0xF00C))
        );
    }

    #[test]
    fn an_undecodable_base_is_rejected() {
        // 0x800F is not in the table.
        assert!(matches!(
            encode(0x800F, &[], None),
            Err(AsmError::UnknownBase(0x800F))
        ));
    }

    #[test]
    fn a_wrong_handler_expectation_is_reported() {
        let err = encode_expecting(0x0000, &[(0, 0), (1, 0)], None, Some("OP_ADD")).unwrap_err();
        match err {
            AsmError::WrongHandler {
                actual, expected, ..
            } => {
                assert_eq!(actual, "OP_MOV");
                assert_eq!(expected, "OP_ADD");
            }
            other => panic!("expected a handler mismatch, got {other:?}"),
        }
    }
}
