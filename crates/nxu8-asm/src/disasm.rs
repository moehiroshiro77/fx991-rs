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

//! Table-driven nX-U8 disassembler.
//!
//! This is the inverse of the encoder in this crate, and it reads its operand
//! placement from the same place: [`nxu8_core::decode::dispatch`].  Neither half
//! can drift from the decoder, because neither has its own copy of the chart.
//!
//! # Why decode bytes instead of reading a listing
//!
//! `data/_disas_verF.txt` is *derived* data -- a separate tool generated it from
//! a declarative instruction-set table.  Reading it back is fine for a
//! static pass over the ROM, but a debugger cannot use it: a listing on disk has
//! no way to show patched bytes, code written into RAM, a `load_code` overlay, or
//! an opcode the generator never saw.  Those are precisely the cases worth
//! staring at while debugging, so the text here is rendered from the bytes
//! actually present at an address.
//!
//! The listing keeps one job: it is an independently produced oracle.  The
//! `listing_oracle` test renders all 116 944 of its instructions from the ROM and
//! compares the two texts, which is a stronger check than either one alone
//! (and is how the rendering rules below were established).
//!
//! # Rendering rules
//!
//! Spacing, `h` suffixes, `[EA+]`, and register naming all follow the `u8dis`
//! spec the listing came from, so the two agree line for line.  Three conventions
//! are worth stating because they are not guessable:
//!
//! * Immediates written `#{i}` in the source chart are **decimal**; immediates
//!   written `#{signedtohex(i, n)}h` are **hex** with a `-` sign and a digit
//!   width of `1 + (n-1)/4`.  `ADD Rn,#96` is decimal, `ADD SP,#-27h` is hex.
//! * Several mnemonics share one handler, so the mnemonic is resolved from the
//!   *table row*, recovered as `word & !varying_bits`.  That is what tells
//!   `CMP`/`SUB`/`CMPC` apart, and `MOV` from `ADD` for the `ERn,#imm` form.
//! * A `DSR<-` prefix decodes as its own instruction, because that is how the
//!   bytes read.  A caller that needs the width of one *executed* step wants
//!   [`step_length`], since the CPU runs the prefix and its payload together.

#![warn(missing_docs)]

use nxu8_core::decode::{dispatch, H_DS, H_IA, H_ST, H_TI};

/// How wide the mnemonic column is, matching the listing's alignment.
const MNEMONIC_WIDTH: usize = 8;

/// The `BC` condition names.  Index 15 is not a condition, and the reference
/// generator prints it literally, so this does too.
const CONDITIONS: [&str; 16] = [
    "GE",
    "LT",
    "GT",
    "LE",
    "GES",
    "LTS",
    "GTS",
    "LES",
    "NE",
    "EQ",
    "NV",
    "OV",
    "PS",
    "NS",
    "AL",
    "<Unrecognized>",
];

/// What kind of thing is at an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsnKind {
    /// An ordinary instruction.
    Normal,
    /// A `DSR<-` prefix, which applies to the instruction after it.
    DsrPrefix,
    /// An opcode the decode table does not map.
    Unknown,
    /// An encoding the instruction chart defines but the decode table omits, so
    /// this core cannot execute it even though its meaning is known.
    ///
    /// The two known cases are `BC` with condition field 15 and `EXTBW` whose two
    /// register fields disagree; the chart prints its own diagnostic for the
    /// latter.  A debugger should show them, because a ROM that reaches one is
    /// doing something the emulator cannot follow.
    ChartOnly,
    /// A coprocessor transfer, which the core deliberately refuses to execute.
    Coprocessor,
}

/// One disassembled instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insn {
    /// Physical address of the instruction's first byte.
    pub address: u32,
    /// The bytes the instruction occupies: 2, or 4 when a long immediate follows.
    pub bytes: Vec<u8>,
    /// The rendered text, e.g. `MOV     R0, #96`.
    ///
    /// A `DSR<-` prefix renders as its own line; see [`InsnKind::DsrPrefix`].
    pub text: String,
    /// Length of [`Insn::bytes`], in bytes.
    pub length: usize,
    /// The decode table's handler name, or `None` when the opcode is unmapped.
    pub handler: Option<&'static str>,
    /// What kind of thing was decoded.
    pub kind: InsnKind,
    /// True for `BL`, the only instruction that writes the link register.
    ///
    /// A debugger needs this to implement "step over": a call's return address is
    /// not `pc + length`, it is wherever `LR` says.
    pub is_call: bool,
}

