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

//! Code patches: replacing an instruction without touching the ROM image.
//!
//! A patch is written into the bus's *overlay* list, which shadows both the code
//! fetch and the data read at an address.  The ROM image on disk is never modified,
//! so a patch is undoable by construction and a session that goes wrong leaves
//! nothing behind.
//!
//! # Three properties that are easy to get wrong
//!
//! * **An overlay shadows data reads too.**  `Bus::read_data_plain` consults the
//!   overlay before the ROM windows, so patching an address that the ROM also reads
//!   as *data* changes that read as well.  A patch on an instruction in a table the
//!   ROM walks will therefore alter both.  That is inherent to how the hardware's
//!   fetch works here, and worth knowing before patching something in `0x00000`.
//! * **A patch must cover whole instructions.**  `read_code` pairs the overlay's
//!   byte with the *overlay's own* next byte, using `0` past the end -- so a
//!   one-byte overlay yields `lo | 0x00<<8` and silently changes the halfword.  The
//!   assembler helpers all produce whole instructions, and [`Patcher::apply`]
//!   checks the length.
//! * **The overlay list is the only copy.**  [`Patches`] re-derives its view from
//!   the bus on every access rather than caching one, because restoring a snapshot
//!   rewrites the overlay list underneath it.  A cache would then be describing a
//!   machine that no longer exists.
//!
//! # What can be assembled
//!
//! The [`nxu8_asm::asm`] helpers, which is most of what a patch wants: moves,
//! arithmetic, loads and stores, compares, branches, and the stack forms.  A
//! general assembler is a much larger job, and is not needed to change one
//! instruction.

use fx991_bus::Bus;
use nxu8_asm::asm;

/// Why a patch could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchError {
    /// The bytes do not cover a whole instruction.
    OddLength(usize),
    /// The address is not even.
    UnalignedAddress(u32),
    /// The assembler rejected the request.
    Assembly(String),
    /// The bytes would run past the end of the address space.
    OutOfRange {
        /// The address asked for.
        address: u32,
        /// How many bytes were offered.
        length: usize,
    },
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatchError::OddLength(length) => write!(
                f,
                "{length} bytes is not a whole instruction; a patch must be 2 or 4 bytes"
            ),
            PatchError::UnalignedAddress(address) => {
                write!(f, "{address:#x} is odd; instructions are halfword aligned")
            }
            PatchError::Assembly(message) => write!(f, "could not assemble: {message}"),
            PatchError::OutOfRange { address, length } => write!(
                f,
                "{length} bytes at {address:#x} runs past the address space"
            ),
        }
    }
}

impl std::error::Error for PatchError {}

/// One applied patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    /// The address it was written at.
    pub address: u32,
    /// The bytes written.
    pub bytes: Vec<u8>,
    /// The text the user typed, so the patch describes itself.
    ///
    /// Kept rather than re-derived from the bytes: rendering mnemonics back from
    /// bytes is the disassembler's job and would not round-trip a hand-written
    /// form like a branch target.
    pub source: String,
    /// The ROM bytes that were there before, for reporting the change.
    pub original: Vec<u8>,
}

impl Patch {
    /// How many bytes it covers.
    pub fn length(&self) -> usize {
        self.bytes.len()
    }

