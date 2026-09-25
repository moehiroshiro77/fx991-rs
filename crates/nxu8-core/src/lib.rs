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

//! A clean-room nX-U8 CPU core.
//!
//! This crate is the *processor* only: registers, the decoded instruction table,
//! and the handlers.  It knows nothing about calculators, ROM images, or memory
//! maps -- memory access goes through the [`Memory`] trait and interrupts through
//! [`ops::ctrl::ControlSink`].  That is what lets it be reused and unit-tested
//! against hand-written programs, which is how most of the semantics here are
//! pinned down.
//!
//! # Provenance
//!
//! The semantics come from the OKI *nX-U8/100 Core Instruction Manual*
//! (FEZ0317A0-U8-INST-02), which specifies every instruction's operation, its
//! flag behaviour, and its operand encoding.  Where the manual leaves something
//! ambiguous the behaviour was established by observing the ROM, and the comment
//! says which.
//!
//! The manual is the authority for the flag rules in particular, because they are
//! not the obvious ones: `Z` is only ever *cleared* (`ZSCheck`), so an operation
//! that writes zero sets it while one that leaves the value alone does not.
//!
//! # Layout
//!
//! `text
//! regs    register file, PSW bit fields
//! opcodes generated opcode table
//! decode  hint flags + 65536-entry dispatch built from that table
//! alu     Add8 / ZSCheck / shifts
//! cpu     the Cpu struct and Next
//! ops/    the instruction handlers
//! `
//!
//! # Example
//!
//! ```text
//! use nxu8_core::{Cpu, Memory, NullMemory};
//!
//! let mut cpu = Cpu::new();
//! let mut bus = NullMemory::default();
//! let mut sink = nxu8_core::ops::ctrl::NullSink::default();
//! // `MOV R0,#1` then `RT`
//! bus.words = vec![0x0001, 0xFE1F];
//! cpu.regs.pc = 0;
//! cpu.step(&mut bus, &mut sink);
//! assert_eq!(cpu.regs.r[0], 1);
//! ```

// Public API documentation is expected, but a hard `deny` would drown the
// narrative docs in boilerplate on accessor methods.  Warnings keep it visible.
#![warn(missing_docs)]

pub mod alu;
pub mod cpu;
pub mod decode;
pub mod dispatch_digest;
pub mod opcodes;
pub mod ops;
pub mod regs;

pub use cpu::Cpu;
pub use regs::{Regs, PSW_C, PSW_ELEVEL, PSW_HC, PSW_MIE, PSW_OV, PSW_S, PSW_Z};

/// Memory as seen by the CPU.
///
/// The core only ever needs code halfwords and data bytes; everything about *where*
/// those live is the host's business.
pub trait Memory {
    /// Fetch a 16-bit halfword at a physical code address (`csr << 16 | pc`).
    fn read_code(&mut self, address: u32) -> u16;

    /// Read one data byte.
    fn read_data(&mut self, address: u32) -> u8;

    /// Write one data byte.
    fn write_data(&mut self, address: u32, value: u8);
}

/// A `Memory` that only holds a flat word array, for unit tests and examples.
#[derive(Debug, Default, Clone)]
pub struct NullMemory {
    /// Words served by [`Memory::read_code`], indexed by halfword address.
    pub words: Vec<u16>,
    /// Flat byte space served by the data accessors.
    pub bytes: Vec<u8>,
}

impl Memory for NullMemory {
    fn read_code(&mut self, address: u32) -> u16 {
        self.words.get((address / 2) as usize).copied().unwrap_or(0)
    }

    fn read_data(&mut self, address: u32) -> u8 {
        self.bytes.get(address as usize).copied().unwrap_or(0)
    }

    fn write_data(&mut self, address: u32, value: u8) {
        let index = address as usize;
        if index >= self.bytes.len() {
            self.bytes.resize(index + 1, 0);
        }
        self.bytes[index] = value;
    }
}

/// An opcode the core deliberately refuses to execute.
///
/// The only current source is the coprocessor transfer group, which VerF's ROM
/// never uses.  Recording it rather than panicking lets a debugger report the
/// address and keep going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unimplemented {
    /// The offending opcode.
    pub opcode: u16,
    /// PC at the fault.
    pub pc: u16,
    /// CSR at the fault.
    pub csr: u16,
}

impl Unimplemented {
    /// The physical address the fault occurred at.
    pub fn address(&self) -> u32 {
        ((self.csr as u32) << 16) | self.pc as u32
    }
}

impl std::fmt::Display for Unimplemented {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unimplemented opcode {:#06x} at {:#05x}",
            self.opcode,
            self.address()
        )
    }
}

impl std::error::Error for Unimplemented {}

/// A bus fault: a code or data access outside the mapped space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryFault {
    /// The address that faulted.
    pub address: u32,
    /// True for a write, false for a read.
    pub write: bool,
}

impl std::fmt::Display for MemoryFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = if self.write { "write to" } else { "read from" };
        write!(f, "{kind} unmapped address {:#05x}", self.address)
    }
}

impl std::error::Error for MemoryFault {}
