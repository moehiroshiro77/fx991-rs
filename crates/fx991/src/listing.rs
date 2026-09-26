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

//! The verified disassembly listing: `data/_disas_verF.txt`.
//!
//! The listing is the project's single source of truth for "what instruction is at
//! this address" (: 116944 instructions, 100 % byte-consistent with the
//! ROM).  Reading it is both cheaper and more trustworthy than re-deriving the
//! encoding tables, so this crate reuses it rather than decoding bytes a second
//! time.

use std::collections::HashMap;

/// One listing line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Physical address of the instruction.
    pub address: u32,
    /// The instruction's bytes, exactly as the ROM holds them.
    pub bytes: Vec<u8>,
    /// Disassembled text, e.g. `BC      AL, 0946Eh`.
    pub text: String,
}

/// A parsed listing.
pub struct Listing {
    entries: Vec<Entry>,
    by_address: HashMap<u32, usize>,
}

impl Listing {
    /// Parse listing text.  Unparsable lines are skipped rather than aborting: the
    /// file's job is to be a reference, not a strict format.
    pub fn parse(text: &str) -> Self {
        let mut entries = Vec::new();
        let mut by_address = HashMap::new();

        for line in text.lines() {
            let Some(entry) = parse_line(line) else {
                continue;
            };
            by_address.insert(entry.address, entries.len());
            entries.push(entry);
        }

        Self {
            entries,
            by_address,
        }
    }

    /// Every parsed instruction, in address order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The instruction at an exact address.
    pub fn at(&self, address: u32) -> Option<&Entry> {
        self.by_address
            .get(&address)
            .map(|index| &self.entries[*index])
    }

    /// The instruction whose bytes are at `address`, or `None`.
    ///
    /// Unlike [`Listing::at`], a miss returns a raw byte rendering instead of
    /// nothing, so a caller can still see what is there.
    pub fn disassemble(&self, address: u32, count: usize) -> String {
        let mut out = String::new();
        let mut pc = address & !1;
        for _ in 0..count {
            match self.at(pc) {
                Some(entry) => {
                    let hex: Vec<String> = entry
                        .bytes
                        .iter()
                        .map(|byte| format!("{byte:02X}"))
                        .collect();
                    out.push_str(&format!(
                        "{pc:06X}  {:<12}  {}\n",
                        hex.join(" "),
                        entry.text
                    ));
                    pc += entry.bytes.len() as u32;
                }
                None => {
                    out.push_str(&format!("{pc:06X}  <no listing>\n"));
                    pc += 2;
                }
            }
        }
        out
    }

    /// How many instructions the listing holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing parsed.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn parse_line(line: &str) -> Option<Entry> {
    let line = line.trim_end();
    if line.len() < 9 {
        return None;
    }
    // `split_at` panics when the index is inside a multi-byte character, and this
    // reads a user-supplied file, so a stray non-ASCII byte must skip the line
    // rather than abort the parse.  `split_at_checked` is the fallible form.
    let (address_part, rest) = line.split_at_checked(6)?;
    let address = u32::from_str_radix(address_part, 16).ok()?;

    // The byte field ends at the first run of two or more spaces; its own bytes are
    // separated by single spaces.  Splitting on whitespace instead would read a
    // mnemonic like `BC` as a byte, since those are valid hex digits too.
    let rest = rest.trim_start();
    let split = rest.find("  ")?;
    let (bytes_part, text_part) = rest.split_at(split);

    let mut bytes = Vec::new();
    for token in bytes_part.split(' ') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        bytes.push(u8::from_str_radix(token, 16).ok()?);
    }
    let text = text_part.split_whitespace().collect::<Vec<_>>().join(" ");

    if bytes.is_empty() {
        return None;
    }
    Some(Entry {
        address,
        bytes,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
000000   00 F0 6A 94        B       00h:0946Ah
00946A   01 CE              BC      AL, 0946Eh
0221B8   CE F8              PUSH    LR
";

    #[test]
    fn a_line_parses_into_address_bytes_and_text() {
        let listing = Listing::parse(SAMPLE);
        let entry = listing.at(0x000000).unwrap();
        assert_eq!(entry.bytes, vec![0x00, 0xF0, 0x6A, 0x94]);
        assert_eq!(entry.text, "B 00h:0946Ah");
    }

    #[test]
    fn a_two_byte_instruction_parses_too() {
        let listing = Listing::parse(SAMPLE);
        let entry = listing.at(0x00946A).unwrap();
        assert_eq!(entry.bytes, vec![0x01, 0xCE]);
        assert_eq!(entry.text, "BC AL, 0946Eh");
    }

    #[test]
    fn disassembling_walks_by_instruction_length() {
        // Build a contiguous sample: a 4-byte `B` then two 2-byte instructions, so
        // the walk has to use each instruction's own length to find the next.
        let sample = "000000   00 F0 6A 94        B       00h:0946Ah
000004   6C 94              SRL     R4, #6
000006   60 00              MOV     R0, #96
";
        let listing = Listing::parse(sample);
        let text = listing.disassemble(0x000000, 3);
        assert!(text.contains("000000"), "{text}");
        assert!(
            text.contains("000004"),
            "the 4-byte B must advance by four: {text}"
        );
        assert!(text.contains("000006"), "{text}");
    }

    #[test]
    fn a_line_whose_address_field_is_not_ascii_is_skipped() {
        // The address field ends at byte 6, and this reads a user-supplied file, so
        // a multi-byte character straddling that boundary must skip the line rather
        // than abort the whole parse.  `e` with an acute accent is two bytes, so it
        // starts at 5 and byte 6 is inside it.
        let text =
            "00000\u{e9}  00 00  MOV R0, #0\n000000   00 F0 6A 94        B       00h:0946Ah\n";
        assert!(
            !text.is_char_boundary(6),
            "the sample must split a character, or it proves nothing"
        );
        let listing = Listing::parse(text);
        // The bad line is dropped and the good one still parses.
        assert_eq!(listing.entries().len(), 1);
        assert_eq!(
            listing.at(0x000000).unwrap().bytes,
            vec![0x00, 0xF0, 0x6A, 0x94]
        );
    }

    #[test]
    fn an_address_with_no_listing_is_marked_as_such() {
        let listing = Listing::parse(SAMPLE);
        assert!(listing.disassemble(0x001234, 1).contains("<no listing>"));
    }
}
