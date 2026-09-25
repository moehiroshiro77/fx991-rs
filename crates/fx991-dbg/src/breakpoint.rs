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

//! Breakpoints and watchpoints: what to stop on, and why.
//!
//! There are two kinds, and they are not interchangeable:
//!
//! | | matches | stops | condition sees |
//! |---|---|---|---|
//! | [`Breakpoint`] | an address about to execute | before the instruction | the state it is about to act on |
//! | [`Watch`] | an address being read or written | after the accessing instruction | the state after the access |
//!
//! A watch stops *after* the access on purpose: the instruction that wrote is the
//! interesting one, and the report should carry its PC.  Stopping before would
//! report the previous instruction, which is never what was meant.
//!
//! # Why this is not `fx991::Breakpoint`
//!
//! The scripting API's breakpoint records its [`fx991::Hit`] *after* the tick, so
//! its PC is where control went next rather than where it stopped.  A debugger
//! needs the address it stopped *at*.  The two coexist: `Emu::Breakpoint` remains
//! what a script uses, and this is what the debugger uses.  Both take
//! `Mode::Stop`-like semantics, where the instruction is suppressed so the machine
//! is left before it runs.

use crate::condition::Expr;

/// Identifies a breakpoint or watchpoint within a [`crate::Debugger`].
///
/// An id rather than an index: indices shift when one is removed, and a user
/// typing `be 2` after a removal would otherwise enable a different breakpoint
/// than they meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BreakpointId(pub u64);

/// Identifies a watchpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WatchId(pub u64);

/// How a breakpoint's address is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressKind {
    /// A full physical address, `(csr << 16) | pc`.
    Physical,
    /// A bare 16-bit PC, matching the low half of a physical address.
    ///
    /// This is the form the plans quote when they write `0x946A`, which is the
    /// reset vector in segment 0 and *also* an address in every other segment.
    Pc,
}

/// Which accesses a watchpoint cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchKind {
    /// A data read.
    Read,
    /// A data write.
    Write,
    /// Either direction.
    ReadWrite,
    /// A code fetch.
    Execute,
}

impl WatchKind {
    /// The short form a command line accepts: `r`, `w`, `rw`, `x`.
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.to_ascii_lowercase().as_str() {
            "r" | "read" => WatchKind::Read,
            "w" | "write" => WatchKind::Write,
            "rw" | "wr" | "readwrite" => WatchKind::ReadWrite,
            "x" | "exec" | "execute" => WatchKind::Execute,
            _ => return None,
        })
    }

    /// The short form to print.
    pub fn label(self) -> &'static str {
        match self {
            WatchKind::Read => "r",
            WatchKind::Write => "w",
            WatchKind::ReadWrite => "rw",
            WatchKind::Execute => "x",
        }
    }

    /// Turn into the bus's own flags.
    pub fn to_bus(self, address: u32) -> fx991_bus::Watch {
        fx991_bus::Watch {
            address,
            read: matches!(self, WatchKind::Read | WatchKind::ReadWrite),
            write: matches!(self, WatchKind::Write | WatchKind::ReadWrite),
            execute: matches!(self, WatchKind::Execute),
        }
    }
}

/// An execution breakpoint.
#[derive(Debug, Clone)]
pub struct Breakpoint {
    /// The address to stop at.
    pub address: u32,
    /// How to read the address.
    pub kind: AddressKind,
    /// Whether it is armed.
    pub enabled: bool,
    /// Disable it after the first hit.
    pub once: bool,
    /// Stop on arrival, or only record it.
    ///
    /// A logging breakpoint is how a trace of one address is collected without
    /// stopping the machine 40 000 times.
    pub log_only: bool,
    /// The `if` clause, or `None` for "always".
    pub condition: Option<Expr>,
    /// How many times it has matched, including hits it did not stop on.
    pub hits: u64,
    /// How many times it stopped the machine.
    pub stops: u64,
}

impl Breakpoint {
    /// A breakpoint on a physical address.
    pub fn physical(address: u32) -> Self {
        Self::new(address, AddressKind::Physical)
    }

    /// A breakpoint on a bare 16-bit PC.
    pub fn pc(address: u16) -> Self {
        Self::new(address as u32, AddressKind::Pc)
    }

    /// Auto-detect: `0x10000` and above is physical, below is a bare PC.
    ///
    /// Matches how the plans write addresses, and how `Emu::Breakpoint::auto`
    /// already behaves, so the two agree about what `0x946A` means.
    pub fn auto(address: u32) -> Self {
        let kind = if address >= 0x1_0000 {
            AddressKind::Physical
        } else {
            AddressKind::Pc
        };
        Self::new(address, kind)
    }

    fn new(address: u32, kind: AddressKind) -> Self {
        Self {
            address,
            kind,
            enabled: true,
            once: false,
            log_only: false,
            condition: None,
            hits: 0,
            stops: 0,
        }
    }

    /// Attach an `if` clause.
    pub fn with_condition(mut self, condition: Expr) -> Self {
        self.condition = Some(condition);
        self
    }

    /// Disable after the first hit.
    pub fn once(mut self) -> Self {
        self.once = true;
        self
    }

    /// Record arrivals without stopping.
    pub fn log_only(mut self) -> Self {
        self.log_only = true;
        self
    }

    /// Whether this breakpoint matches a physical PC.
    pub fn matches(&self, physical_pc: u32) -> bool {
        match self.kind {
            AddressKind::Physical => self.address == physical_pc,
            AddressKind::Pc => (self.address & 0xFFFF) == (physical_pc & 0xFFFF),
        }
    }

    /// How the address prints: `0F968h` for a PC, `201FA14h` for a full one.
    ///
    /// The two forms are distinguishable on sight, which matters because `0F968h`
    /// as a PC matches in every segment while `00F968h` does not.
    pub fn address_text(&self) -> String {
        match self.kind {
            AddressKind::Physical => format!("{:07X}h", self.address),
            AddressKind::Pc => format!("{:05X}h", self.address & 0xFFFF),
        }
    }

    /// Whether this breakpoint would stop the machine, as opposed to only logging.
    pub fn stops(&self) -> bool {
        !self.log_only
    }
}

/// A data watchpoint.
#[derive(Debug, Clone)]
pub struct Watch {
    /// The address being watched.
    pub address: u32,
    /// Which accesses are interesting.
    pub kind: WatchKind,
    /// Whether it is armed.
    pub enabled: bool,
    /// The `if` clause, or `None` for "always".
    pub condition: Option<Expr>,
    /// How many accesses matched.
    pub hits: u64,
    /// How many times it stopped the machine.
    pub stops: u64,
}

impl Watch {
    /// Watch an address in the given direction, enabled and unconditional.
    pub fn new(address: u32, kind: WatchKind) -> Self {
        Self {
            address,
            kind,
            enabled: true,
            condition: None,
            hits: 0,
            stops: 0,
        }
    }

    /// Attach an `if` clause.
    pub fn with_condition(mut self, condition: Expr) -> Self {
        self.condition = Some(condition);
        self
    }

    /// Whether this watch cares about accesses through the bus.
    pub fn bus_flags(&self) -> fx991_bus::Watch {
        self.kind.to_bus(self.address)
    }
}