impl Insn {
    /// The address, bytes and text on one line, aligned the way the listing is.
    pub fn listing_line(&self) -> String {
        format!(
            "{:06X}   {:<19}{}",
            self.address,
            self.byte_text(),
            self.text
        )
    }

    /// The bytes as the listing writes them: uppercase hex, each followed by a space.
    pub fn byte_text(&self) -> String {
        let mut out = String::with_capacity(self.bytes.len() * 3);
        for byte in &self.bytes {
            out.push_str(&format!("{byte:02X} "));
        }
        out
    }

    /// True when the decode table had no entry for the opcode.
    pub fn is_unknown(&self) -> bool {
        self.kind == InsnKind::Unknown
    }
}

/// Read a code halfword from somewhere.
///
/// The disassembler borrows this rather than a bus, so it stays pure: a debugger
/// passes a side-effect-free read, a test passes a slice, and neither drags the
/// emulator into this crate.
pub trait CodeRead {
    /// The halfword at a physical code address.
    fn halfword(&mut self, address: u32) -> u16;
}

impl<F: FnMut(u32) -> u16> CodeRead for F {
    fn halfword(&mut self, address: u32) -> u16 {
        self(address)
    }
}

/// A [`CodeRead`] over a byte slice, for tests and offline use.
///
/// Reads past the end return 0, the way an unmapped fetch does.
pub struct SliceReader<'a> {
    bytes: &'a [u8],
    base: u32,
}

impl<'a> SliceReader<'a> {
    /// Read a slice as if its first byte sat at `base`.
    pub fn new(bytes: &'a [u8], base: u32) -> Self {
        Self { bytes, base }
    }
}

impl CodeRead for SliceReader<'_> {
    fn halfword(&mut self, address: u32) -> u16 {
        let offset = address.wrapping_sub(self.base) as usize;
        let low = self.bytes.get(offset).copied().unwrap_or(0);
        let high = self.bytes.get(offset + 1).copied().unwrap_or(0);
        u16::from_le_bytes([low, high])
    }
}

/// Decode the instruction starting at `address`.
///
/// A `DSR<-` prefix decodes as itself; use [`step_length`] for the width of the
/// whole step, or [`disassemble`] to see the prefix and its payload together.
pub fn disassemble_one(read: &mut dyn CodeRead, address: u32) -> Insn {
    let address = address & !1;
    let word = read.halfword(address);

    let Some(entry) = dispatch()[word as usize] else {
        // The chart describes two encodings the decode table omits.  Rendering
        // them beats calling them unrecognized: a ROM reaching one is doing
        // something this core cannot execute, and that is worth seeing.
        if let Some(text) = chart_only(word, address) {
            return Insn {
                address,
                bytes: bytes_at(read, address, 2),
                text,
                length: 2,
                handler: None,
                kind: InsnKind::ChartOnly,
                is_call: false,
            };
        }
        return Insn {
            address,
            bytes: bytes_at(read, address, 2),
            text: "Unrecognized command".to_string(),
            length: 2,
            handler: None,
            kind: InsnKind::Unknown,
            is_call: false,
        };
    };

    let has_immediate = entry.hint & H_TI != 0;
    let long_imm = if has_immediate {
        read.halfword(address + 2)
    } else {
        0
    };
    let length = if has_immediate { 4 } else { 2 };

    Insn {
        address,
        bytes: bytes_at(read, address, length),
        text: render(entry.handler, entry.hint, word, long_imm, address),
        length,
        handler: Some(entry.handler),
        kind: if entry.hint & H_DS != 0 {
            InsnKind::DsrPrefix
        } else if entry.handler.starts_with("OP_CR") {
            InsnKind::Coprocessor
        } else {
            InsnKind::Normal
        },
        is_call: entry.handler == "OP_BL",
    }
}

/// The length of one *executed* step at `address`.
///
/// The CPU runs a `DSR<-` prefix and the instruction after it inside a single
/// `step`, so this is the distance a stepper must move: the prefix plus its
/// payload, or just the instruction when there is no prefix.  It is also the
/// distance to the next instruction boundary that begins a step.
pub fn step_length(read: &mut dyn CodeRead, address: u32) -> usize {
    let first = disassemble_one(read, address);
    if first.kind != InsnKind::DsrPrefix {
        return first.length;
    }
    let payload = disassemble_one(read, first.address + first.length as u32);
    first.length + payload.length
}

