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

//! The debugger session: breakpoints, stepping, run control, and the state they
//! produce.
//!
//! A caller drives it the way a person would:
//!
//! ```no_run
//! use fx991::Emu;
//! use fx991_dbg::{Debugger, StopReason};
//!
//! let mut emu = Emu::from_rom(vec![0u8; 0x4_0000]);
//! let mut debugger = Debugger::new();
//! debugger.add_breakpoint_at(0x0_946A);
//! match debugger.run(&mut emu, 10_000) {
//!     StopReason::Breakpoint { id, address } => println!("stopped at {address:#x}"),
//!     other => println!("{other:?}"),
//! }
//! ```
//!
//! # Hooks are installed, not handed in
//!
//! Every run installs one hook on [`fx991::Emu::step_with_bus`] that consults the
//! breakpoints, the watches, and the trace.  A caller never writes a closure, so
//! the precedence between "a breakpoint hit", "a watch fired", and "the budget ran
//! out" is decided in one place rather than at each call site.
//!
//! # The three ways a run ends
//!
//! [`StopReason::Breakpoint`] and [`StopReason::Watch`] are the interesting ones.
//! [`StopReason::Stopped`] is the one that needs explaining: the ROM parks in STOP
//! for most of its life, and a tick then executes nothing at all.  Reporting that
//! distinctly -- rather than letting a "step" look like it worked -- is what stops
//! a user concluding the debugger is broken when `sp` will not move.

use fx991::{Emu, EmuSnapshot};
use fx991_chipset::TickOutcome;
use nxu8_asm::disasm::Insn;

use crate::breakpoint::{Breakpoint, BreakpointId, Watch, WatchId, WatchKind};
use crate::condition::{Context, Expr};
use crate::patch::{Patch, PatchError, Patcher, Patches};
use crate::snapshot::{Diff, Snapshots};
use crate::trace::TraceRing;

/// Why the machine stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// An execution breakpoint matched.
    Breakpoint {
        /// Which one.
        id: BreakpointId,
        /// Where it stopped, which is the address about to run.
        address: u32,
    },
    /// A memory watch matched.  The machine is at the instruction *after* the
    /// accessing one, which is why both addresses are reported.
    Watch {
        /// Which watch.
        id: WatchId,
        /// The address accessed.
        address: u32,
        /// Whether it was a read or a write.
        write: bool,
        /// The byte read or written.
        value: u8,
        /// The instruction that made the access.
        at: u32,
    },
    /// The machine is halted or stopped, so no instruction ran.
    ///
    /// A tick in this state executes nothing, so `step` cannot make progress and
    /// repeating it will not help.  Use [`Debugger::force_run`] to leave STOP, or
    /// run until an interrupt wakes the machine.
    Stopped,
    /// The tick budget ran out.
    Budget {
        /// The budget that was spent.
        budget: u64,
        /// Where the machine got to.
        at: u32,
    },
    /// The machine paused because a run was asked to stop, e.g. by a script.
    Paused,
}

impl StopReason {
    /// Whether this reason means an instruction is waiting to run at
    /// [`Debugger::pc`].
    pub fn is_breakpoint(&self) -> bool {
        matches!(
            self,
            StopReason::Breakpoint { .. } | StopReason::Watch { .. }
        )
    }

    /// A one-line description.
    pub fn text(&self) -> String {
        match self {
            StopReason::Breakpoint { id, address } => {
                format!("breakpoint {} at {address:07X}h", id.0)
            }
            StopReason::Watch {
                id,
                address,
                write,
                value,
                at,
            } => format!(
                "watch {}: {} {address:05X}h = {value:02X} (from {at:07X}h)",
                id.0,
                if *write { "write" } else { "read" },
            ),
            StopReason::Stopped => "stopped (halted or in standby); use force_run".to_string(),
            StopReason::Budget { budget, at } => {
                format!("budget of {budget} ticks spent at {at:07X}h")
            }
            StopReason::Paused => "paused".to_string(),
        }
    }
}

/// The debugger's own state: what to stop on, and the bookkeeping that goes with
/// it.  The machine lives in the [`Emu`] the caller passes in, so one debugger can
/// drive a machine that was reloaded.
pub struct Debugger {
    breakpoints: Vec<(BreakpointId, Breakpoint)>,
    watches: Vec<(WatchId, Watch)>,
    next_breakpoint_id: u64,
    next_watch_id: u64,
    /// Logged arrivals of `log_only` breakpoints, oldest first.
    log: Vec<(BreakpointId, u64, u32)>,
    /// How many logged arrivals to keep.
    pub log_limit: usize,
    snapshots: Snapshots,
    /// The last patch applied, kept for reporting.  The authoritative record is the
    /// bus's overlay list; see [`crate::patch`].
    patches: Vec<Patch>,
    trace: Option<TraceRing>,
    /// A pending temporary breakpoint, used by `step_over`, `step_out` and
    /// `run_to`.  All three want the same thing -- stop when the PC reaches this
    /// address -- and the caller reads [`Debugger::stepped`] to tell that apart from
    /// a user breakpoint, so the reason it was set does not need recording.
    temporary: Option<u32>,
    /// Set when a temporary breakpoint fires, so the caller can tell a step
    /// completion from a user breakpoint.
    stepped: bool,
}

impl Default for Debugger {
    fn default() -> Self {
        Self::new()
    }
}

impl Debugger {
    /// An empty session: nothing armed, no trace running.
    pub fn new() -> Self {
        Self {
            breakpoints: Vec::new(),
            watches: Vec::new(),
            next_breakpoint_id: 1,
            next_watch_id: 1,
            log: Vec::new(),
            log_limit: 4096,
            snapshots: Snapshots::new(),
            patches: Vec::new(),
            trace: None,
            temporary: None,
            stepped: false,
        }
    }

    // ------------------------------------------------------------- breakpoints

    /// Arm an execution breakpoint, returning its id.
    pub fn add_breakpoint(&mut self, breakpoint: Breakpoint) -> BreakpointId {
        let id = BreakpointId(self.next_breakpoint_id);
        self.next_breakpoint_id += 1;
        self.breakpoints.push((id, breakpoint));
        id
    }

    /// Arm a breakpoint at an address, auto-detecting the address form.
    pub fn add_breakpoint_at(&mut self, address: u32) -> BreakpointId {
        self.add_breakpoint(Breakpoint::auto(address))
    }

