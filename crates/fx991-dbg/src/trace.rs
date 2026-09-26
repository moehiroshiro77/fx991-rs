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

//! A bounded record of executed instructions.
//!
//! The ROM runs at roughly two million instructions per second, so an unbounded
//! trace is a memory leak with a fuse rather than a debugging aid.  This keeps the
//! most recent `capacity` entries and drops the oldest, which is what "what led up
//! to this" actually needs.
//!
//! # What each entry records
//!
//! The address, the handler name the core ran, the stack pointer, and which
//! registers that instruction changed.  The register changes are computed by
//! comparing the snapshots taken either side of the instruction, so they answer the
//! question a trace is usually opened to answer: *which* instruction moved the
//! value being chased.
//!
//! A trace is meant to be left running, so nothing here does more work per
//! instruction than it has to: no formatting, no lookup, and one allocation --
//! the list of changed registers, which is at most sixteen small tuples and is
//! empty for most instructions.  Growing that list is the only per-instruction
//! allocation, and `Vec` is what makes the alternative below worth not taking:
//! an inline array would remove it at the cost of a wider entry for every
//! instruction, including the many that changed nothing.

use std::collections::VecDeque;

/// What a traced instruction looked like on the way in and the way out.
///
/// A struct rather than a parameter list because a sample has seven facts in it and
/// the two register snapshots are easy to transpose when they are positional.
#[derive(Debug, Clone, Copy)]
pub struct Sample<'a> {
    /// The tick the instruction retired on.
    pub tick: u64,
    /// The physical PC it executed at.
    pub pc: u32,
    /// The decode table's handler name, or `None` for an unmapped opcode.
    pub handler: Option<&'static str>,
    /// The byte registers before the instruction.
    pub registers_before: &'a [u8; 16],
    /// The byte registers after it.
    pub registers_after: &'a [u8; 16],
    /// The stack pointer after it.
    pub sp: u16,
    /// The PSW after it.
    pub psw: u8,
}

/// One traced instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEntry {
    /// The tick the instruction retired on.
    pub tick: u64,
    /// The physical PC it executed at.
    pub pc: u32,
    /// The decode table's handler name, or `None` for an unmapped opcode.
    pub handler: Option<&'static str>,
    /// The stack pointer after the instruction.
    pub sp: u16,
    /// The PSW after the instruction.
    pub psw: u8,
    /// Byte registers this instruction changed, as `(index, before, after)`.
    pub changed: Vec<(u8, u8, u8)>,
}

impl TraceEntry {
    /// The changed registers as text, e.g. `R0=41>42 R4=00>01`.
    pub fn changed_text(&self) -> String {
        self.changed
            .iter()
            .map(|(index, before, after)| format!("R{index}={before:02X}>{after:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The mnemonic the core ran, or a readable stand-in.
    pub fn handler_text(&self) -> &str {
        self.handler.unwrap_or("-")
    }
}

/// A ring buffer of recent instructions.
#[derive(Debug, Clone)]
pub struct TraceRing {
    entries: VecDeque<TraceEntry>,
    capacity: usize,
    /// How many instructions have been traced in total, including dropped ones.
    recorded: u64,
}

impl TraceRing {
    /// A ring holding at most `capacity` entries.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity.min(4096)),
            capacity: capacity.max(1),
            recorded: 0,
        }
    }

    /// How many entries are held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing has been traced.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// How many instructions have been traced in total.
    pub fn recorded(&self) -> u64 {
        self.recorded
    }

    /// True when entries have been dropped to stay within the capacity.
    pub fn has_dropped(&self) -> bool {
        self.recorded > self.entries.len() as u64
    }

    /// Forget every entry, keeping the capacity.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.recorded = 0;
    }