/// Decode `count` instructions, one per line, address and bytes included.
pub fn disassemble(read: &mut dyn CodeRead, address: u32, count: usize) -> String {
    let mut out = String::new();
    let mut pc = address & !1;
    for _ in 0..count {
        let insn = disassemble_one(read, pc);
        out.push_str(&insn.listing_line());
        out.push('\n');
        pc += insn.length as u32;
    }
    out
}

fn bytes_at(read: &mut dyn CodeRead, address: u32, length: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(length);
    let mut offset = 0u32;
    while (offset as usize) < length {
        bytes.extend_from_slice(&read.halfword(address + offset).to_le_bytes());
        offset += 2;
    }
    bytes
}

// ---------------------------------------------------------------- field helpers

/// Extract an operand field, exactly as `Cpu::step` does.
fn field(word: u16, (_, mask, shift): (u8, u16, u8)) -> u16 {
    (word >> shift) & mask
}

/// The operand bits of a row: `word & !varying` recovers the table row's base
/// opcode, which is what identifies the mnemonic when handlers are shared.
fn varying(entry: &nxu8_core::decode::Dispatch) -> u16 {
    (entry.operands[0].1 << entry.operands[0].2) | (entry.operands[1].1 << entry.operands[1].2)
}

/// The `R`/`ER`/`XR`/`QR` name for a register index and a width in bytes.
fn register(index: u16, width: u8) -> String {
    match width {
        2 => format!("ER{index}"),
        4 => format!("XR{index}"),
        8 => format!("QR{index}"),
        _ => format!("R{index}"),
    }
}

/// Uppercase zero-padded hex, as `tohex` in the source chart.
fn hex(value: u32, digits: usize) -> String {
    format!("{value:0digits$X}")
}

/// The source chart's `signedtohex(n, binlen)`: a `-` sign in front of a
/// zero-padded field whose width is `1 + (binlen-1)/4`, so a negative value is
/// one character wider than a positive one.
fn signed_hex(value: u16, binlen: u32) -> String {
    let sign_bit = binlen - 1;
    let digits = (1 + sign_bit / 4) as usize;
    if (value >> sign_bit) & 1 == 1 {
        let magnitude = ((1u32 << binlen) - value as u32) as u16;
        format!("-{}", hex(magnitude as u32, digits))
    } else {
        hex(value as u32, digits)
    }
}

/// The chart's `0{tohex(n, digits)}h` idiom: a leading zero that makes an address
/// visually distinct from a plain immediate.
fn address_literal(value: u32, digits: usize) -> String {
    format!("0{}h", hex(value, digits))
}

/// Sign-extend `bits` of `value`, for the immediates the chart marks signed.
fn sign_extend(value: u16, bits: u32) -> i64 {
    let shift = 64 - bits;
    (((value as u64) << shift) as i64) >> shift
}

/// Right-pad a mnemonic to the operand column.
fn m(mnemonic: &str) -> String {
    format!("{mnemonic:<MNEMONIC_WIDTH$}")
}

// -------------------------------------------------------------------- rendering