    /// The bytes as hex, e.g. `8E F2`.
    pub fn byte_text(&self) -> String {
        self.bytes
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The original bytes as hex.
    pub fn original_text(&self) -> String {
        self.original
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A view over the patches currently planted in a bus.
///
/// Borrowed from the bus rather than owned: see the module documentation.
pub struct Patches<'a> {
    bus: &'a Bus,
}

impl<'a> Patches<'a> {
    /// Read the patches out of a bus.
    pub fn of(bus: &'a Bus) -> Self {
        Self { bus }
    }

    /// Every planted block, in the order they were applied.
    ///
    /// The source text is not recoverable from the bus -- only the bytes are -- so
    /// this reports what is there with the address as its description.  Callers
    /// that kept the [`Patch`] records should use those instead.
    pub fn iter(&self) -> impl Iterator<Item = Patch> + '_ {
        self.bus.overlays().iter().map(|(address, bytes)| Patch {
            address: *address,
            bytes: bytes.clone(),
            source: String::new(),
            original: Vec::new(),
        })
    }

    /// How many blocks are planted.
    pub fn len(&self) -> usize {
        self.bus.overlays().len()
    }

    /// True when nothing is planted.
    pub fn is_empty(&self) -> bool {
        self.bus.overlays().is_empty()
    }

    /// The bytes planted at an address, if any.
    pub fn at(&self, address: u32) -> Option<&[u8]> {
        self.bus
            .overlays()
            .iter()
            .find(|(base, _)| *base == address)
            .map(|(_, bytes)| bytes.as_slice())
    }
}

/// Write and remove patches.
///
/// A thin layer over the bus so that every patch goes through the length and
/// alignment checks, and so that the ROM bytes a patch replaces are captured for
/// reporting.  It holds no state: see the module documentation.
#[derive(Debug, Default, Clone, Copy)]
pub struct Patcher;

impl Patcher {
    /// Plant `bytes` at `address`, replacing any patch already there.
    ///
    /// `source` is the text to remember for display.
    pub fn apply(
        bus: &mut Bus,
        address: u32,
        bytes: &[u8],
        source: impl Into<String>,
    ) -> Result<Patch, PatchError> {
        if !address.is_multiple_of(2) {
            return Err(PatchError::UnalignedAddress(address));
        }
        if !bytes.len().is_multiple_of(2) || bytes.is_empty() {
            return Err(PatchError::OddLength(bytes.len()));
        }
        if address as usize + bytes.len() > 0x100_0000 {
            return Err(PatchError::OutOfRange {
                address,
                length: bytes.len(),
            });
        }

        let original = original_bytes(bus, address, bytes.len());
        bus.load_code(address, bytes);
        Ok(Patch {
            address,
            bytes: bytes.to_vec(),
            source: source.into(),
            original,
        })
    }

    /// Remove the patch at `address`.  Returns whether there was one.
    pub fn undo(bus: &mut Bus, address: u32) -> bool {
        bus.clear_code(address)
    }

