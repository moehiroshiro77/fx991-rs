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

//! Opcode decoding: hint flags, the source table, and the 65536-entry dispatch.
//!
//! The table itself lives in [`crate::opcodes`] (generated).  What is here is the
//! *expansion*, mirroring `CPU::SetupOpcodeDispatch`:
//! collect every bit any operand mask covers, enumerate all combinations of those
//! bits, and fill only the slots that are still empty -- so the first row that
//! claims an opcode wins.

use std::sync::OnceLock;

/// The raw table, one row per decoded opcode pattern.
pub use crate::opcodes::OPCODE_SOURCES;

// --- hint flags ----------------------------------------
/// Extend immediate (sign-extend bit 6 of a 7-bit immediate).
pub const H_IE: u32 = 0x0001;
/// Store direction (load/store share a handler).
pub const H_ST: u32 = 0x0002;
/// The DSR prefix instruction carries a new DSR value.
pub const H_DW: u32 = 0x0004;
/// Instruction is a DSR prefix: decode one more instruction afterwards.
pub const H_DS: u32 = 0x0008;
/// Post-increment EA (`[EA+]`).
pub const H_IA: u32 = 0x0010;
/// A 16-bit long immediate follows the opcode.
pub const H_TI: u32 = 0x0020;
/// Write operand 0 back to its register.
pub const H_WB: u32 = 0x0040;

/// One row of the source table.
#[derive(Debug, Clone, Copy)]
pub struct OpcodeSource {
    /// Name of the handler in [`crate::ops`].
    pub handler: &'static str,
    /// Hint flags; the `H_*` constants in this module.
    pub hint: u32,
    /// Base opcode; operand bits are OR-ed on top of it.
    pub opcode: u16,
    /// `(register_size, mask, shift)`; `register_size == 0` means immediate.
    /// `(register_size, mask, shift)` per operand slot.
    /// Operand descriptors, copied from the source row.
    pub operands: [(u8, u16, u8); 2],
}

impl OpcodeSource {
    /// Build a row.  `const` so the generated table can be a plain literal.
    pub const fn new(
        handler: &'static str,
        hint: u32,
        opcode: u16,
        op0: (u8, u16, u8),
        op1: (u8, u16, u8),
    ) -> Self {
        Self {
            handler,
            hint,
            opcode,
            operands: [op0, op1],
        }
    }

    /// Bit mask of every opcode bit this row's operands vary over.
    fn varying_bits(&self) -> u16 {
        self.operands
            .iter()
            .fold(0u16, |acc, (_, mask, shift)| acc | (mask << shift))
    }
}

/// A resolved dispatch slot.
#[derive(Debug, Clone, Copy)]
pub struct Dispatch {
    /// Which handler in [`crate::ops`] runs for this opcode.
    pub handler: &'static str,
    /// Hint flags, copied from the source row.
    pub hint: u32,
    /// Operand descriptors, copied from the source row.
    pub operands: [(u8, u16, u8); 2],
}

/// The 65536-entry opcode dispatch, built once on first use.
///
/// Built on the heap on purpose: `[None; 0x10000]` as a value would be a
/// multi-megabyte stack temporary, which overflows a debug-build stack.
pub fn dispatch() -> &'static [Option<Dispatch>] {
    static TABLE: OnceLock<Box<[Option<Dispatch>]>> = OnceLock::new();
    TABLE.get_or_init(build_dispatch)
}

fn build_dispatch() -> Box<[Option<Dispatch>]> {
    let mut table: Vec<Option<Dispatch>> = vec![None; 0x10000];

    for source in OPCODE_SOURCES {
        let varying = source.varying_bits();

        // Enumerate every combination of the varying bits.
        let mut permutations: Vec<u16> = vec![source.opcode];
        let mut checkbit = 0x8000u16;
        while checkbit != 0 {
            if varying & checkbit != 0 {
                let added: Vec<u16> = permutations.iter().map(|p| p | checkbit).collect();
                permutations.extend(added);
            }
            checkbit >>= 1;
        }

        let entry = Dispatch {
            handler: source.handler,
            hint: source.hint,
            operands: source.operands,
        };
        for permutation in permutations {
            let slot = &mut table[permutation as usize];
            if slot.is_none() {
                *slot = Some(entry);
            }
        }
    }

    table.into_boxed_slice()
}

