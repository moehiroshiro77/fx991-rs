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

//! The scripting surface: breakpoints, bounded runs, tracing, and snapshots.
//!
//! The experiments in  all read the same way -- "run until X,
//! look at the registers" -- so that loop is the API rather than something each
//! script reimplements.
//!
//! Three details matter and are easy to get wrong:
//!
//! * **`run_until` needs a tick budget.**  The ROM parks in a STOP loop for most of
//!   its life, so an unbudgeted "run until X" hangs a script instead of reporting
//!   that X was never reached.
//! * **Interrupt dispatch can move PC inside a tick.**  The reset vector at `0x946A`
//!   only exists between dispatch and the fetch, so a breakpoint there has to
//!   *suppress* the instruction to be observable.  That is [`Mode::Stop`].
//! * **Hooks borrow rather than own.**  [`Emu::step_with`] and [`Emu::run_with`] take
//!   `&mut dyn FnMut(u32) -> bool`, so a script can accumulate into a local variable
//!   without the `'static` bound that storing boxed listeners would impose.
//!   [`Breakpoint`] additionally records its own hits, which covers the common
//!   "collect every arrival" case with no closure at all.

use fx991_chipset::{Chipset, TickOutcome};
use nxu8_core::Regs;

/// What an address means in a breakpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressKind {
    /// A 20-bit physical address, `(csr << 16) | pc`.
    Physical,
    /// A bare 16-bit PC, matching how the plans quote `0x946A` versus `rom:221B8`.
    Pc,
}

/// Whether a breakpoint observes or stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Fire and let the instruction run.  Fires once per arrival.
    Observe,
    /// Fire and suppress the instruction, so the caller sees the PC itself.
    Stop,
}

/// One recorded hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Tick the hit happened on.
    pub tick: u64,
    /// Physical PC at the hit.
    pub pc: u32,
    /// Stack pointer at the hit.
    pub sp: u16,
    /// `ER0` at the hit; the first argument register of the calling convention.
    pub er0: u16,
}

/// How many hits a breakpoint records by default.
pub const DEFAULT_HIT_LIMIT: usize = 64;

/// A watch on an address.
pub struct Breakpoint {
    /// The address to watch.
    pub address: u32,
    /// How to interpret it.
    pub kind: AddressKind,
    /// Whether the instruction is suppressed on a hit.
    pub mode: Mode,
    /// Disable after the first hit.
    pub once: bool,
    /// Whether the breakpoint is still armed.
    pub enabled: bool,
    /// How many times it matched, including hits past the recording limit.
    pub hits: u64,
    /// The first [`Breakpoint::hit_limit`] hits, in order.
    pub recorded: Vec<Hit>,
    /// How many hits to record.
    pub hit_limit: usize,
}

impl Breakpoint {
    /// A breakpoint on a physical address, observing.
    pub fn physical(address: u32) -> Self {
        Self::with_kind(address, AddressKind::Physical)
    }

    /// A breakpoint on a bare PC, observing.
    pub fn pc(address: u16) -> Self {
        Self::with_kind(address as u32, AddressKind::Pc)
    }

    /// Auto-detect the kind: anything at or above `0x10000` is a physical address,
    /// matching how the plans write them.
    pub fn auto(address: u32) -> Self {
        let kind = if address >= 0x1_0000 {
            AddressKind::Physical
        } else {
            AddressKind::Pc
        };
        Self::with_kind(address, kind)
    }

    fn with_kind(address: u32, kind: AddressKind) -> Self {
        Self {
            address,
            kind,
            mode: Mode::Observe,
            once: false,
            enabled: true,
            hits: 0,
            recorded: Vec::new(),
            hit_limit: DEFAULT_HIT_LIMIT,
        }
    }

    /// Stop instead of just observing.
    pub fn stopping(mut self) -> Self {
        self.mode = Mode::Stop;
        self
    }

    /// Disable after the first hit.
    pub fn once(mut self) -> Self {
        self.once = true;
        self
    }

    /// Keep at most `limit` hit records.
    pub fn limit(mut self, limit: usize) -> Self {
        self.hit_limit = limit;
        self
    }

    /// Whether this breakpoint matches a physical PC.
    pub fn matches(&self, physical_pc: u32) -> bool {
        match self.kind {
            AddressKind::Physical => self.address == physical_pc,
            AddressKind::Pc => (self.address & 0xFFFF) == (physical_pc & 0xFFFF),
        }
    }