    /// Remove every patch.
    pub fn undo_all(bus: &mut Bus) -> usize {
        let addresses: Vec<u32> = bus.overlays().iter().map(|(base, _)| *base).collect();
        for address in &addresses {
            bus.clear_code(*address);
        }
        addresses.len()
    }
}

/// The bytes an address currently reads from ROM, ignoring any overlay.
///
/// Read straight from the image rather than through the bus so that re-patching an
/// address reports the *ROM's* bytes as the original, not the previous patch's.
fn original_bytes(bus: &Bus, address: u32, length: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(length);
    for offset in 0..length as u32 {
        let at = address.wrapping_add(offset);
        let byte = bus.rom().byte(rom_offset(at)).unwrap_or(0);
        out.push(byte);
    }
    out
}

/// Where a data-space address lands in the ROM image, for the windows that map it.
fn rom_offset(address: u32) -> u32 {
    for window in fx991_bus::CLASSWIZ_ROM_WINDOWS {
        if address >= window.base && address < window.base + window.size {
            return window.rom_base + (address - window.base);
        }
    }
    // Segment 0 code fetches read the image directly, past the region boundary.
    if address < 0x1_0000 {
        return address;
    }
    address
}

/// An instruction assembled from a name and operands, for the `patch` command.
///
/// Deliberately a small, explicit set rather than a general parser: these are the
/// forms a patch actually uses, and each one is a call into the typed helper that
/// the encoder tests already cover.
pub fn assemble_instruction(text: &str) -> Result<Vec<u8>, PatchError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(PatchError::Assembly("no instruction given".to_string()));
    }
    let (mnemonic, operands) = match text.find(char::is_whitespace) {
        Some(index) => (&text[..index], text[index..].trim()),
        None => (text, ""),
    };
    let parts: Vec<&str> = operands
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();

    // Mnemonics, register names and the `[EA]` operand forms are matched
    // case-insensitively, the way a person types them.  The original spelling is
    // kept for the operands so an immediate still reads as the user wrote it.
    let mnemonic_upper = mnemonic.to_ascii_uppercase();
    let upper: Vec<String> = parts.iter().map(|part| part.to_ascii_uppercase()).collect();
    let operands_upper: Vec<&str> = upper.iter().map(String::as_str).collect();

    match (mnemonic_upper.as_str(), operands_upper.as_slice()) {
        ("NOP", []) => Ok(asm::NOP.to_vec()),
        ("RT", []) => Ok(asm::RT.to_vec()),
        ("RTI", []) => Ok(asm::RTI.to_vec()),
        ("BRK", []) => Ok(asm::BRK.to_vec()),
        ("POP", ["PC"]) => Ok(asm::pop_pc().to_vec()),
        ("POP", ["LR"]) => Ok(asm::pop_lr().to_vec()),
        ("PUSH", ["LR"]) => Ok(asm::push_lr().to_vec()),
        ("INC", ["[EA]"]) => Ok(asm::INC_EA.to_vec()),
        ("DEC", ["[EA]"]) => Ok(asm::DEC_EA.to_vec()),
        ("EI", []) => Ok(asm::ei()),
        ("DI", []) => Ok(asm::di()),
        ("CPLC", []) => Ok(asm::cplc()),
        // Loading and storing through `EA`, which is how a patch reaches memory:
        // `LEA Dadr` then `L`/`ST` with `[EA]`.  Without these a patch cannot touch
        // a RAM address at all, which is most of what one is for.
        ("LEA", [_]) => Ok(asm::lea_dadr(parse_address(parts[0])?)),
        ("L", [_, _]) if operand_is(&parts, 1, "[EA]") => {
            Ok(asm::l_r_ea(byte_register(parts[0], "L")?))
        }
        ("ST", [_, _]) if operand_is(&parts, 1, "[EA]") => {
            Ok(asm::st_r_ea(byte_register(parts[0], "ST")?))
        }
        ("L", [_, _]) if operand_is(&parts, 1, "[EA+]") => {
            Ok(asm::l_r_ea_inc(byte_register(parts[0], "L")?))
        }
        ("ST", [_, _]) if operand_is(&parts, 1, "[EA+]") => {
            Ok(asm::st_r_ea_inc(byte_register(parts[0], "ST")?))
        }
        // The indexed form needs the pointer register named explicitly, because the
        // encoder takes it as a field rather than deriving it.
        ("L", [_, pointer]) | ("ST", [_, pointer]) if register_index_er(pointer).is_some() => {
            let pointer = register_index_er(pointer).expect("just checked");
            if mnemonic_upper == "L" {
                Ok(asm::l_r_er(byte_register(parts[0], "L")?, pointer))
            } else {
                Ok(asm::st_r_er(byte_register(parts[0], "ST")?, pointer))
            }
        }
        // The forms a patch is most likely to need: move a value in, compare,
        // jump, and call.
        ("MOV", [_, _]) if parts.len() == 2 && is_immediate(parts[1]) => {
            let index = register_index(parts[0]).ok_or_else(|| {
                PatchError::Assembly(format!("MOV wants a byte register 0-15, got {}", parts[0]))
            })?;
            Ok(asm::mov_r_imm(index, immediate_value(parts[1])?))
        }
        ("MOV", [_, _]) => match (
            register_index_er(parts[0]),
            register_index_er(parts.get(1).copied().unwrap_or("")),
        ) {
            (Some(n), Some(m)) => Ok(asm::mov_er_er(n, m)),
            _ => Err(PatchError::Assembly(format!(
                "MOV wants two ER registers, got {parts:?}"
            ))),
        },
        ("ADD", [_, _]) if parts.len() == 2 && is_immediate(parts[1]) => {
            let index = register_index(parts[0]).ok_or_else(|| {
                PatchError::Assembly(format!("ADD wants a byte register 0-15, got {}", parts[0]))
            })?;
            Ok(asm::add_r_imm(index, immediate_value(parts[1])?))
        }
        ("CMP", [_, _]) => match (
            register_index(parts[0]),
            register_index(parts.get(1).copied().unwrap_or("")),
        ) {
            (Some(n), Some(m)) => Ok(asm::cmp_r_r(n, m)),
            _ => Err(PatchError::Assembly(format!(
                "CMP wants two byte registers, got {parts:?}"
            ))),
        },
        ("BL", [_]) => Ok(asm::bl(0, parse_address(parts[0])?)),
        ("B", [_]) => Ok(asm::b(0, parse_address(parts[0])?)),
        // A relative branch cannot be assembled from the target alone: the
        // displacement depends on where the instruction itself sits, which this
        // function is not told.
        ("BC", [condition, target]) => {
            let qualifier = match condition_code(condition) {
                Some(_) => String::new(),
                None => format!(" (and {condition:?} is not one of the sixteen)"),
            };
            Err(PatchError::Assembly(format!(
                "BC is relative, so it needs its own address to compute the displacement \
                 to {target}; use `asm::bc_to(pc, target, {})` instead{qualifier}",
                condition.to_ascii_uppercase(),
            )))
        }
        _ => Err(PatchError::Assembly(format!(
            "no assembler for {text:?}; supported: NOP, RT, RTI, BRK, POP PC/LR, \
             PUSH LR, INC/DEC [EA], EI, DI, CPLC, MOV Rn,#imm, MOV ERn,ERm, \
             ADD Rn,#imm, CMP Rn,Rm, B/BL addr"
        ))),
    }
}