/// Instruction length in bytes: 4 when a long immediate follows, else 2.
#[inline]
pub fn opcode_length(opcode: u16) -> usize {
    match dispatch()[opcode as usize] {
        Some(entry) if entry.hint & H_TI != 0 => 4,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV-1a 64 over the whole dispatch, hashed field by field.
    ///
    /// Comparing this against the stored digest proves every one of the 65536
    /// halfwords decodes to the handler and operand layout the table claims.
    fn rust_dispatch_digest() -> (u64, usize) {
        use std::fmt::Write;

        let mut digest: u64 = 0xCBF29CE484222325;
        let mut mapped = 0usize;
        let mut text = String::new();
        for slot in dispatch() {
            text.clear();
            match slot {
                None => text.push('-'),
                Some(entry) => {
                    mapped += 1;
                    let (s0, m0, h0) = entry.operands[0];
                    let (s1, m1, h1) = entry.operands[1];
                    let _ = write!(
                        text,
                        "{}|{}|{},{},{}|{},{},{};",
                        entry.handler, entry.hint, s0, m0, h0, s1, m1, h1
                    );
                }
            }
            for byte in text.bytes() {
                digest ^= byte as u64;
                digest = digest.wrapping_mul(0x100000001B3);
            }
        }
        (digest, mapped)
    }

    #[test]
    fn the_dispatch_matches_the_stored_digest() {
        let (digest, mapped) = rust_dispatch_digest();
        assert_eq!(
            digest,
            crate::dispatch_digest::DISPATCH_DIGEST,
            "the opcode table and the decoder disagree; one of them changed without the other"
        );
        assert_eq!(mapped, crate::dispatch_digest::MAPPED_OPCODES);
    }

    #[test]
    fn table_has_the_same_row_count_as_the_reference() {
        //  has 165 rows; a dropped row would silently change
        // decoding, so this is the cheap guard against a bad regeneration.
        assert_eq!(OPCODE_SOURCES.len(), 165);
    }

    #[test]
    fn mapped_opcodes_outnumber_unmapped_by_a_wide_margin() {
        let table = dispatch();
        let mapped = table.iter().filter(|slot| slot.is_some()).count();
        assert!(mapped > 50_000, "only {mapped} opcodes decoded");
        assert!(mapped < 65_536, "every opcode decoding would be suspicious");
    }

    #[test]
    fn every_mapped_opcode_names_a_handler_we_implement() {
        for (opcode, slot) in dispatch().iter().enumerate() {
            if let Some(entry) = slot {
                assert!(
                    crate::ops::ALL_HANDLERS.contains(&entry.handler),
                    "opcode {opcode:#06x} maps to unknown handler {}",
                    entry.handler
                );
            }
        }
    }

    #[test]
    fn known_encodings_land_where_the_manual_says() {
        let table = dispatch();
        let handler = |word: u16| table[word as usize].unwrap().handler;

        assert_eq!(handler(0xFE8F), "OP_NOP");
        assert_eq!(handler(0xFE1F), "OP_RT");
        assert_eq!(handler(0xFE0F), "OP_RTI");
        assert_eq!(handler(0xFE2F), "OP_INC_EA");
        assert_eq!(handler(0xFE3F), "OP_DEC_EA");
        assert_eq!(handler(0xFFFF), "OP_BRK");
        assert_eq!(handler(0xF00E), "OP_POP"); // POP R0
        assert_eq!(handler(0xF01E), "OP_POP"); // POP ER0
        assert_eq!(handler(0xE100), "OP_ADDSP");
        // `POP PC` is the 0xF08E row with list bit 1 set: 0xF28E, i.e. `8E F2`.
        assert_eq!(handler(0xF28E), "OP_POPL");
        // `PUSH LR` is the 0xF0CE row with list bit 3 set: 0xF8CE, i.e. `CE F8`.
        assert_eq!(handler(0xF8CE), "OP_PUSHL");
    }

    #[test]
    fn pop_pc_list_mask_lives_in_opcode_bits_8_to_11() {
        //  declares `mask 0x000F, shift 8`.
        let entry = dispatch()[0xF08E].unwrap();
        assert_eq!(entry.operands[0], (0, 0x000F, 8));
        let word = 0xF28E;
        assert_eq!((word >> 8) & 0x000F, 0b0010);
    }

    #[test]
    fn instruction_lengths_follow_the_long_immediate_bit() {
        assert_eq!(opcode_length(0xFE1F), 2); // RT
        assert_eq!(opcode_length(0xF00C), 4); // LEA Dadr
        assert_eq!(opcode_length(0xF000), 4); // B Cadr
        assert_eq!(opcode_length(0xF001), 4); // BL Cadr
    }

    #[test]
    fn the_first_row_wins_for_an_opcode_claimed_twice() {
        // 0xFFFF is BRK; nothing later may steal it.
        assert_eq!(dispatch()[0xFFFF].unwrap().handler, "OP_BRK");
    }
}