    /// Bump the hit counter.  Records are appended later, once the chipset borrow
    /// from the tick is over.
    fn bump(&mut self) {
        self.hits += 1;
    }
}

/// A traced register set, written as CSV.
pub struct Trace {
    fields: Vec<String>,
    lines: Vec<String>,
}

impl Trace {
    /// Start a trace over the named registers.
    pub fn new(fields: &[&str]) -> Self {
        Self {
            fields: fields.iter().map(|f| (*f).to_string()).collect(),
            lines: Vec::new(),
        }
    }

    /// The header line.
    pub fn header(&self) -> String {
        format!("tick,{}", self.fields.join(","))
    }

    /// Record one sample.
    pub fn record(&mut self, tick: u64, values: &[String]) {
        self.lines.push(format!("{tick},{}", values.join(",")));
    }

    /// The number of recorded samples.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// True when nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The whole file contents.
    pub fn to_csv(&self) -> String {
        let mut out = self.header();
        out.push('\n');
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

/// Raised when a bounded run spends its budget without arriving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetExceeded {
    /// The address that was never reached.
    pub address: u32,
    /// The budget that ran out.
    pub budget: u64,
    /// Where the machine actually was.
    pub at: u32,
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "run_until({:#x}) exceeded {} ticks (at {:#07x})",
            self.address, self.budget, self.at
        )
    }
}

impl std::error::Error for BudgetExceeded {}

/// Everything about the emulator that a snapshot covers.
///
/// Breakpoints are deliberately *not* part of it: they are the debugger's own
/// bookkeeping rather than machine state, so rolling the machine back should not
/// resurrect a breakpoint that was cleared in between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmuSnapshot {
    /// The machine.
    pub chipset: fx991_chipset::ChipsetSnapshot,
    /// Ticks elapsed since the last reset.
    pub ticks: u64,
}

/// The emulator façade.
pub struct Emu {
    /// The machine.
    pub chipset: Chipset,
    /// Instructions ticked since the last reset.
    pub ticks: u64,
    breakpoints: Vec<Breakpoint>,
    /// Hits whose breakpoint fired during the tick being processed, with the
    /// register state captured at the moment it fired.
    pending_hits: Vec<(usize, Hit)>,
}

impl Emu {
    /// Wrap a chipset and perform a reset.
    pub fn new(chipset: Chipset) -> Self {
        let mut emu = Self {
            chipset,
            ticks: 0,
            breakpoints: Vec::new(),
            pending_hits: Vec::new(),
        };
        emu.reset();
        emu
    }

    /// Build from ROM bytes.
    pub fn from_rom(rom: Vec<u8>) -> Self {
        Self::new(Chipset::new(fx991_bus::Bus::new(fx991_bus::Rom::new(rom))))
    }

    /// Load a ROM image from a path.
    pub fn load_rom(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        Ok(Self::from_rom(std::fs::read(path)?))
    }

    /// `Chipset::Reset` plus the tick counter.
    pub fn reset(&mut self) {
        self.chipset.reset();
        self.ticks = 0;
        self.pending_hits.clear();
    }

    // ------------------------------------------------------------ registers
    /// The physical PC.
    pub fn pc(&self) -> u32 {
        self.chipset.cpu.regs.physical_pc()
    }

    /// The stack pointer.
    pub fn sp(&self) -> u16 {
        self.chipset.cpu.regs.sp
    }

    /// Set the stack pointer, rounding down to even as the hardware does.
    pub fn set_sp(&mut self, sp: u16) {
        self.chipset.cpu.regs.sp = sp & 0xFFFE;
    }

    /// Read a byte register.
    pub fn reg(&self, index: usize) -> u8 {
        self.chipset.cpu.regs.r[index & 0xF]
    }

    /// Write a byte register.
    pub fn set_reg(&mut self, index: usize, value: u8) {
        self.chipset.cpu.regs.r[index & 0xF] = value;
    }

    /// Read a 16-bit register by its byte index.
    pub fn er(&self, index: usize) -> u16 {
        self.chipset.cpu.regs.er(index)
    }

    /// Write a 16-bit register by its byte index.
    pub fn set_er(&mut self, index: usize, value: u16) {
        self.chipset.cpu.regs.set_er(index, value);
    }

    /// The program status word.
    pub fn psw(&self) -> u8 {
        self.chipset.cpu.regs.psw()
    }