    /// Change the capacity, dropping the oldest entries if it shrank.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity.max(1);
        while self.entries.len() > self.capacity {
            self.entries.pop_front();
        }
    }

    /// Record an executed instruction.
    ///
    /// The sample carries both register snapshots, taken around the instruction, so
    /// the recorded change is *this* instruction's doing.  Taking only the state
    /// afterwards and comparing against the previous entry would leave the very
    /// first entry unable to say what it changed, which is the entry a "what just
    /// happened" question most often wants.
    pub fn record(&mut self, sample: Sample<'_>) {
        let mut changed = Vec::new();
        for index in 0..16 {
            let before = sample.registers_before[index];
            let after = sample.registers_after[index];
            if before != after {
                changed.push((index as u8, before, after));
            }
        }

        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(TraceEntry {
            tick: sample.tick,
            pc: sample.pc,
            handler: sample.handler,
            sp: sample.sp,
            psw: sample.psw,
            changed,
        });
        self.recorded += 1;
    }

    /// The entries, oldest first.
    pub fn entries(&self) -> impl Iterator<Item = &TraceEntry> {
        self.entries.iter()
    }

    /// The most recent `count` entries, oldest first.
    pub fn tail(&self, count: usize) -> Vec<&TraceEntry> {
        let skip = self.entries.len().saturating_sub(count);
        self.entries.iter().skip(skip).collect()
    }

    /// Render as CSV, with the header.
    ///
    /// The column names match `fx991::Trace`'s where the fields overlap, so a
    /// trace from either path can be diffed or plotted the same way.
    ///
    /// Every field is escaped, not just the ones expected to need it: a comma in a
    /// handler name or a quote in a register list would otherwise shift the columns
    /// or truncate the row, and a malformed trace is worse than a verbose one.
    pub fn to_csv(&self) -> String {
        let mut out = String::from("tick,pc,handler,sp,psw,changed\n");
        for entry in &self.entries {
            out.push_str(&format!(
                "{},{},{},{},{},{}\n",
                entry.tick,
                entry.pc,
                escape_csv(entry.handler_text()),
                entry.sp,
                entry.psw,
                escape_csv(&entry.changed_text())
            ));
        }
        out
    }

    /// The addresses this trace visited, in order, for a coverage-style summary.
    pub fn addresses(&self) -> Vec<u32> {
        self.entries.iter().map(|entry| entry.pc).collect()
    }
}