fn render(handler: &str, hint: u32, word: u16, long_imm: u16, address: u32) -> String {
    let entry = dispatch()[word as usize].expect("render is only called for mapped opcodes");
    let base = word & !varying(&entry);
    let (size0, _, _) = entry.operands[0];
    let (size1, _, _) = entry.operands[1];
    let v0 = field(word, entry.operands[0]);
    let v1 = field(word, entry.operands[1]);

    match handler {
        // The byte-register family: one mnemonic per table row.
        "OP_MOV" | "OP_ADD" | "OP_ADDC" | "OP_AND" | "OP_OR" | "OP_XOR" => {
            let name = match base {
                0x0000 => "MOV",
                0x1000 => "ADD",
                0x6000 => "ADDC",
                0x2000 => "AND",
                0x3000 => "OR",
                0x4000 => "XOR",
                0x8000 => "MOV",
                0x8001 => "ADD",
                0x8006 => "ADDC",
                0x8002 => "AND",
                0x8003 => "OR",
                0x8004 => "XOR",
                _ => handler,
            };
            format!("{}{}, {}", m(name), register(v0, 1), operand1(v1, size1))
        }
        // SUB and CMPC share OP_SUB / OP_SUBC with their mnemonic row.
        "OP_SUB" | "OP_SUBC" => {
            let name = match base {
                0x7000 => "CMP",
                0x8007 => "CMP",
                0x5000 => "CMPC",
                0x8005 => "CMPC",
                0x8008 => "SUB",
                0x8009 => "SUBC",
                _ => handler,
            };
            format!("{}{}, {}", m(name), register(v0, 1), operand1(v1, size1))
        }
        // The 16-bit forms, where the immediate's signedness differs by row.
        "OP_MOV16" | "OP_ADD16" | "OP_CMP16" => {
            let name = match base {
                0xE000 | 0xF005 => "MOV",
                0xE080 | 0xF006 => "ADD",
                0xF007 => "CMP",
                _ => handler,
            };
            let source = if size1 == 0 {
                // `MOV` takes bits 0-6 as unsigned; `ADD` sign-extends bit 6.
                if base == 0xE080 {
                    format!("#{}", sign_extend(v1, 7))
                } else {
                    format!("#{v1}")
                }
            } else {
                register(v1, size1)
            };
            format!("{}{}, {source}", m(name), register(v0, size0))
        }
        "OP_DAA" | "OP_DAS" | "OP_NEG" => {
            let name = match handler {
                "OP_DAA" => "DAA",
                "OP_DAS" => "DAS",
                _ => "NEG",
            };
            format!("{}{}", m(name), register(v0, 1))
        }
        "OP_MUL" | "OP_DIV" => {
            let name = if handler == "OP_MUL" { "MUL" } else { "DIV" };
            format!("{}{}, {}", m(name), register(v0, size0), register(v1, 1))
        }
        "OP_INC_EA" => format!("{}[EA]", m("INC")),
        "OP_DEC_EA" => format!("{}[EA]", m("DEC")),

        "OP_SLL" | "OP_SLLC" | "OP_SRA" | "OP_SRL" | "OP_SRLC" => {
            let name = match handler {
                "OP_SLL" => "SLL",
                "OP_SLLC" => "SLLC",
                "OP_SRA" => "SRA",
                "OP_SRL" => "SRL",
                _ => "SRLC",
            };
            let source = if size1 == 0 {
                format!("#{v1}")
            } else {
                register(v1, size1)
            };
            format!("{}{}, {source}", m(name), register(v0, 1))
        }

        "OP_LS_EA" | "OP_LS_R" | "OP_LS_I" | "OP_LS_I_R" | "OP_LS_BP" | "OP_LS_FP" => {
            render_load_store(handler, hint, v0, v1, long_imm)
        }

        "OP_PUSH" | "OP_POP" => {
            let name = if handler == "OP_PUSH" { "PUSH" } else { "POP" };
            let index = if handler == "OP_PUSH" { v1 } else { v0 };
            format!("{}{}", m(name), register(index, size1.max(size0)))
        }
        "OP_PUSHL" => {
            let list = render_list(v1 as u8, true);
            format!("{}{list}", m("PUSH"))
        }
        "OP_POPL" => {
            let list = render_list(v0 as u8, false);
            format!("{}{list}", m("POP"))
        }

        "OP_ADDSP" => format!("{}SP, #{}h", m("ADD"), signed_hex(v0, 8)),
        "OP_CTRL" => render_ctrl(hint, v0, v1),
        "OP_LEA" => match base {
            // `LEA [ERm]`
            0xF00A => format!("{}[{}]", m("LEA"), register(v1, 2)),
            // `LEA Dadr[ERm]` -- the chart's `signedtohex(., 16)`, so a negative
            // displacement renders as `-0006h` rather than an unsigned address.
            0xF00B => format!(
                "{}{}h[{}]",
                m("LEA"),
                signed_hex(long_imm, 16),
                register(v1, 2)
            ),
            // `LEA Dadr` -- the chart's `0{tohex(., 4)}h`, a zero-prefixed address.
            _ => format!("{}{}", m("LEA"), address_literal(long_imm as u32, 4)),
        },
        "OP_BC" => {
            let condition = CONDITIONS[((word >> 8) & 0x0F) as usize];
            let target = (address as i64 + 2 + sign_extend(v0, 8) * 2) as u32;
            format!("{}{condition}, {}h", m("BC"), hex(target, 5))
        }
        "OP_B" | "OP_BL" => {
            let name = if handler == "OP_B" { "B" } else { "BL" };
            if hint & H_TI != 0 {
                format!(
                    "{}{}h:{}h",
                    m(name),
                    hex(((word >> 8) & 0x0F) as u32, 2),
                    hex(long_imm as u32, 5)
                )
            } else {
                format!("{}{}", m(name), register(v1, size1))
            }
        }
        "OP_RT" => m("RT").trim_end().to_string(),
        "OP_RTI" => m("RTI").trim_end().to_string(),
        "OP_NOP" => m("NOP").trim_end().to_string(),
        "OP_CPLC" => m("CPLC").trim_end().to_string(),
        "OP_BRK" => m("BRK").trim_end().to_string(),
        "OP_SWI" => format!("{}#{v0}", m("SWI")),
        "OP_DSR" => render_dsr(size0, v0, word),
        "OP_PSW_OR" | "OP_PSW_AND" => psw_name(word),

        "OP_BITMOD" => {
            let name = match word & 0x000F {
                0 => "SB",
                1 => "TB",
                _ => "RB",
            };
            // The 4-byte form takes its address from the long immediate; the
            // 2-byte form takes it from operand 0.
            if hint & H_TI != 0 {
                format!("{}{}.{}", m(name), address_literal(long_imm as u32, 4), v1)
            } else {
                format!("{}{}.{}", m(name), register(v0, 1), v1)
            }
        }
        // The chart decodes `nnn01111 1000mmm1` for **all** n/m combinations and
        // prints a diagnostic when they disagree.  The table only carries the
        // eight rows the ROM contains, so the rest arrive here via
        // [`chart_only`]; this arm handles the mapped ones.
        "OP_EXTBW" => {
            let n = (word >> 5) & 0x7;
            let mm = (word >> 9) & 0x7;
            let prefix = if n == mm { "" } else { "Wrong format - " };
            format!("{prefix}{}ER{}", m("EXTBW"), n * 2)
        }

        // `MOV CRn,Rm` and `MOV Rn,CRm` share the `0xA00E`/`0xA006` row pair;
        // the store hint picks the direction.  In both, operand 0 is the `nnnn`
        // field and operand 1 the `mmmm` field, so only the order changes.
        "OP_CR_R" => {
            if hint & H_ST != 0 {
                format!("{}R{v0}, CR{v1}", m("MOV"))
            } else {
                format!("{}CR{v0}, R{v1}", m("MOV"))
            }
        }
        // `MOV CRn,[EA]` and its width variants.  The register field sits in
        // operand 1 on the load rows and operand 0 on the store rows, and the
        // access width is the hint's secondary selector (`C` + the ordinary
        // register name for that width: byte `CR`, `CER`, `CXR`, `CQR`).
        "OP_CR_EA" => {
            let width = (hint >> 8) as u8;
            let index = if hint & H_ST != 0 { v0 } else { v1 };
            let name = format!("C{}", register(index, width));
            let ea = if hint & H_IA != 0 { "[EA+]" } else { "[EA]" };
            if hint & H_ST != 0 {
                format!("{}{ea}, {name}", m("MOV"))
            } else {
                format!("{}{name}, {ea}", m("MOV"))
            }
        }

        other => format!("{other}   ?"),
    }
}