/// The byte index of a `Rn` name, or `None` when it is not one.
///
/// The index is range-checked here rather than left to the encoder: `R20` would
/// otherwise reach `encode_expecting`, which **panics** because the assembler
/// helpers unwrap internally.  A bad operand is a user error and has to be a
/// `PatchError`, not a crash.
fn register_index(text: &str) -> Option<u8> {
    let text = text.trim();
    let rest = text.strip_prefix('R').or_else(|| text.strip_prefix('r'))?;
    let index: u8 = rest.parse().ok()?;
    (index < 16).then_some(index)
}

/// The byte index of an `ERn` name, or `None` when it is not one.
///
/// `ER` names are the even byte offsets, so the index is both range- and
/// parity-checked.
fn register_index_er(text: &str) -> Option<u8> {
    let text = text.trim();
    let rest = text
        .strip_prefix("ER")
        .or_else(|| text.strip_prefix("er"))
        .or_else(|| text.strip_prefix("Er"))
        .or_else(|| text.strip_prefix("eR"))?;
    let index: u8 = rest.parse().ok()?;
    (index < 16 && index.is_multiple_of(2)).then_some(index)
}

fn is_immediate(text: &str) -> bool {
    text.trim().starts_with('#')
}

/// Whether operand `index` is exactly `expected`, ignoring case.
///
/// The operands arrive uppercased, so the comparison text is uppercase too.  This
/// is what separates `L R0,[EA]` from `L R0,[ER8]`, which encode differently.
fn operand_is(parts: &[&str], index: usize, expected: &str) -> bool {
    parts
        .get(index)
        .is_some_and(|part| part.eq_ignore_ascii_case(expected))
}

/// A byte register operand, or a patch error naming the mnemonic.
fn byte_register(text: &str, mnemonic: &str) -> Result<u8, PatchError> {
    register_index(text).ok_or_else(|| {
        PatchError::Assembly(format!("{mnemonic} wants a byte register 0-15, got {text}"))
    })
}

fn immediate_value(text: &str) -> Result<u8, PatchError> {
    let digits = text.trim().trim_start_matches('#');
    let value = parse_u32(digits)
        .ok_or_else(|| PatchError::Assembly(format!("cannot read the immediate {digits:?}")))?;
    u8::try_from(value)
        .map_err(|_| PatchError::Assembly(format!("{value:#x} does not fit in a byte")))
}

/// A 16-bit address, decimal or hexadecimal.
fn parse_address(text: &str) -> Result<u16, PatchError> {
    let text = text.trim();
    let value = parse_u32(text)
        .ok_or_else(|| PatchError::Assembly(format!("cannot read the address {text:?}")))?;
    u16::try_from(value)
        .map_err(|_| PatchError::Assembly(format!("{value:#x} does not fit in an address")))
}

/// A number, in the shared grammar the command line and conditions use.
///
/// The grammar lives in [`crate::number`]; this is the narrow-to-`u32` entry point
/// for the assembler's operands.
fn parse_u32(text: &str) -> Option<u32> {
    crate::number::parse_u32(text)
}