/// Quote a field for CSV, per RFC 4180: wrap it when it holds a delimiter, a quote
/// or a newline, and double any quote inside.
///
/// A field that needs none of that is emitted bare, so the common case stays
/// readable and the file is still valid.
fn escape_csv(field: &str) -> String {
    if !field.contains([',', '"', '\n', '\r']) {
        return field.to_string();
    }
    let mut out = String::with_capacity(field.len() + 2);
    out.push('"');
    for character in field.chars() {
        if character == '"' {
            out.push('"');
        }
        out.push(character);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Record an instruction whose only effect is to put `r0` in R0.
    ///
    /// `from` is what R0 held before, so a sequence reads as a chain.
    fn record(ring: &mut TraceRing, tick: u64, pc: u32, from: u8, r0: u8) {
        let mut before = [0u8; 16];
        before[0] = from;
        let mut after = [0u8; 16];
        after[0] = r0;
        ring.record(Sample {
            tick,
            pc,
            handler: Some("OP_MOV"),
            registers_before: &before,
            registers_after: &after,
            sp: 0xF000,
            psw: 0,
        });
    }

    #[test]
    fn a_ring_keeps_the_most_recent_entries() {
        let mut ring = TraceRing::new(3);
        for index in 0..5 {
            record(&mut ring, index, 0x1000 + index as u32 * 2, 0, 0);
        }
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.recorded(), 5);
        assert!(ring.has_dropped());

        let pcs: Vec<u32> = ring.entries().map(|entry| entry.pc).collect();
        assert_eq!(pcs, vec![0x1004, 0x1006, 0x1008], "oldest two dropped");
    }

    #[test]
    fn a_ring_that_has_not_wrapped_reports_no_loss() {
        let mut ring = TraceRing::new(10);
        for index in 0..3 {
            record(&mut ring, index, 0x1000, 0, 0);
        }
        assert_eq!(ring.len(), 3);
        assert!(!ring.has_dropped());
    }

    #[test]
    fn every_entry_reports_what_that_instruction_changed() {
        // Including the first: the snapshots are taken around the instruction, so
        // there is no baseline to be missing.
        let mut ring = TraceRing::new(8);
        record(&mut ring, 0, 0x1000, 0x00, 0x11);
        assert_eq!(
            ring.entries().next().unwrap().changed,
            vec![(0, 0x00, 0x11)]
        );

        record(&mut ring, 1, 0x1002, 0x11, 0x42);
        let second = ring.entries().nth(1).unwrap();
        assert_eq!(second.changed, vec![(0, 0x11, 0x42)]);
        assert_eq!(second.changed_text(), "R0=11>42");
    }

    #[test]
    fn an_instruction_that_changed_nothing_reports_nothing() {
        let mut ring = TraceRing::new(4);
        ring.record(Sample {
            tick: 0,
            pc: 0x1000,
            handler: Some("OP_NOP"),
            registers_before: &[0u8; 16],
            registers_after: &[0u8; 16],
            sp: 0,
            psw: 0,
        });
        assert!(ring.entries().next().unwrap().changed.is_empty());
    }

    #[test]
    fn a_register_that_did_not_move_is_not_listed() {
        let mut ring = TraceRing::new(8);
        // The second instruction leaves R0 where it was.
        let registers = [0x42u8; 16];
        ring.record(Sample {
            tick: 0,
            pc: 0x1000,
            handler: Some("OP_MOV"),
            registers_before: &[0u8; 16],
            registers_after: &registers,
            sp: 0,
            psw: 0,
        });
        ring.record(Sample {
            tick: 1,
            pc: 0x1002,
            handler: Some("OP_NOP"),
            registers_before: &registers,
            registers_after: &registers,
            sp: 0,
            psw: 0,
        });
        assert!(
            ring.entries().nth(1).unwrap().changed.is_empty(),
            "nothing moved"
        );
    }

    #[test]
    fn clearing_resets_the_counters() {
        let mut ring = TraceRing::new(4);
        record(&mut ring, 0, 0x1000, 0, 1);
        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.recorded(), 0);
        assert!(!ring.has_dropped());

        // The counters restart, and a fresh entry still reports its own change.
        record(&mut ring, 1, 0x2000, 0x00, 9);
        assert_eq!(ring.entries().next().unwrap().changed, vec![(0, 0x00, 9)]);
    }

    #[test]
    fn shrinking_the_capacity_drops_the_oldest() {
        let mut ring = TraceRing::new(8);
        for index in 0..6 {
            record(&mut ring, index, 0x1000 + index as u32, 0, 0);
        }
        ring.set_capacity(2);
        assert_eq!(ring.len(), 2);
        let pcs: Vec<u32> = ring.entries().map(|entry| entry.pc).collect();
        assert_eq!(pcs, vec![0x1004, 0x1005]);
    }

    #[test]
    fn a_zero_capacity_is_clamped_rather_than_panicking() {
        let mut ring = TraceRing::new(0);
        assert_eq!(ring.capacity(), 1);
        record(&mut ring, 0, 0x1000, 0, 0);
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn the_tail_is_the_most_recent_entries_oldest_first() {
        let mut ring = TraceRing::new(10);
        for index in 0..5 {
            record(&mut ring, index, 0x1000 + index as u32, 0, 0);
        }
        let pcs: Vec<u32> = ring.tail(2).iter().map(|entry| entry.pc).collect();
        assert_eq!(pcs, vec![0x1003, 0x1004]);
        assert_eq!(ring.tail(99).len(), 5, "asking for more than there is");
    }

    #[test]
    fn the_csv_has_a_header_and_one_line_per_entry() {
        let mut ring = TraceRing::new(4);
        record(&mut ring, 7, 0x0_946A, 0x00, 0x11);
        record(&mut ring, 8, 0x0_946C, 0x11, 0x22);

        let csv = ring.to_csv();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3, "header plus two entries");
        assert!(lines[0].starts_with("tick,pc,handler,sp,psw,changed"));
        // Plain decimal for the numbers: a spreadsheet reads the column as a
        // number, which an `0x`-prefixed field would not be.
        assert!(lines[1].starts_with("7,37994,"), "{}", lines[1]);
        assert!(lines[1].ends_with(",R0=00>11"), "{}", lines[1]);
        assert!(lines[2].contains("R0=11>22"), "{}", lines[2]);
        // Six fields per row, so the columns line up.
        for line in &lines[1..] {
            assert_eq!(line.split(',').count(), 6, "{line}");
        }
    }

    #[test]
    fn a_field_that_needs_quoting_gets_it() {
        // The changed list can never hold a comma today, but the handler name is a
        // public field and a row with the wrong number of columns is a malformed
        // file rather than a cosmetic problem.
        assert_eq!(escape_csv("POP PC"), "POP PC", "nothing to escape");
        assert_eq!(escape_csv("a,b"), "\"a,b\"");
        assert_eq!(escape_csv("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape_csv("two\nlines"), "\"two\nlines\"");

        let mut ring = TraceRing::new(1);
        ring.record(Sample {
            tick: 1,
            pc: 2,
            handler: Some("WEIRD,NAME"),
            registers_before: &[0u8; 16],
            registers_after: &[0u8; 16],
            sp: 0,
            psw: 0,
        });
        let csv = ring.to_csv();
        let row = csv.lines().nth(1).expect("one entry");
        assert!(row.contains("\"WEIRD,NAME\""), "{row}");
        // Still six columns: the comma stayed inside the quotes.
        assert_eq!(row.matches(',').count(), 6, "{row}");
    }

    #[test]
    fn an_unmapped_instruction_traces_with_a_placeholder_not_a_blank() {
        let mut ring = TraceRing::new(2);
        ring.record(Sample {
            tick: 0,
            pc: 0x1000,
            handler: None,
            registers_before: &[0u8; 16],
            registers_after: &[0u8; 16],
            sp: 0,
            psw: 0,
        });
        assert_eq!(ring.entries().next().unwrap().handler_text(), "-");
    }
}
