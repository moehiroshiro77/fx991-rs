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

//! Interrupt arbitration.
//!
//! Derived from the hardware's documented behaviour.
//!
//! Indices double as vector indices: the vector address is `index * 2`
//!.  Keyboard is index 5, timer index 9.
//!
//! The arbitration loop keeps its rough edges, because
//! one of them is load-bearing: a maskable request whose SFR mask bit is clear
//! still "succeeds" as far as the run loop is concerned -- `run_mode` returns to
//! running and the request is retired -- it just never reaches the CPU.

/// `INT_CHECKFLAG`, unused.
pub const INT_CHECKFLAG: usize = 0;
/// Reset; takes the vector at `0x0002`.
pub const INT_RESET: usize = 1;
/// `BRK`; vector at `0x0004`.
pub const INT_BREAK: usize = 2;
/// Host-injected; vector at `0x0006`.
pub const INT_EMULATOR: usize = 3;
/// Non-maskable; vector at `0x0008`.
pub const INT_NONMASKABLE: usize = 4;
/// First maskable index; vectors run from `0x000A`.
pub const INT_MASKABLE: usize = 5;
/// First software interrupt; vectors run from `0x0080` (`SWI #0`).
pub const INT_SOFTWARE: usize = 64;
/// Number of interrupt slots.
pub const INT_COUNT: usize = 128;

/// Software interrupts are raised at `index + this`.
const SOFTWARE_BASE: u32 = 0x40;

/// The SFR field is indexed from this interrupt number.
pub const MANAGED_INTERRUPT_BASE: usize = 4;
/// Number of bits in the mask/pending fields.
pub const MANAGED_INTERRUPT_AMOUNT: usize = 13;
/// Mask for the 13-bit interrupt fields.
pub const INTERRUPT_BITFIELD_MASK: u16 = (1 << MANAGED_INTERRUPT_AMOUNT) - 1;

/// The keyboard's interrupt index.
pub const KEYBOARD_INTERRUPT: usize = 5;
/// The timer's interrupt index.
pub const TIMER_INTERRUPT: usize = 9;

/// What the CPU is doing between instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Stopped, waiting for an interrupt.
    Stop,
    /// Halted.
    Halt,
    /// Executing.
    Run,
}

/// Interrupt controller state and request bookkeeping.
#[derive(Debug, Clone)]
pub struct Interrupts {
    /// `0xF010`, 13 bits.
    pub mask: u16,
    /// `0xF014`, 13 bits.
    pub pending: u16,
    /// Which sources have an outstanding request.
    pub active: [bool; INT_COUNT],
    /// Requests outstanding, so `accept` is only called when there is something.
    pub pending_count: usize,
    /// Per-source result of the last try, mirroring `InterruptSource:raise_success`.
    success: [bool; INT_COUNT],
    /// What the CPU is doing.
    pub run_mode: RunMode,
}

impl Default for Interrupts {
    fn default() -> Self {
        Self::new()
    }
}

impl Interrupts {
    /// A controller with everything idle and the CPU running.
    pub fn new() -> Self {
        Self {
            mask: 0,
            pending: 0,
            active: [false; INT_COUNT],
            pending_count: 0,
            success: [false; INT_COUNT],
            run_mode: RunMode::Run,
        }
    }

    /// `Chipset::Reset`: clear the SFRs and arm the reset interrupt.
    pub fn reset(&mut self) {
        self.mask = 0;
        self.pending = 0;
        self.active = [false; INT_COUNT];
        self.success = [false; INT_COUNT];
        self.active[INT_RESET] = true;
        self.pending_count = 1;
        self.run_mode = RunMode::Run;
    }

    /// Whether the mask SFR enables this source.
    pub fn enabled(&self, index: usize) -> bool {
        self.mask & (1 << (index - MANAGED_INTERRUPT_BASE)) != 0
    }

    /// Whether the pending SFR has this source latched.
    pub fn is_pending(&self, index: usize) -> bool {
        self.pending & (1 << (index - MANAGED_INTERRUPT_BASE)) != 0
    }

    /// Latch the pending bit for a source.
    pub fn set_pending(&mut self, index: usize) {
        self.pending |= 1 << (index - MANAGED_INTERRUPT_BASE);
        self.pending &= INTERRUPT_BITFIELD_MASK;
    }

    fn activate(&mut self, index: usize) {
        if self.active[index] {
            return;
        }
        self.active[index] = true;
        self.pending_count += 1;
    }

    /// `InterruptSource::TryRaise`: refuse when masked out or already latched.
    ///
    /// The refusal is the important part.  A source that is already pending can
    /// never re-request, so if the ROM clears the pending bit and then parks
    /// before the source gets a chance to speak again, nothing will ever wake it.
    pub fn try_raise(&mut self, index: usize) -> bool {
        let accepted = self.enabled(index) && !self.is_pending(index);
        self.success[index] = accepted;
        if accepted {
            self.activate(index);
        }
        accepted
    }

    /// `InterruptSource::Success`: the request stuck *and* the CPU has seen it.
    pub fn success(&self, index: usize) -> bool {
        self.success[index] && self.is_pending(index)
    }

    /// `Chipset::RaiseMaskable`.
    pub fn raise_maskable(&mut self, index: usize) {
        debug_assert!((INT_MASKABLE..INT_SOFTWARE).contains(&index));
        self.activate(index);
    }