/// Render an encoding the instruction chart defines but the decode table omits.
///
/// Both are genuinely absent from the table rather than mis-decoded, which is why
/// they are recognized here instead of in [`render`]:
///
/// * `EXTBW ERn` where the `nnn` and `mmm` fields disagree.  The chart enumerates
///   all 64 combinations of `nnn01111 1000mmm1` and prefixes the mismatching ones
///   with its own `Wrong format - ` diagnostic, so that text is reproduced
///   verbatim.  Only the eight agreeing rows are in the table.
/// * `BC` with condition field 15.  The chart's condition table has sixteen
///   entries and calls the last `<Unrecognized>`; the table here stops at `AL`.
fn chart_only(word: u16, address: u32) -> Option<String> {
    // `nnn01111 1000mmm1`: the low byte is `nnn01111`, the high byte `1000mmm1`.
    if word & 0x001F == 0x000F && (word >> 8) & 0x00F1 == 0x0081 {
        let n = (word >> 5) & 0x7;
        let mm = (word >> 9) & 0x7;
        let prefix = if n == mm { "" } else { "Wrong format - " };
        return Some(format!("{prefix}{}ER{}", m("EXTBW"), n * 2));
    }
    // `rrrrrrrr 1100cccc` with every condition bit set: the table stops at `AL`
    // (condition 14), so 14 conditions of this row are mapped and one is not.
    if (word >> 8) & 0x00FF == 0x00CF {
        let displacement = sign_extend(word & 0x00FF, 8) * 2;
        let target = (address as i64 + 2 + displacement) as u32;
        return Some(format!("{}<Unrecognized>, {}h", m("BC"), hex(target, 5)));
    }
    None
}