    // --------------------------------------------------------------- memory
    /// Read one data byte.
    pub fn peek(&mut self, address: u32) -> u8 {
        use nxu8_core::Memory;
        self.chipset.bus.read_data(address)
    }

    /// Read one data byte without recording a fault or tripping a watchpoint.
    ///
    /// This is what a debugger's own inspection uses: the guest did not make the
    /// access, so it must not appear in the fault log and must not fire a
    /// watchpoint by itself.
    pub fn peek_quiet(&mut self, address: u32) -> u8 {
        self.chipset.bus.peek_quiet(address)
    }

    /// [`Emu::peek_quiet`] over a range.
    pub fn peek_quiet_block(&mut self, address: u32, length: usize) -> Vec<u8> {
        self.chipset.bus.peek_quiet_block(address, length)
    }

    /// Fetch a code halfword without recording a fault or tripping a watchpoint.
    ///
    /// What the disassembler view uses, so scrolling it cannot perturb the
    /// machine's fault bookkeeping.
    pub fn read_code_quiet(&mut self, address: u32) -> u16 {
        self.chipset.bus.read_code_quiet(address)
    }

    /// Read a block of data bytes.
    ///
    /// The range wraps at the top of the address space rather than running off the
    /// end of it.
    pub fn peek_block(&mut self, address: u32, length: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(length);
        for offset in 0..length as u32 {
            out.push(self.peek(address.wrapping_add(offset)));
        }
        out
    }

    /// Write one data byte.
    pub fn poke(&mut self, address: u32, value: u8) {
        use nxu8_core::Memory;
        self.chipset.bus.write_data(address, value)
    }

    /// Write a block of data bytes.
    ///
    /// The range wraps at the top of the address space, like [`Emu::peek_block`].
    pub fn poke_block(&mut self, address: u32, bytes: &[u8]) {
        for (offset, byte) in bytes.iter().enumerate() {
            self.poke(address.wrapping_add(offset as u32), *byte);
        }
    }

    /// Plant code that fetches see instead of ROM.
    pub fn load_code(&mut self, address: u32, blob: &[u8]) {
        self.chipset.bus.load_code(address, blob);
    }

    /// The battery-backed RAM.
    pub fn ram(&self) -> &[u8] {
        self.chipset.bus.ram()
    }

    /// Restore the RAM, the way a battery-backed calculator keeps it across a power
    /// cycle.
    pub fn set_ram(&mut self, bytes: &[u8]) {
        let ram = self.chipset.bus.ram_mut();
        let length = bytes.len().min(ram.len());
        ram[..length].copy_from_slice(&bytes[..length]);
    }

    // ------------------------------------------------------------- snapshots
    /// The whole machine's state, for a snapshot or a diff.
    pub fn snapshot(&self) -> EmuSnapshot {
        EmuSnapshot {
            chipset: self.chipset.snapshot(),
            ticks: self.ticks,
        }
    }

    /// Restore a snapshot.
    ///
    /// The tick counter goes back too: it is the unit a debugger steppers and
    /// traces in, so leaving it advanced after a rollback would make every
    /// subsequent record carry a time that never happened.
    pub fn restore(&mut self, snapshot: &EmuSnapshot) {
        self.chipset.restore(&snapshot.chipset);
        self.ticks = snapshot.ticks;
        self.pending_hits.clear();
    }

    // -------------------------------------------------------------- stepping
    /// Execute one tick, consulting any armed breakpoints.
    pub fn step(&mut self) -> TickOutcome {
        let mut no_extra = |_pc: u32, _regs: &Regs, _bus: &mut fx991_bus::Bus| false;
        self.step_with_bus(&mut no_extra)
    }

    /// Execute one tick with a caller-supplied hook over the physical PC.
    ///
    /// The hook may borrow local state, which is what makes `hits.push(.)` style
    /// accumulation possible without a `'static` bound.  Returning `true` suppresses
    /// the instruction -- see [`Mode::Stop`].
    pub fn step_with(&mut self, hook: &mut dyn FnMut(u32) -> bool) -> TickOutcome {
        let mut adapter = |pc: u32, _regs: &Regs, _bus: &mut fx991_bus::Bus| hook(pc);
        self.step_with_bus(&mut adapter)
    }