    /// `Chipset::RaiseSoftware` -- `SWI #n` becomes index `n + 0x40`.
    pub fn raise_software(&mut self, index: u32) {
        let index = index as usize + SOFTWARE_BASE as usize;
        if index < INT_COUNT {
            self.activate(index);
        }
    }

    /// `Chipset::Break`, including its "reset when nested too deep" escape.
    pub fn break_(&mut self, exception_level: usize) -> bool {
        if exception_level > 1 {
            // The caller must perform a full reset; it is not done here.
            return true;
        }
        if self.active[INT_BREAK] {
            return false;
        }
        self.active[INT_BREAK] = true;
        self.pending_count += 1;
        false
    }

    /// `Chipset::Halt`.
    pub fn halt(&mut self) {
        self.run_mode = RunMode::Halt;
    }

    /// `Chipset::Stop`.
    pub fn stop(&mut self) {
        self.run_mode = RunMode::Stop;
    }

    /// Pick the highest-priority outstanding request, or `None`.
    ///
    /// Mirrors.  The indices are meaningful (they *are* the
    /// interrupt numbers, which is also the vector index / 2), so the loops keep the
    /// Indexed rather than iterated so the bit index is explicit.
    #[allow(clippy::needless_range_loop)]
    pub fn select(&self, old_level: usize) -> Option<usize> {
        if self.active[INT_RESET] {
            return Some(INT_RESET);
        }
        for index in INT_SOFTWARE..INT_COUNT {
            if self.active[index] {
                return Some(index);
            }
        }
        if self.active[INT_EMULATOR] {
            return Some(INT_EMULATOR);
        }
        if self.active[INT_BREAK] {
            return Some(INT_BREAK);
        }
        if self.active[INT_NONMASKABLE] && old_level <= 2 {
            return Some(INT_NONMASKABLE);
        }
        if old_level <= 1 {
            for index in INT_MASKABLE..INT_SOFTWARE {
                if self.active[index] {
                    return Some(index);
                }
            }
        }
        None
    }

    /// The exception level an interrupt is taken at.
    pub fn exception_level_for(index: usize) -> u8 {
        match index {
            INT_RESET => 0,
            INT_BREAK | INT_NONMASKABLE => 2,
            INT_EMULATOR => 3,
            _ => 1,
        }
    }

    /// Retire a request once it has been dispatched.
    pub fn retire(&mut self, index: usize) {
        self.active[index] = false;
        self.pending_count = self.pending_count.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_masked_out_source_refuses_and_does_not_latch() {
        let mut ints = Interrupts::new();
        ints.reset();
        assert!(!ints.try_raise(KEYBOARD_INTERRUPT));
        assert!(!ints.is_pending(KEYBOARD_INTERRUPT));
        assert_eq!(ints.pending_count, 1, "only the reset request is live");
    }

    #[test]
    fn a_latched_source_refuses_a_second_request() {
        let mut ints = Interrupts::new();
        assert_eq!(ints.pending_count, 0, "a bare controller has no requests");
        ints.mask = 1 << (TIMER_INTERRUPT - MANAGED_INTERRUPT_BASE);
        assert!(ints.try_raise(TIMER_INTERRUPT));
        assert_eq!(ints.pending_count, 1);

        ints.set_pending(TIMER_INTERRUPT);
        assert!(!ints.try_raise(TIMER_INTERRUPT), "already latched");
        assert_eq!(ints.pending_count, 1, "no second activation");
        assert!(!ints.success(TIMER_INTERRUPT), "the request did not stick");
    }

    #[test]
    fn the_timer_mask_bit_is_bit_five_of_the_sfr_field() {
        // The field is indexed from `MANAGED_INTERRUPT_BASE`, so timer index 9 is
        // bit 5 -- a detail that is easy to get wrong and silently disables the
        // interrupt.
        let mut ints = Interrupts::new();
        ints.mask = 1 << 5;
        assert!(ints.enabled(TIMER_INTERRUPT));
        ints.mask = 1 << 4;
        assert!(!ints.enabled(TIMER_INTERRUPT));
    }

    #[test]
    fn reset_outranks_everything() {
        let mut ints = Interrupts::new();
        ints.reset();
        ints.mask = 0x3F;
        ints.try_raise(TIMER_INTERRUPT);
        assert_eq!(ints.select(0), Some(INT_RESET));
    }

    #[test]
    fn maskable_requests_are_ignored_above_exception_level_one() {
        let mut ints = Interrupts::new();
        ints.mask = 0x3F;
        ints.try_raise(TIMER_INTERRUPT);
        assert_eq!(ints.select(2), None);
        assert_eq!(ints.select(1), Some(TIMER_INTERRUPT));
    }

    #[test]
    fn software_interrupts_are_immediately_visible() {
        let mut ints = Interrupts::new();
        ints.raise_software(0);
        assert_eq!(ints.select(0), Some(INT_SOFTWARE));
        assert_eq!(Interrupts::exception_level_for(INT_SOFTWARE), 1);
    }

    #[test]
    fn the_bitfield_mask_covers_exactly_thirteen_bits() {
        assert_eq!(INTERRUPT_BITFIELD_MASK, 0x1FFF);
        let mut ints = Interrupts::new();
        ints.pending = 0xFFFF;
        ints.set_pending(TIMER_INTERRUPT);
        assert_eq!(ints.pending, 0x1FFF);
    }
}