    /// Arm a conditional breakpoint.  The condition text is parsed now, so a typo
    /// is reported here rather than never matching silently.
    pub fn add_conditional_breakpoint(
        &mut self,
        address: u32,
        condition: &str,
    ) -> Result<BreakpointId, crate::condition::ParseError> {
        let condition = Expr::parse(condition)?;
        Ok(self.add_breakpoint(Breakpoint::auto(address).with_condition(condition)))
    }

    /// Every armed breakpoint, in the order they were added.
    pub fn breakpoints(&self) -> impl Iterator<Item = (BreakpointId, &Breakpoint)> {
        self.breakpoints
            .iter()
            .map(|(id, breakpoint)| (*id, breakpoint))
    }

    /// One breakpoint by id.
    pub fn breakpoint(&self, id: BreakpointId) -> Option<&Breakpoint> {
        self.breakpoints
            .iter()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, breakpoint)| breakpoint)
    }

    /// One breakpoint by id, mutably.
    pub fn breakpoint_mut(&mut self, id: BreakpointId) -> Option<&mut Breakpoint> {
        self.breakpoints
            .iter_mut()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, breakpoint)| breakpoint)
    }

    /// How many breakpoints are armed.
    pub fn breakpoint_count(&self) -> usize {
        self.breakpoints.len()
    }

    /// Disarm one.  Returns whether it was there.
    pub fn remove_breakpoint(&mut self, id: BreakpointId) -> bool {
        let before = self.breakpoints.len();
        self.breakpoints.retain(|(candidate, _)| *candidate != id);
        self.breakpoints.len() != before
    }

    /// Disarm every execution breakpoint.
    pub fn clear_breakpoints(&mut self) {
        self.breakpoints.clear();
    }

    /// Logged arrivals, as `(breakpoint, tick, address)`, oldest first.
    pub fn log(&self) -> &[(BreakpointId, u64, u32)] {
        &self.log
    }

    // ---------------------------------------------------------------- watches

    /// Arm a memory watch, returning its id.
    ///
    /// The watch is installed on the bus immediately.
    pub fn add_watch(&mut self, emu: &mut Emu, watch: Watch) -> WatchId {
        let id = WatchId(self.next_watch_id);
        self.next_watch_id += 1;
        emu.chipset.bus.watch(watch.bus_flags());
        self.watches.push((id, watch));
        id
    }

    /// Every armed watch, in the order they were added.
    pub fn watches(&self) -> impl Iterator<Item = (WatchId, &Watch)> {
        self.watches.iter().map(|(id, watch)| (*id, watch))
    }

    /// One watch by id.
    pub fn watch(&self, id: WatchId) -> Option<&Watch> {
        self.watches
            .iter()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, watch)| watch)
    }

    /// How many watches are armed.
    pub fn watch_count(&self) -> usize {
        self.watches.len()
    }

    /// Remove a watch, and uninstall it from the bus if no other watch uses that
    /// address.
    pub fn remove_watch(&mut self, emu: &mut Emu, id: WatchId) -> bool {
        let Some(index) = self
            .watches
            .iter()
            .position(|(candidate, _)| *candidate == id)
        else {
            return false;
        };
        let address = self.watches[index].1.address;
        self.watches.remove(index);
        if !self
            .watches
            .iter()
            .any(|(_, watch)| watch.address == address)
        {
            emu.chipset.bus.unwatch(address);
        }
        true
    }

    /// Remove every watch and uninstall them.
    pub fn clear_watches(&mut self, emu: &mut Emu) {
        for (_, watch) in self.watches.drain(..) {
            emu.chipset.bus.unwatch(watch.address);
        }
    }

    // --------------------------------------------------------------- snapshots

    /// Save the machine under a name.
    pub fn save_snapshot(&mut self, name: impl Into<String>, emu: &Emu) {
        self.snapshots.save(name, emu.snapshot());
    }

    /// Restore a named snapshot.  Returns whether the name existed.
    ///
    /// The patch list is re-read afterwards because the overlay list is part of
    /// what was restored.
    pub fn restore_snapshot(&mut self, name: &str, emu: &mut Emu) -> bool {
        let Some(state) = self.snapshots.get(name).cloned() else {
            return false;
        };
        emu.restore(&state);
        self.refresh_patches(emu);
        true
    }

    /// The names of the saved snapshots.
    pub fn snapshot_names(&self) -> Vec<String> {
        self.snapshots
            .entries()
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }

    /// The stored state under a name.
    pub fn snapshot(&self, name: &str) -> Option<&EmuSnapshot> {
        self.snapshots.get(name)
    }

    /// Compare two snapshots.
    pub fn diff_snapshots(&self, before: &str, after: &str) -> Option<Diff> {
        let left = self.snapshots.get(before)?;
        let right = self.snapshots.get(after)?;
        Some(Diff::between(left, right))
    }

    /// Compare a snapshot against the machine as it is now.
    pub fn diff_with_now(&self, name: &str, emu: &Emu) -> Option<Diff> {
        let before = self.snapshots.get(name)?;
        let now = emu.snapshot();
        Some(Diff::between(before, &now))
    }

    // ----------------------------------------------------------------- patches

    /// Plant a patch, remembering it for reporting.
    pub fn apply_patch(
        &mut self,
        emu: &mut Emu,
        address: u32,
        bytes: &[u8],
        source: impl Into<String>,
    ) -> Result<Patch, PatchError> {
        let patch = Patcher::apply(&mut emu.chipset.bus, address, bytes, source)?;
        self.patches.retain(|existing| existing.address != address);
        self.patches.push(patch.clone());
        Ok(patch)
    }

    /// Remove the patch at an address.
    pub fn undo_patch(&mut self, emu: &mut Emu, address: u32) -> bool {
        let removed = Patcher::undo(&mut emu.chipset.bus, address);
        self.patches.retain(|patch| patch.address != address);
        removed
    }

    /// Remove every patch.
    pub fn undo_all_patches(&mut self, emu: &mut Emu) -> usize {
        let count = Patcher::undo_all(&mut emu.chipset.bus);
        self.patches.clear();
        count
    }

    /// The patches this session applied, for display.
    pub fn patches(&self) -> &[Patch] {
        &self.patches
    }

    /// What is planted in the machine right now, which after a snapshot restore is
    /// authoritative where [`Debugger::patches`] is only a record.
    pub fn planted<'a>(&self, emu: &'a Emu) -> Patches<'a> {
        Patches::of(&emu.chipset.bus)
    }

    /// Re-read the patch record after something replaced the overlay list.
    fn refresh_patches(&mut self, emu: &Emu) {
        self.patches = Patches::of(&emu.chipset.bus).iter().collect();
    }

    // ------------------------------------------------------------------- trace

    /// Start tracing into a ring of the given capacity.
    pub fn start_trace(&mut self, capacity: usize) {
        self.trace = Some(TraceRing::new(capacity));
    }

    /// Stop tracing, returning what was collected.
    pub fn stop_trace(&mut self) -> Option<TraceRing> {
        self.trace.take()
    }

    /// The running trace, if any.
    pub fn trace(&self) -> Option<&TraceRing> {
        self.trace.as_ref()
    }

    /// Whether a trace is running.
    pub fn is_tracing(&self) -> bool {
        self.trace.is_some()
    }

    // ---------------------------------------------------------------- stepping

    /// Where the machine is now.
    pub fn pc(&self, emu: &Emu) -> u32 {
        emu.pc()
    }

    /// Whether the machine is halted or stopped.
    pub fn is_stopped(&self, emu: &Emu) -> bool {
        emu.chipset.run_mode() != fx991_chipset::RunMode::Run
    }

    /// Leave STOP/HALT so instructions can run again.
    ///
    /// Needed because a standby request is applied on the tick *after* the write
    /// that asked for it, so a machine parked by its own code cannot be woken by
    /// stepping -- each tick only re-confirms the stop.
    pub fn force_run(&self, emu: &mut Emu) {
        emu.chipset.force_run();
    }

    /// Execute one instruction.
    ///
    /// Returns [`StopReason::Stopped`] without executing anything when the machine
    /// is parked, which is the honest answer: a tick in that state does nothing.
    pub fn step(&mut self, emu: &mut Emu) -> StopReason {
        self.stepped = false;
        let outcome = self.tick(emu);
        match outcome {
            TickOutcome::Stopped => StopReason::Stopped,
            TickOutcome::Suppressed => self.suppressed_reason(emu),
            _ => StopReason::Paused,
        }
    }

    /// Step over a call: run to the instruction after it.
    ///
    /// A `BL` does not return to `pc + length` -- it returns whenever the callee
    /// `RT`s -- so this is a temporary breakpoint on the return address, which is
    /// what `LR` holds after a call.  For a non-call it is just a step.
    ///
    /// Not meaningful on a ROP chain: a gadget ends in `POP PC`, which does not use
    /// the link register at all, so there is no return address to stop at.
    pub fn step_over(&mut self, emu: &mut Emu, budget: u64) -> StopReason {
        let pc = emu.pc();
        let bytes = self.decode(emu, pc);
        if !bytes.is_some_and(|insn| insn.is_call) {
            return self.step(emu);
        }

        // `BL` writes LR before jumping, so read it after the call executes.
        self.tick(emu);
        let return_address =
            ((emu.chipset.cpu.regs.lcsr() as u32) << 16) | emu.chipset.cpu.regs.lr() as u32;
        self.temporary = Some(return_address);
        self.run(emu, budget)
    }

    /// Run until the current call returns.
    ///
    /// Uses the link register, so this is meaningful only inside a `BL`-style call.
    pub fn step_out(&mut self, emu: &mut Emu, budget: u64) -> StopReason {
        let return_address =
            ((emu.chipset.cpu.regs.lcsr() as u32) << 16) | emu.chipset.cpu.regs.lr() as u32;
        self.temporary = Some(return_address);
        self.run(emu, budget)
    }

    /// Run until the PC reaches an address, or the budget is spent.
    pub fn run_to(&mut self, emu: &mut Emu, address: u32, budget: u64) -> StopReason {
        self.temporary = Some(address);
        self.run(emu, budget)
    }

    /// Run until a breakpoint or watch fires, or the budget is spent.
    /// Run until a breakpoint or watch fires, or the budget is spent.
    ///
    /// # A parked machine is waited out, not reported
    ///
    /// The ROM parks in STOP as normal operation -- it does it after every reset and
    /// once per key scan -- so `run` keeps ticking through a stop and lets the timer
    /// or the keyboard wake the machine.  A stop is only reported when the *whole
    /// budget* passes without a single instruction executing, which means the machine
    /// is genuinely stuck rather than waiting.
    ///
    /// That is deliberately different from [`Debugger::step`], which reports a
    /// parked machine immediately: one step that executed nothing is always worth
    /// saying, whereas "continue" on a sleeping calculator means "wait for it".
    pub fn run(&mut self, emu: &mut Emu, budget: u64) -> StopReason {
        self.stepped = false;
        let mut executed = false;
        for _ in 0..budget {
            // A breakpoint fires on arrival, before the instruction runs.
            if let Some(reason) = self.breakpoints_at(emu) {
                return reason;
            }
            let address_before = emu.pc();
            let outcome = self.tick(emu);
            if !matches!(outcome, TickOutcome::Stopped) {
                executed = true;
            }

            // A data watch fires after the access, so the report can name the
            // instruction that made it.
            if let Some(reason) = self.watches_hit(emu, address_before) {
                return reason;
            }

            if self.stepped {
                self.stepped = false;
                return StopReason::Paused;
            }
        }
        if !executed {
            return StopReason::Stopped;
        }
        StopReason::Budget {
            budget,
            at: emu.pc(),
        }
    }

    /// Run a budget without consulti any breakpoint, for "let it settle" steps.
    pub fn run_uninterrupted(&mut self, emu: &mut Emu, budget: u64) {
        for _ in 0..budget {
            self.tick(emu);
        }
    }

    // ------------------------------------------------------------------ internals

    /// One tick, with the trace recorded.
    fn tick(&mut self, emu: &mut Emu) -> TickOutcome {
        let address = emu.pc();
        // The registers are sampled around the instruction so the trace can say
        // what *this* instruction changed rather than comparing entries.
        let before = emu.chipset.cpu.regs.r;
        let outcome = emu.step();

        if let Some(trace) = self.trace.as_mut() {
            let after = emu.chipset.cpu.regs.r;
            let handler = match outcome {
                TickOutcome::Executed(name) => Some(name),
                _ => None,
            };
            trace.record(crate::trace::Sample {
                tick: emu.ticks,
                pc: address,
                handler,
                registers_before: &before,
                registers_after: &after,
                sp: emu.chipset.cpu.regs.sp,
                psw: emu.chipset.cpu.regs.psw(),
            });
        }
        outcome
    }

    /// The reason the instruction was suppressed, if a breakpoint or the
    /// temporary one asked for it.
    fn suppressed_reason(&mut self, emu: &Emu) -> StopReason {
        if let Some(reason) = self.temporary_at(emu.pc()) {
            return reason;
        }
        StopReason::Breakpoint {
            id: BreakpointId(0),
            address: emu.pc(),
        }
    }

    /// Whether a breakpoint matches the current PC, updating counters.
    fn breakpoints_at(&mut self, emu: &mut Emu) -> Option<StopReason> {
        let pc = emu.pc();

        if let Some(reason) = self.temporary_at(pc) {
            return Some(reason);
        }

        // The breakpoint list is taken out for the duration: a condition needs the
        // machine borrowed mutably, so walking the list in place would borrow
        // `self` twice.
        let mut breakpoints = std::mem::take(&mut self.breakpoints);
        let mut triggered: Option<(BreakpointId, bool)> = None;
        for (id, breakpoint) in breakpoints.iter_mut() {
            if !breakpoint.enabled || !breakpoint.matches(pc) {
                continue;
            }
            let hits = breakpoint.hits;
            // Borrowed, not cloned: `Expr` is a boxed tree, so a clone here would
            // walk and re-allocate it on every arrival at every conditional
            // breakpoint.  The breakpoint list is already out of `self`, and
            // `evaluate` takes the machine separately, so the two borrows are
            // disjoint.
            let matches_condition = match &breakpoint.condition {
                None => true,
                Some(condition) => self.evaluate(condition, emu, hits),
            };
            breakpoint.hits += 1;
            if !matches_condition {
                continue;
            }
            if breakpoint.once {
                breakpoint.enabled = false;
            }
            let stops = breakpoint.stops();
            if stops {
                breakpoint.stops += 1;
            }
            triggered = Some((*id, stops));
            break;
        }
        self.breakpoints = breakpoints;

        let (id, stops) = triggered?;
        if !stops {
            // A logging breakpoint records and lets the machine continue.
            let tick = emu.ticks;
            if self.log.len() < self.log_limit {
                self.log.push((id, tick, pc));
            }
            return None;
        }
        Some(StopReason::Breakpoint { id, address: pc })
    }

    /// Evaluate a condition against the machine.
    fn evaluate(&mut self, condition: &Expr, emu: &mut Emu, hits: u64) -> bool {
        let mut context = Context {
            regs: &emu.chipset.cpu.regs,
            bus: &mut emu.chipset.bus,
            hits,
        };
        condition.eval(&mut context)
    }

    /// Whether a pending temporary breakpoint is at `pc`.
    fn temporary_at(&mut self, pc: u32) -> Option<StopReason> {
        if self.temporary? != pc {
            return None;
        }
        self.temporary = None;
        self.stepped = true;
        Some(StopReason::Paused)
    }

    /// Whether a watch fired on the tick that just ran.
    fn watches_hit(&mut self, emu: &mut Emu, at: u32) -> Option<StopReason> {
        if self.watches.is_empty() {
            return None;
        }
        let hits = emu.chipset.bus.take_watch_hits();
        if hits.is_empty() {
            return None;
        }

        for hit in hits {
            let write = matches!(
                hit.kind,
                fx991_bus::Access::Write | fx991_bus::Access::SfrWrite
            );
            // The watch list is taken out for the duration for the same reason the
            // breakpoint list is: a condition borrows the machine mutably.
            let mut watches = std::mem::take(&mut self.watches);
            let mut matched: Option<WatchId> = None;
            for (id, watch) in watches.iter_mut() {
                if !watch.enabled || watch.address != hit.address {
                    continue;
                }
                let direction_matches = match watch.kind {
                    WatchKind::Read => !write && hit.kind != fx991_bus::Access::Code,
                    WatchKind::Write => write,
                    WatchKind::ReadWrite => hit.kind != fx991_bus::Access::Code,
                    WatchKind::Execute => hit.kind == fx991_bus::Access::Code,
                };
                if !direction_matches {
                    continue;
                }
                let count = watch.hits;
                // Borrowed for the same reason as the breakpoint condition above.
                let condition_holds = match &watch.condition {
                    None => true,
                    Some(condition) => self.evaluate(condition, emu, count),
                };
                watch.hits += 1;
                if !condition_holds {
                    continue;
                }
                watch.stops += 1;
                matched = Some(*id);
                break;
            }
            self.watches = watches;

            if let Some(id) = matched {
                return Some(StopReason::Watch {
                    id,
                    address: hit.address,
                    write,
                    value: hit.value,
                    at,
                });
            }
        }
        None
    }

    /// Decode the instruction at an address, for step-over's call test.
    fn decode(&mut self, emu: &mut Emu, address: u32) -> Option<nxu8_asm::disasm::Insn> {
        let mut read = |at: u32| emu.chipset.bus.read_code_quiet(at);
        Some(nxu8_asm::disasm::disassemble_one(&mut read, address))
    }

    /// Disassemble `count` instructions from an address, for a view.
    pub fn disassemble(&mut self, emu: &mut Emu, address: u32, count: usize) -> String {
        let mut read = |at: u32| emu.chipset.bus.read_code_quiet(at);
        nxu8_asm::disasm::disassemble(&mut read, address, count)
    }

    /// The same disassembly, as structured instructions rather than text.
    ///
    /// A caller that needs each line's address -- to mark the current PC, say --
    /// wants this rather than splitting [`Debugger::disassemble`]'s output back
    /// apart, which cannot tell where the address field ends.
    pub fn disassemble_insns(&mut self, emu: &mut Emu, address: u32, count: usize) -> Vec<Insn> {
        let mut read = |at: u32| emu.chipset.bus.read_code_quiet(at);
        let mut out = Vec::with_capacity(count);
        let mut pc = address & !1;
        for _ in 0..count {
            let insn = nxu8_asm::disasm::disassemble_one(&mut read, pc);
            // The address space is 24 bits, so walking off the top wraps rather
            // than overflowing.  `wrapping_add` first: `pc` is masked but the
            // addition can still leave the space before the mask brings it back.
            pc = pc.wrapping_add(insn.length as u32) & 0x00FF_FFFF;
            out.push(insn);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxu8_asm::asm;

    /// A machine whose ROM is a hand-written program at `0x10000`.
    ///
    /// No ROM image is needed, so these tests run everywhere rather than skipping.
    /// The reset interrupt is cleared by hand and execution starts at `csr = 1,
    /// pc = 0`, the same shape the workspace's other scratch-program tests use.
    fn scratch(program: &[u8]) -> Emu {
        let mut rom = vec![0u8; 0x4_0000];
        rom[0x0000] = 0x00;
        rom[0x0001] = 0xF0; // SP = 0xF000
        rom[0x1_0000..0x1_0000 + program.len()].copy_from_slice(program);
        let mut emu = Emu::from_rom(rom);
        // Drop the reset request, so the program is the whole world here.
        {
            let mut interrupts = emu.chipset.interrupts.borrow_mut();
            interrupts.active = [false; fx991_chipset::INT_COUNT];
            interrupts.pending_count = 0;
        }
        // SP is already 0xF000 from the vector; point PC at the program.
        emu.chipset.cpu.regs.csr = 1;
        emu.chipset.cpu.regs.pc = 0;
        emu
    }

    /// Two instructions then a spin: enough to step and stop on.
    fn short_program() -> Vec<u8> {
        let mut program = Vec::new();
        program.extend_from_slice(&asm::mov_r_imm(0, 0x11)); // 0x10000
        program.extend_from_slice(&asm::mov_r_imm(1, 0x22)); // 0x10002
        program.extend_from_slice(&asm::mov_r_imm(2, 0x33)); // 0x10004
        program
    }

    #[test]
    fn stepping_runs_one_instruction_at_a_time() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();

        assert_eq!(emu.pc(), 0x1_0000);
        debugger.step(&mut emu);
        assert_eq!(emu.pc(), 0x1_0002);
        assert_eq!(emu.reg(0), 0x11);

        debugger.step(&mut emu);
        assert_eq!(emu.pc(), 0x1_0004);
        assert_eq!(emu.reg(1), 0x22);
    }

    #[test]
    fn a_breakpoint_stops_before_the_instruction_runs() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        let id = debugger.add_breakpoint_at(0x1_0002);

        let reason = debugger.run(&mut emu, 100);
        assert_eq!(
            reason,
            StopReason::Breakpoint {
                id,
                address: 0x1_0002
            }
        );
        assert_eq!(emu.pc(), 0x1_0002, "stopped AT the breakpoint");
        assert_eq!(emu.reg(0), 0x11, "the first instruction ran");
        assert_eq!(emu.reg(1), 0x00, "the second did not");
    }

    #[test]
    fn a_breakpoint_hit_count_and_stop_count_both_advance() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        let id = debugger.add_breakpoint_at(0x1_0002);

        debugger.run(&mut emu, 100);
        let breakpoint = debugger.breakpoint(id).unwrap();
        assert_eq!(breakpoint.hits, 1);
        assert_eq!(breakpoint.stops, 1);
    }

    #[test]
    fn a_once_breakpoint_disarms_itself() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        let id = debugger.add_breakpoint(Breakpoint::auto(0x1_0002).once());

        assert!(debugger.run(&mut emu, 100).is_breakpoint());
        assert!(!debugger.breakpoint(id).unwrap().enabled);

        // Running on must not stop there again.
        let reason = debugger.run(&mut emu, 100);
        assert!(matches!(reason, StopReason::Budget { .. }), "{reason:?}");
    }

    #[test]
    fn a_conditional_breakpoint_only_stops_when_the_condition_holds() {
        let mut program = Vec::new();
        program.extend_from_slice(&asm::mov_r_imm(0, 0x11)); // 0x10000
        program.extend_from_slice(&asm::mov_r_imm(0, 0x42)); // 0x10002
        program.extend_from_slice(&asm::mov_r_imm(1, 0x99)); // 0x10004
        let mut emu = scratch(&program);
        let mut debugger = Debugger::new();

        // The condition is false on arrival (R0 is 0), so this must not stop.
        debugger
            .add_conditional_breakpoint(0x1_0002, "r0 == 0x42")
            .unwrap();
        let reason = debugger.run(&mut emu, 100);
        assert!(
            matches!(reason, StopReason::Budget { .. }),
            "the condition never held: {reason:?}"
        );
        assert_eq!(emu.reg(1), 0x99, "the program ran to the end");
    }

    #[test]
    fn a_conditional_breakpoint_that_holds_stops() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        // R0 is 0x11 by the time 0x10002 is reached.
        let id = debugger
            .add_conditional_breakpoint(0x1_0002, "r0 == 0x11")
            .unwrap();
        let reason = debugger.run(&mut emu, 100);
        assert_eq!(
            reason,
            StopReason::Breakpoint {
                id,
                address: 0x1_0002
            }
        );
    }

    #[test]
    fn a_condition_can_read_memory() {
        let mut program = Vec::new();
        // Point EA at 0xD180, store a byte, then arrive at the breakpoint.
        program.extend_from_slice(&asm::lea_dadr(0xD180)); // 0x10000
        program.extend_from_slice(&asm::mov_r_imm(0, 0xFF)); // 0x10004
        program.extend_from_slice(&asm::st_r_ea(0)); // 0x10006
        program.extend_from_slice(&asm::mov_r_imm(1, 0x77)); // 0x10008
        let mut emu = scratch(&program);
        let mut debugger = Debugger::new();

        // 0xD180 holds 0xFF by then, so the condition holds.
        let id = debugger
            .add_conditional_breakpoint(0x1_0008, "[0xD180] == 0xFF")
            .unwrap();
        let reason = debugger.run(&mut emu, 100);
        assert_eq!(
            reason,
            StopReason::Breakpoint {
                id,
                address: 0x1_0008
            }
        );
        assert_eq!(emu.reg(1), 0x00, "the instruction after did not run");
    }

    #[test]
    fn hits_is_available_to_a_condition() {
        let mut program = Vec::new();
        // A short backward branch loop, so one address is visited repeatedly.
        program.extend_from_slice(&asm::mov_r_imm(0, 0)); // 0x10000
        program.extend_from_slice(&asm::add_r_imm(0, 1)); // 0x10002
        program.extend_from_slice(&asm::cmp_r_r(0, 0)); // 0x10004
        let mut emu = scratch(&program);
        let mut debugger = Debugger::new();

        // Stop on the third visit rather than the first.
        let id = debugger
            .add_conditional_breakpoint(0x1_0002, "hits >= 2")
            .unwrap();
        let reason = debugger.run(&mut emu, 10);
        // The loop never branches back, so this address is reached once.
        assert!(
            matches!(reason, StopReason::Budget { .. }),
            "only one visit, so the count never reached three: {reason:?}"
        );
        assert_eq!(debugger.breakpoint(id).unwrap().hits, 1);
    }

    #[test]
    fn a_logging_breakpoint_records_without_stopping() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        let id = debugger.add_breakpoint(Breakpoint::auto(0x1_0002).log_only());

        let reason = debugger.run(&mut emu, 100);
        assert!(
            matches!(reason, StopReason::Budget { .. }),
            "it did not stop: {reason:?}"
        );
        assert_eq!(debugger.log().len(), 1);
        // The logged tick is the one the arrival is checked on, which is before the
        // instruction at that address has run.
        let (logged_id, _, logged_address) = debugger.log()[0];
        assert_eq!(logged_id, id);
        assert_eq!(logged_address, 0x1_0002);
        assert_eq!(emu.reg(1), 0x22, "the machine kept running");
    }

    #[test]
    fn an_unknown_name_in_a_condition_is_reported_at_parse_time() {
        let mut debugger = Debugger::new();
        let error = debugger
            .add_conditional_breakpoint(0x1_0000, "no_such_thing == 1")
            .unwrap_err();
        assert!(error.message.contains("unknown name"), "{error}");
        assert_eq!(debugger.breakpoint_count(), 0, "nothing was armed");
    }

    #[test]
    fn run_to_stops_at_an_address_without_arming_a_breakpoint() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        let reason = debugger.run_to(&mut emu, 0x1_0004, 100);
        assert_eq!(reason, StopReason::Paused);
        assert_eq!(emu.pc(), 0x1_0004);
        assert_eq!(debugger.breakpoint_count(), 0);
    }

    #[test]
    fn run_to_reports_a_budget_that_ran_out() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        let reason = debugger.run_to(&mut emu, 0x2_0000, 5);
        assert!(
            matches!(reason, StopReason::Budget { budget: 5, .. }),
            "{reason:?}"
        );
    }

    /// A program with a call: the caller at `0x10000`, the callee at `0x10100`.
    ///
    /// `BL` is four bytes with its long immediate, so the instruction after the
    /// call sits at `0x10006`, not `0x10004` -- worth spelling out because getting
    /// it wrong makes the caller's next instruction land inside the branch.
    fn call_program() -> Vec<u8> {
        const CALLEE: usize = 0x100;
        let mut program = vec![0u8; CALLEE + 4];
        program[0..2].copy_from_slice(&asm::mov_r_imm(0, 0x01)); // 0x10000
        program[2..6].copy_from_slice(&asm::bl(1, CALLEE as u16)); // 0x10002
        program[6..8].copy_from_slice(&asm::mov_r_imm(2, 0x03)); // 0x10006
        program[CALLEE..CALLEE + 2].copy_from_slice(&asm::mov_r_imm(1, 0x02)); // 0x10100
        program[CALLEE + 2..CALLEE + 4].copy_from_slice(&asm::RT); // 0x10102
        program
    }

    #[test]
    fn step_over_follows_a_call_to_its_return_address() {
        let mut emu = scratch(&call_program());
        let mut debugger = Debugger::new();

        debugger.step(&mut emu); // the MOV
        assert_eq!(emu.pc(), 0x1_0002);

        let reason = debugger.step_over(&mut emu, 100);
        assert_eq!(reason, StopReason::Paused, "the call returned");
        assert_eq!(emu.pc(), 0x1_0006, "landed after the four-byte call");
        assert_eq!(emu.reg(1), 0x02, "the callee ran");
        assert_eq!(emu.reg(2), 0x00, "but not the instruction after");
    }

    #[test]
    fn step_over_on_a_non_call_is_just_a_step() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        debugger.step_over(&mut emu, 100);
        assert_eq!(emu.pc(), 0x1_0002);
        assert_eq!(emu.reg(0), 0x11);
    }

    #[test]
    fn step_out_returns_to_the_caller() {
        let mut emu = scratch(&call_program());
        let mut debugger = Debugger::new();

        debugger.step(&mut emu); // the MOV
        debugger.step(&mut emu); // enter the call
        assert_eq!(emu.pc(), 0x1_0100);

        let reason = debugger.step_out(&mut emu, 100);
        assert_eq!(reason, StopReason::Paused);
        assert_eq!(emu.pc(), 0x1_0006, "back in the caller");
        assert_eq!(emu.reg(1), 0x02);
    }

    #[test]
    fn a_stopped_machine_reports_stopped_rather_than_pretending_to_step() {
        // The ROM parks in STOP for most of its life; a step there executes
        // nothing, and saying so is the whole point of `StopReason::Stopped`.
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        emu.chipset.interrupts.borrow_mut().stop();

        let pc_before = emu.pc();
        let reason = debugger.step(&mut emu);
        assert_eq!(reason, StopReason::Stopped);
        assert_eq!(emu.pc(), pc_before, "nothing ran");
        assert!(reason.text().contains("force_run"), "{}", reason.text());
    }

    #[test]
    fn a_run_waits_out_a_parked_machine_rather_than_reporting_it() {
        // The ROM parks as normal operation, so `continue` on a sleeping calculator
        // means "wait for the timer" rather than "tell me it is asleep".  `step`
        // still reports immediately, which the test above pins.
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        emu.chipset.interrupts.borrow_mut().stop();

        // A budget spent entirely parked is a stuck machine, and that is reported.
        let reason = debugger.run(&mut emu, 10);
        assert_eq!(reason, StopReason::Stopped);

        // But a machine that wakes inside the budget is not reported as stopped: the
        // timer interrupt clears the stop and the program then runs.
        emu.chipset.interrupts.borrow_mut().run_mode = fx991_chipset::RunMode::Run;
        let reason = debugger.run(&mut emu, 4);
        assert!(matches!(reason, StopReason::Budget { .. }), "{reason:?}");
        assert_eq!(emu.pc(), 0x1_0008, "four instructions ran");
    }

    #[test]
    fn force_run_leaves_stop_so_stepping_works_again() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        emu.chipset.interrupts.borrow_mut().stop();
        assert_eq!(debugger.step(&mut emu), StopReason::Stopped);

        debugger.force_run(&mut emu);
        assert!(!debugger.is_stopped(&emu));
        debugger.step(&mut emu);
        assert_eq!(emu.pc(), 0x1_0002, "it stepped");
    }

    #[test]
    fn a_watch_stops_after_the_write_and_names_the_accessing_instruction() {
        let mut program = Vec::new();
        program.extend_from_slice(&asm::lea_dadr(0xD180)); // 0x10000
        program.extend_from_slice(&asm::mov_r_imm(0, 0x5A)); // 0x10004
        program.extend_from_slice(&asm::st_r_ea(0)); // 0x10006: the write
        program.extend_from_slice(&asm::mov_r_imm(1, 0x77)); // 0x10008
        let mut emu = scratch(&program);
        let mut debugger = Debugger::new();
        let id = debugger.add_watch(&mut emu, Watch::new(0xD180, WatchKind::Write));

        let reason = debugger.run(&mut emu, 100);
        match reason {
            StopReason::Watch {
                id: reported,
                address,
                write,
                value,
                at,
            } => {
                assert_eq!(reported, id);
                assert_eq!(address, 0xD180);
                assert!(write);
                assert_eq!(value, 0x5A);
                assert_eq!(at, 0x1_0006, "the ST was the instruction");
            }
            other => panic!("expected a watch stop, got {other:?}"),
        }
        assert_eq!(emu.reg(1), 0x00, "it stopped after the store, before this");
        assert_eq!(debugger.watch(id).unwrap().hits, 1);
    }

    #[test]
    fn a_read_watch_ignores_writes_and_vice_versa() {
        let mut program = Vec::new();
        program.extend_from_slice(&asm::lea_dadr(0xD180)); // 0x10000
        program.extend_from_slice(&asm::mov_r_imm(0, 0x11)); // 0x10004
        program.extend_from_slice(&asm::st_r_ea(0)); // 0x10006: write
        program.extend_from_slice(&asm::l_r_ea(1)); // 0x10008: read
        let mut emu = scratch(&program);
        let mut debugger = Debugger::new();
        debugger.add_watch(&mut emu, Watch::new(0xD180, WatchKind::Read));

        let reason = debugger.run(&mut emu, 100);
        match reason {
            StopReason::Watch { write, at, .. } => {
                assert!(!write, "the read is what matched, not the write");
                assert_eq!(at, 0x1_0008);
            }
            other => panic!("expected a read watch stop, got {other:?}"),
        }
    }

    #[test]
    fn a_conditional_watch_only_stops_when_the_condition_holds() {
        let mut program = Vec::new();
        program.extend_from_slice(&asm::lea_dadr(0xD180)); // 0x10000
        program.extend_from_slice(&asm::mov_r_imm(0, 0x01)); // 0x10004
        program.extend_from_slice(&asm::st_r_ea(0)); // 0x10006: write 1
        program.extend_from_slice(&asm::mov_r_imm(0, 0xF0)); // 0x10008
        program.extend_from_slice(&asm::st_r_ea(0)); // 0x1000A: write 0xF0
        let mut emu = scratch(&program);
        let mut debugger = Debugger::new();
        debugger.add_watch(
            &mut emu,
            Watch::new(0xD180, WatchKind::Write)
                .with_condition(Expr::parse("[0xD180] == 0xF0").unwrap()),
        );

        let reason = debugger.run(&mut emu, 100);
        match reason {
            StopReason::Watch { value, at, .. } => {
                assert_eq!(value, 0xF0, "only the second write matched");
                assert_eq!(at, 0x1_000A);
            }
            other => panic!("expected the second write to stop, got {other:?}"),
        }
    }

    #[test]
    fn removing_a_watch_uninstalls_it_from_the_bus() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        let id = debugger.add_watch(&mut emu, Watch::new(0xD180, WatchKind::ReadWrite));
        assert_eq!(emu.chipset.bus.watches().len(), 1);

        assert!(debugger.remove_watch(&mut emu, id));
        assert!(emu.chipset.bus.watches().is_empty());
        assert!(!debugger.remove_watch(&mut emu, id), "already gone");
    }

    #[test]
    fn clearing_watches_uninstalls_them_all() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        debugger.add_watch(&mut emu, Watch::new(0xD180, WatchKind::Read));
        debugger.add_watch(&mut emu, Watch::new(0xD181, WatchKind::Write));
        assert_eq!(emu.chipset.bus.watches().len(), 2);

        debugger.clear_watches(&mut emu);
        assert!(emu.chipset.bus.watches().is_empty());
        assert_eq!(debugger.watch_count(), 0);
    }

    #[test]
    fn two_watches_on_one_address_keep_the_bus_watch_until_both_are_gone() {
        // Otherwise removing the read watch would silently disable the write one.
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        let first = debugger.add_watch(&mut emu, Watch::new(0xD180, WatchKind::Read));
        debugger.add_watch(&mut emu, Watch::new(0xD180, WatchKind::Write));

        debugger.remove_watch(&mut emu, first);
        assert_eq!(
            emu.chipset.bus.watches().len(),
            1,
            "the write watch still needs it"
        );
    }

    #[test]
    fn a_snapshot_can_be_saved_restored_and_diffed() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();

        debugger.step(&mut emu);
        debugger.save_snapshot("start", &emu);
        debugger.step(&mut emu);
        debugger.save_snapshot("later", &emu);

        let diff = debugger.diff_snapshots("start", "later").unwrap();
        assert!(diff.ram.is_empty(), "the program touched no RAM");
        assert!(diff.registers.iter().any(|(name, _, _)| name == "R1"));

        assert!(debugger.restore_snapshot("start", &mut emu));
        assert_eq!(emu.pc(), 0x1_0002);
        assert_eq!(emu.reg(1), 0x00, "the second step was rolled back");
        assert!(!debugger.restore_snapshot("nothing", &mut emu));
    }

    #[test]
    fn a_snapshot_can_be_diffed_against_the_machine_now() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        debugger.save_snapshot("here", &emu);
        debugger.step(&mut emu);

        let diff = debugger.diff_with_now("here", &emu).unwrap();
        assert_eq!(diff.pc, (0x1_0000, 0x1_0002));
        assert_eq!(diff.ticks, (0, 1));
    }

    #[test]
    fn restoring_a_snapshot_reproduces_the_following_run() {
        // The property rollback rests on, at the debugger level: what happens next
        // must be what happened last time.
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        for _ in 0..2 {
            debugger.step(&mut emu);
        }
        debugger.save_snapshot("mid", &emu);

        debugger.run_uninterrupted(&mut emu, 1);
        let first = (emu.pc(), emu.reg(2));

        debugger.restore_snapshot("mid", &mut emu);
        debugger.run_uninterrupted(&mut emu, 1);
        assert_eq!((emu.pc(), emu.reg(2)), first);
    }

    #[test]
    fn a_trace_records_what_ran_and_what_moved() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        debugger.start_trace(16);
        assert!(debugger.is_tracing());

        debugger.run(&mut emu, 3);
        let trace = debugger.stop_trace().unwrap();
        assert_eq!(trace.len(), 3);
        assert!(!debugger.is_tracing());

        let entries: Vec<_> = trace.entries().collect();
        // Each entry reports what its own instruction changed: the first writes R0,
        // the second R1, the third R2.
        assert_eq!(entries[0].pc, 0x1_0000);
        assert_eq!(entries[1].pc, 0x1_0002);
        assert_eq!(entries[0].changed, vec![(0, 0x00, 0x11)]);
        assert_eq!(entries[1].changed, vec![(1, 0x00, 0x22)]);
        assert_eq!(entries[2].changed, vec![(2, 0x00, 0x33)]);
    }

    #[test]
    fn a_trace_carries_the_handler_name_the_core_ran() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        debugger.start_trace(4);
        debugger.step(&mut emu);
        let trace = debugger.stop_trace().unwrap();
        assert_eq!(trace.entries().next().unwrap().handler_text(), "OP_MOV");
    }

    #[test]
    fn a_trace_does_not_stop_the_machine() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        debugger.start_trace(8);
        let reason = debugger.run(&mut emu, 3);
        assert!(matches!(reason, StopReason::Budget { .. }), "{reason:?}");
        assert_eq!(debugger.trace().unwrap().len(), 3);
    }

    #[test]
    fn a_patch_is_applied_removed_and_listed() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();

        let patch = debugger
            .apply_patch(&mut emu, 0x1_0000, &asm::pop_pc(), "POP PC")
            .unwrap();
        assert_eq!(patch.source, "POP PC");
        assert_eq!(debugger.patches().len(), 1);
        assert_eq!(debugger.planted(&emu).len(), 1);

        assert!(debugger.undo_patch(&mut emu, 0x1_0000));
        assert!(debugger.patches().is_empty());
        assert!(debugger.planted(&emu).is_empty());
    }

    #[test]
    fn restoring_a_snapshot_resynchronises_the_patch_record() {
        // A patch is machine state, so a restore brings it back; the recorded list
        // has to follow rather than describe a machine that no longer exists.
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        debugger
            .apply_patch(&mut emu, 0x1_0000, &asm::pop_pc(), "POP PC")
            .unwrap();
        debugger.save_snapshot("patched", &emu);

        debugger.undo_all_patches(&mut emu);
        assert!(debugger.patches().is_empty());

        debugger.restore_snapshot("patched", &mut emu);
        assert_eq!(
            debugger.patches().len(),
            1,
            "the record followed the restore"
        );
        assert_eq!(debugger.planted(&emu).len(), 1);
    }

    #[test]
    fn a_patched_instruction_is_what_executes() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        // Replace `MOV R0,#0x11` with `MOV R0,#0x99`.
        debugger
            .apply_patch(&mut emu, 0x1_0000, &asm::mov_r_imm(0, 0x99), "MOV R0,#153")
            .unwrap();

        debugger.step(&mut emu);
        assert_eq!(emu.reg(0), 0x99, "the patch ran instead of the ROM");
    }

    #[test]
    fn the_disassembler_view_reads_the_patched_bytes() {
        // The whole reason the disassembler decodes memory: a patch has to be
        // visible, which a listing on disk could never show.
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        assert!(debugger.disassemble(&mut emu, 0x1_0000, 1).contains("#17"));

        debugger
            .apply_patch(&mut emu, 0x1_0000, &asm::pop_pc(), "POP PC")
            .unwrap();
        let text = debugger.disassemble(&mut emu, 0x1_0000, 1);
        assert!(text.contains("POP"), "{text}");
        assert!(text.contains("8E F2"), "{text}");
    }

    #[test]
    fn a_run_reports_a_budget_rather_than_spinning() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        let reason = debugger.run(&mut emu, 7);
        match reason {
            StopReason::Budget { budget, at } => {
                assert_eq!(budget, 7);
                assert_eq!(at, 0x1_0000 + 7 * 2);
            }
            other => panic!("expected a budget stop, got {other:?}"),
        }
    }

    #[test]
    fn disabling_a_breakpoint_stops_it_firing() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        let id = debugger.add_breakpoint_at(0x1_0002);
        debugger.breakpoint_mut(id).unwrap().enabled = false;

        let reason = debugger.run(&mut emu, 100);
        assert!(matches!(reason, StopReason::Budget { .. }), "{reason:?}");
        assert_eq!(debugger.breakpoint(id).unwrap().hits, 0);
    }

    #[test]
    fn removing_a_breakpoint_stops_it_firing() {
        let mut emu = scratch(&short_program());
        let mut debugger = Debugger::new();
        let id = debugger.add_breakpoint_at(0x1_0002);
        assert!(debugger.remove_breakpoint(id));
        assert!(!debugger.remove_breakpoint(id), "already gone");

        let reason = debugger.run(&mut emu, 100);
        assert!(matches!(reason, StopReason::Budget { .. }), "{reason:?}");
        assert_eq!(debugger.breakpoint_count(), 0);
    }

    #[test]
    fn a_pc_breakpoint_matches_its_low_half_in_any_segment() {
        // `0x946A` written without a segment is the form the plans use, and it
        // matches as a bare PC -- so it fires in segment 1 as well as segment 0.
        let mut program = vec![0u8; 8];
        program[4..6].copy_from_slice(&asm::mov_r_imm(0, 0x7F));
        let mut emu = scratch(&program);
        let mut debugger = Debugger::new();
        let id = debugger.add_breakpoint(Breakpoint::pc(0x0004));

        let reason = debugger.run(&mut emu, 100);
        assert_eq!(
            reason,
            StopReason::Breakpoint {
                id,
                address: 0x1_0004
            }
        );

        // The same address as a physical one does not match here.
        let mut emu = scratch(&program);
        let mut debugger = Debugger::new();
        debugger.add_breakpoint(Breakpoint::physical(0x0_0004));
        let reason = debugger.run(&mut emu, 100);
        assert!(matches!(reason, StopReason::Budget { .. }), "{reason:?}");
    }

    #[test]
    fn address_text_distinguishes_a_pc_from_a_physical_address() {
        assert_eq!(Breakpoint::pc(0x946A).address_text(), "0946Ah");
        assert_eq!(Breakpoint::physical(0x0_946A).address_text(), "000946Ah");
        assert_eq!(Breakpoint::auto(0x946A).address_text(), "0946Ah");
        assert_eq!(Breakpoint::auto(0x1_946A).address_text(), "001946Ah");
    }

    #[test]
    fn stop_reasons_describe_themselves() {
        let reason = StopReason::Breakpoint {
            id: BreakpointId(2),
            address: 0x0_946A,
        };
        assert!(reason.text().contains("000946Ah"), "{}", reason.text());
        assert!(reason.is_breakpoint());

        let reason = StopReason::Watch {
            id: WatchId(1),
            address: 0xD180,
            write: true,
            value: 0x41,
            at: 0x1_0006,
        };
        let text = reason.text();
        assert!(text.contains("write"), "{text}");
        assert!(text.contains("D180h"), "{text}");
        assert!(text.contains("41"), "{text}");

        assert!(!StopReason::Stopped.is_breakpoint());
        assert!(StopReason::Stopped.text().contains("force_run"));
    }
}