    /// [`Emu::step_with`], with the registers and the bus visible to the hook.
    ///
    /// A debugger needs the bus here: a conditional breakpoint on `[0xD180] == 0xFF`
    /// has to read the data space *before* the instruction runs, and re-running the
    /// instruction to re-test the condition is not equivalent because a tick
    /// advances the timer divider.
    pub fn step_with_bus(
        &mut self,
        hook: &mut dyn FnMut(u32, &Regs, &mut fx991_bus::Bus) -> bool,
    ) -> TickOutcome {
        let tick = self.ticks + 1;
        self.pending_hits.clear();

        let outcome = {
            let breakpoints = &mut self.breakpoints;
            let pending = &mut self.pending_hits;
            let mut combined = |pc: u32, regs: &Regs, bus: &mut fx991_bus::Bus| -> bool {
                let mut stop = false;
                for (index, bp) in breakpoints.iter_mut().enumerate() {
                    if !bp.enabled || !bp.matches(pc) {
                        continue;
                    }
                    if bp.once {
                        bp.enabled = false;
                    }
                    if bp.mode == Mode::Stop {
                        stop = true;
                    }
                    bp.bump();
                    // The registers describe the instruction about to run here and
                    // nowhere later, so the hit is recorded now rather than after
                    // the tick.
                    pending.push((
                        index,
                        Hit {
                            tick,
                            pc,
                            sp: regs.sp,
                            er0: regs.er(0),
                        },
                    ));
                }
                let caller_stop = hook(pc, regs, bus);
                stop || caller_stop
            };
            self.chipset.tick(Some(&mut combined))
        };

        self.ticks = tick;

        // File the hits now that the chipset borrow is over.
        for (index, snapshot) in std::mem::take(&mut self.pending_hits) {
            let bp = &mut self.breakpoints[index];
            if bp.recorded.len() < bp.hit_limit {
                bp.recorded.push(snapshot);
            }
        }

        outcome
    }

    /// Run `count` ticks.
    pub fn run(&mut self, count: u64) {
        for _ in 0..count {
            self.step();
        }
    }

    /// Run `count` ticks with a per-tick hook.
    pub fn run_with(&mut self, count: u64, hook: &mut dyn FnMut(u32) -> bool) {
        for _ in 0..count {
            self.step_with(hook);
        }
    }

    /// Run until the physical PC reaches `address`, inside a tick budget.
    ///
    /// The budget is mandatory in spirit: the ROM parks in STOP for most of its
    /// life, so without one this would spin instead of reporting that the address
    /// was never reached.
    pub fn run_until(&mut self, address: u32, budget: u64) -> Result<(), BudgetExceeded> {
        let wanted = Breakpoint::auto(address);
        for _ in 0..budget {
            if wanted.matches(self.chipset.cpu.regs.physical_pc()) {
                return Ok(());
            }
            self.step();
        }
        Err(BudgetExceeded {
            address,
            budget,
            at: self.chipset.cpu.regs.physical_pc(),
        })
    }

    /// Run until `address`, or the budget is spent; returns whether it arrived.
    ///
    /// Use this when missing the address is an expected outcome (polling a loop, say)
    /// rather than a failure.
    pub fn run_until_or_budget(&mut self, address: u32, budget: u64) -> bool {
        let wanted = Breakpoint::auto(address);
        for _ in 0..budget {
            if wanted.matches(self.chipset.cpu.regs.physical_pc()) {
                return true;
            }
            self.step();
        }
        wanted.matches(self.chipset.cpu.regs.physical_pc())
    }

    // ----------------------------------------------------------- breakpoints
    /// Arm a breakpoint, returning its index.
    pub fn breakpoint(&mut self, breakpoint: Breakpoint) -> usize {
        self.breakpoints.push(breakpoint);
        self.breakpoints.len() - 1
    }

    /// Every armed breakpoint.
    pub fn breakpoints(&self) -> &[Breakpoint] {
        &self.breakpoints
    }

    /// One breakpoint by index.
    pub fn breakpoint_at(&self, index: usize) -> &Breakpoint {
        &self.breakpoints[index]
    }

    /// How many times the breakpoint at `index` matched.
    pub fn hits(&self, index: usize) -> u64 {
        self.breakpoints[index].hits
    }

    /// The recorded hits of the breakpoint at `index`.
    pub fn recorded_hits(&self, index: usize) -> &[Hit] {
        &self.breakpoints[index].recorded
    }

    /// Disarm everything.
    pub fn clear_breakpoints(&mut self) {
        self.breakpoints.clear();
    }
}