fn condition_code(text: &str) -> Option<u8> {
    Some(match text.trim().to_ascii_uppercase().as_str() {
        "GE" => asm::cond::GE,
        "LT" => asm::cond::LT,
        "GT" => asm::cond::GT,
        "LE" => asm::cond::LE,
        "GES" => asm::cond::GES,
        "LTS" => asm::cond::LTS,
        "GTS" => asm::cond::GTS,
        "LES" => asm::cond::LES,
        "NE" => asm::cond::NE,
        "EQ" => asm::cond::EQ,
        "NV" => asm::cond::NV,
        "OV" => asm::cond::OV,
        "PS" => asm::cond::PS,
        "NS" => asm::cond::NS,
        "AL" => asm::cond::AL,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxu8_core::Memory;

    /// A bus over a ROM holding recognisable bytes.
    fn bus() -> Bus {
        let mut data = vec![0u8; 0x4_0000];
        data[0x1_0000] = 0x00;
        data[0x1_0001] = 0x00; // MOV R0,#0
        data[0x1_0002] = 0x8E;
        data[0x1_0003] = 0xF2; // POP PC
        Bus::new(fx991_bus::Rom::new(data))
    }

    #[test]
    fn a_patch_shadows_the_fetch_and_can_be_undone() {
        let mut bus = bus();
        assert_eq!(bus.read_code(0x1_0000), 0x0000);

        let patch = Patcher::apply(&mut bus, 0x1_0000, &[0x8E, 0xF2], "POP PC").unwrap();
        assert_eq!(bus.read_code(0x1_0000), 0xF28E);
        assert_eq!(patch.bytes, vec![0x8E, 0xF2]);
        assert_eq!(patch.original, vec![0x00, 0x00], "the ROM bytes are noted");
        assert_eq!(patch.byte_text(), "8E F2");
        assert_eq!(patch.source, "POP PC");

        assert!(Patcher::undo(&mut bus, 0x1_0000));
        assert_eq!(bus.read_code(0x1_0000), 0x0000, "the image shows again");
        assert!(!Patcher::undo(&mut bus, 0x1_0000));
    }

    #[test]
    fn a_patch_shadows_a_data_read_too() {
        // Inherent to how the fetch works here, and worth being explicit about:
        // patching an address the ROM also reads as data changes both.
        let mut bus = bus();
        bus.load_code(0x1_0000, &[0xAB, 0xCD]);
        assert_eq!(bus.read_data(0x1_0000), 0xAB);
    }

    #[test]
    fn an_odd_length_or_odd_address_is_refused() {
        let mut bus = bus();
        assert_eq!(
            Patcher::apply(&mut bus, 0x1_0000, &[0x8E], "half").unwrap_err(),
            PatchError::OddLength(1)
        );
        assert_eq!(
            Patcher::apply(&mut bus, 0x1_0001, &[0x8E, 0xF2], "odd").unwrap_err(),
            PatchError::UnalignedAddress(0x1_0001)
        );
        assert!(Patches::of(&bus).is_empty(), "nothing was planted");
    }

    #[test]
    fn re_patching_reports_the_rom_bytes_as_the_original() {
        // Otherwise the second patch would claim the first patch's bytes were the
        // ROM's, and an undo report would be nonsense.
        let mut bus = bus();
        Patcher::apply(&mut bus, 0x1_0000, &[0x11, 0x22], "first").unwrap();
        let second = Patcher::apply(&mut bus, 0x1_0000, &[0x33, 0x44], "second").unwrap();
        assert_eq!(second.original, vec![0x00, 0x00]);
        assert_eq!(bus.read_code(0x1_0000), 0x4433);
    }

    #[test]
    fn the_view_lists_and_finds_what_is_planted() {
        let mut bus = bus();
        Patcher::apply(&mut bus, 0x1_0000, &[0x8E, 0xF2], "a").unwrap();
        Patcher::apply(&mut bus, 0x1_0004, &[0x8F, 0xFE], "b").unwrap();

        let patches = Patches::of(&bus);
        assert_eq!(patches.len(), 2);
        assert!(!patches.is_empty());
        assert_eq!(patches.at(0x1_0000), Some([0x8E, 0xF2].as_slice()));
        assert_eq!(patches.at(0x1_0004), Some([0x8F, 0xFE].as_slice()));
        assert_eq!(patches.at(0x1_0008), None);

        let addresses: Vec<u32> = patches.iter().map(|patch| patch.address).collect();
        assert_eq!(addresses, vec![0x1_0000, 0x1_0004]);
    }

    #[test]
    fn undo_all_removes_everything_and_says_how_many() {
        let mut bus = bus();
        Patcher::apply(&mut bus, 0x1_0000, &[0x8E, 0xF2], "a").unwrap();
        Patcher::apply(&mut bus, 0x1_0004, &[0x8F, 0xFE], "b").unwrap();
        assert_eq!(Patcher::undo_all(&mut bus), 2);
        assert!(Patches::of(&bus).is_empty());
        assert_eq!(Patcher::undo_all(&mut bus), 0);
    }

    #[test]
    fn the_view_follows_a_snapshot_restore_rather_than_caching() {
        // The reason `Patches` borrows the bus instead of storing a copy: restoring
        // rewrites the overlay list underneath it.
        let mut bus = bus();
        Patcher::apply(&mut bus, 0x1_0000, &[0x8E, 0xF2], "a").unwrap();
        let snapshot = bus.snapshot();

        Patcher::undo_all(&mut bus);
        assert!(Patches::of(&bus).is_empty());

        bus.restore(&snapshot);
        assert_eq!(
            Patches::of(&bus).len(),
            1,
            "the snapshot brought the patch back"
        );
    }

    // -------------------------------------------------------------- assembling

    #[test]
    fn the_common_instructions_assemble() {
        assert_eq!(assemble_instruction("NOP").unwrap(), asm::NOP.to_vec());
        assert_eq!(assemble_instruction("RT").unwrap(), asm::RT.to_vec());
        assert_eq!(assemble_instruction("POP PC").unwrap(), vec![0x8E, 0xF2]);
        assert_eq!(
            assemble_instruction("INC [EA]").unwrap(),
            asm::INC_EA.to_vec()
        );
        assert_eq!(assemble_instruction("EI").unwrap(), asm::ei());
        assert_eq!(assemble_instruction("DI").unwrap(), asm::di());
    }

    #[test]
    fn mnemonic_case_and_spacing_do_not_matter() {
        assert_eq!(assemble_instruction("pop pc").unwrap(), vec![0x8E, 0xF2]);
        assert_eq!(
            assemble_instruction("  POP   PC  ").unwrap(),
            vec![0x8E, 0xF2]
        );
        assert_eq!(assemble_instruction("mov r0,#1").unwrap(), vec![0x01, 0x00]);
        assert_eq!(
            assemble_instruction("MOV  R0 , #1").unwrap(),
            vec![0x01, 0x00]
        );
    }

    #[test]
    fn immediates_and_registers_land_where_the_encoder_puts_them() {
        // `MOV R9,#8` is `08 09`: the register index is the high nibble of the
        // second byte, which the encoder tests also pin.
        assert_eq!(assemble_instruction("MOV R9,#8").unwrap(), vec![0x08, 0x09]);
        assert_eq!(
            assemble_instruction("MOV R0,#0x40").unwrap(),
            vec![0x40, 0x00]
        );
        assert_eq!(
            assemble_instruction("MOV ER6,ER8").unwrap(),
            asm::mov_er_er(6, 8)
        );
        assert_eq!(
            assemble_instruction("CMP R12,R3").unwrap(),
            asm::cmp_r_r(12, 3)
        );
        assert_eq!(
            assemble_instruction("ADD R4,#0x10").unwrap(),
            asm::add_r_imm(4, 0x10)
        );
    }

    #[test]
    fn a_branch_takes_an_address() {
        assert_eq!(
            assemble_instruction("BL 0F968h").unwrap(),
            asm::bl(0, 0x0F968)
        );
        // The colon form is not something the assembler accepts, and the error
        // quotes the operand as typed.
        assert_eq!(
            assemble_instruction("B 00h:0946Ah").unwrap_err(),
            PatchError::Assembly("cannot read the address \"00h:0946Ah\"".to_string())
        );
        assert_eq!(assemble_instruction("B 0x946A").unwrap(), asm::b(0, 0x946A));
    }

    #[test]
    fn an_unknown_mnemonic_lists_what_is_supported() {
        let error = assemble_instruction("FROBNICATE R0").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("no assembler"), "{message}");
        assert!(message.contains("NOP"), "the error lists the alternatives");
    }

    #[test]
    fn an_out_of_range_immediate_is_refused_rather_than_truncated() {
        let error = assemble_instruction("MOV R0,#0x1FF").unwrap_err();
        assert!(error.to_string().contains("does not fit"), "{error}");
    }

    #[test]
    fn a_value_that_is_not_a_register_is_refused() {
        assert!(assemble_instruction("MOV R20,#1").is_err());
        assert!(assemble_instruction("CMP R0,R1").is_ok());
        assert!(assemble_instruction("CMP R0,5").is_err());
        assert!(assemble_instruction("").is_err());
    }

    #[test]
    fn bc_explains_that_it_needs_its_own_address() {
        // A relative branch cannot be assembled from the target alone, and the
        // error says so rather than silently producing a wrong displacement.
        let error = assemble_instruction("BC EQ, 0F968h").unwrap_err();
        assert!(error.to_string().contains("displacement"), "{error}");
    }

    #[test]
    fn a_patch_applied_through_the_assembler_is_fetchable() {
        let mut bus = bus();
        let bytes = assemble_instruction("POP PC").unwrap();
        let patch = Patcher::apply(&mut bus, 0x1_0002, &bytes, "POP PC").unwrap();
        assert_eq!(patch.bytes, vec![0x8E, 0xF2]);
        assert_eq!(bus.read_code(0x1_0002), 0xF28E);
        assert_eq!(patch.original, vec![0x8E, 0xF2], "that is what the ROM had");
    }
}