/// The second operand of a two-operand byte instruction.
fn operand1(value: u16, size: u8) -> String {
    if size == 0 {
        format!("#{value}")
    } else {
        register(value, size)
    }
}

/// A `POP`/`PUSH` register list.
///
/// The bits are listed **highest first**, which is what the chart's variable
/// order spells out (`{l?LR, }{e?PSW, }{p?PC, }EA`): bit 3 `LR`, bit 2 `PSW`,
/// bit 1 `PC`, bit 0 `EA`.  `PUSH` swaps `PSW`/`PC` for `EPSW`/`ELR`.
fn render_list(mask: u8, push: bool) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if mask & 8 != 0 {
        parts.push("LR");
    }
    if mask & 4 != 0 {
        parts.push(if push { "EPSW" } else { "PSW" });
    }
    if mask & 2 != 0 {
        parts.push(if push { "ELR" } else { "PC" });
    }
    if mask & 1 != 0 {
        parts.push("EA");
    }
    parts.join(", ")
}

fn render_load_store(handler: &str, hint: u32, v0: u16, v1: u16, long_imm: u16) -> String {
    // The access width is the hint's secondary selector.  Both register fields
    // are already byte indices: for a 2-byte access the mask is `0xE`, which
    // captures the ER index pre-shifted, so nothing is multiplied here.
    let width = (hint >> 8) as u8;
    let name = if hint & H_ST == 0 { "L" } else { "ST" };
    let destination = register(v0, width);
    let inc = if hint & H_IA != 0 { "+" } else { "" };

    let source = match handler {
        "OP_LS_EA" => format!("[EA{inc}]"),
        "OP_LS_R" => format!("[{}]", register(v1, 2)),
        "OP_LS_I" => address_literal(long_imm as u32, 4),
        "OP_LS_I_R" => format!("{}h[{}]", signed_hex(long_imm, 16), register(v1, 2)),
        "OP_LS_BP" | "OP_LS_FP" => {
            let base = if handler == "OP_LS_BP" { "BP" } else { "FP" };
            format!("{}h[{base}]", signed_hex(v1, 6))
        }
        _ => "?".to_string(),
    };
    format!("{}{destination}, {source}", m(name))
}

fn render_ctrl(hint: u32, v0: u16, v1: u16) -> String {
    match hint >> 8 {
        1 => format!("{}ECSR, R{v1}", m("MOV")),
        2 => format!("{}ELR, {}", m("MOV"), register(v1, 2)),
        3 => format!("{}EPSW, R{v1}", m("MOV")),
        4 => format!("{}{}, ELR", m("MOV"), register(v0, 2)),
        5 => format!("{}{}, SP", m("MOV"), register(v0, 2)),
        6 | 7 => format!("{}PSW, #{v1}", m("MOV")),
        8 => format!("{}R{v0}, ECSR", m("MOV")),
        9 => format!("{}R{v0}, EPSW", m("MOV")),
        10 => format!("{}R{v0}, PSW", m("MOV")),
        11 => format!("{}SP, {}", m("MOV"), register(v1, 2)),
        other => format!("{}CTRL{other}", m("MOV")),
    }
}

/// `DSR<-` has three forms, and the operand descriptor is what distinguishes
/// them: an immediate (size 0), a byte register (size 1), or `DSR` itself
/// (`0xFE9F`, which carries no fields at all).
fn render_dsr(size0: u8, v0: u16, word: u16) -> String {
    if word == 0xFE9F {
        "DSR<-    DSR".to_string()
    } else if size0 == 0 {
        format!("DSR<-   {}", address_literal(v0 as u32, 2))
    } else {
        format!("DSR<-   R{v0}")
    }
}

/// `EI`/`DI`/`SC`/`RC` are `PSW | #imm` and `PSW & #imm`, so the mnemonic is the
/// opcode's own low byte, not the handler.
fn psw_name(word: u16) -> String {
    match word {
        0xED08 => m("EI").trim_end().to_string(),
        0xEBF7 => m("DI").trim_end().to_string(),
        0xED80 => m("SC").trim_end().to_string(),
        0xEB7F => m("RC").trim_end().to_string(),
        _ => format!("{}PSW, #{}h", m("MOV"), hex(word as u32 & 0xFF, 2)),
    }
}
