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

//! Timer, with its two-stage divider.
//!
//! The divider has two levels and **both** matter:
//!
//! * `DivideTicks` runs only when `ext_to_int_counter == ext_to_int_next`, i.e.
//!   once every `ext_to_int_frequency` **CPU instructions**.  With
//!   `ext_to_int_frequency = 10000` and 2 Mi instructions/s for
//!   HW_CLASSWIZ that divider works out to one tick per 209.7 instructions.
//! * Only inside `DivideTicks` does `data_counter` advance, and it requests an
//!   interrupt when `data_counter == data_interval`.
//!
//! So `data_interval = 1` means "fire every ~210 instructions", **not every
//! instruction**.  That factor of 209 is not cosmetic.  The ROM clears the timer's
//! pending bit at `rom:09200` and enters STOP ten instructions later at
//! `rom:09212`; a per-instruction timer re-latches inside that ten-instruction
//! window, `InterruptSource::TryRaise` then refuses forever because the bit is
//! already set, `pending_count` stays 0, and the machine wedges in STOP with no
//! way out.  See
//!
//! There is no wall-clock coupling here: one instruction is one tick and the
//! divider is a plain counter.  That makes emulation reproducible, which matters
//! more for this project than running at real speed -- the front end adds real
//! pacing on top (see `fx991::throttle`).

use crate::interrupts::{Interrupts, TIMER_INTERRUPT};

/// `0xF020`, 16-bit; writing 0 makes it 1
pub const INTERVAL: u32 = 0x0_F020;
/// `0xF022`, 16-bit; any write clears it
pub const COUNTER: u32 = 0x0_F022;
/// `0xF024`, one byte of unknown purpose
pub const UNKNOWN_F024: u32 = 0x0_F024;
/// `0xF025`, bit 0 starts the timer
pub const CONTROL: u32 = 0x0_F025;

/// How many CPU instructions pass between two divides.
pub const EXT_TO_INT_FREQUENCY: u64 = 10000;

/// The instruction rate `HW_CLASSWIZ` runs at.
pub const HW_CLASSWIZ_CYCLES_PER_SECOND: u64 = 2 * 1024 * 1024;

/// Instructions between divider ticks (209.7, truncated to 209).
pub const TICKS_PER_DIVIDE: u64 = HW_CLASSWIZ_CYCLES_PER_SECOND / EXT_TO_INT_FREQUENCY;

/// The timer.
pub struct Timer {
    /// `0xF020`
    pub data_interval: u16,
    /// `0xF022`
    pub data_counter: u16,
    /// `0xF024`
    pub data_f024: u8,
    /// `0xF025`
    pub data_control: u8,
    /// Whether the divider has asked for an interrupt that has not been serviced.
    pub raise_required: bool,
    instructions_since_divide: u64,
    /// Overridable so a test can drive the divider without 209 instructions.
    ticks_per_divide: u64,
}

/// The timer's state, for snapshot and restore.
///
/// `instructions_since_divide` is part of it: the divider's progress decides
/// which instruction the next timer interrupt lands on, so a snapshot that
/// dropped it would restore a machine that is subtly out of phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerState {
    /// `0xF020`
    pub data_interval: u16,
    /// `0xF022`
    pub data_counter: u16,
    /// `0xF024`
    pub data_f024: u8,
    /// `0xF025`
    pub data_control: u8,
    /// Whether the divider asked for an interrupt not yet serviced.
    pub raise_required: bool,
    /// How far the divider has counted towards the next tick.
    pub instructions_since_divide: u64,
    /// The divider period in use.
    pub ticks_per_divide: u64,
}

impl Default for Timer {
    fn default() -> Self {
        Self::new()
    }
}

impl Timer {
    /// Everything needed to put the timer back where it was.
    pub fn snapshot(&self) -> TimerState {
        TimerState {
            data_interval: self.data_interval,
            data_counter: self.data_counter,
            data_f024: self.data_f024,
            data_control: self.data_control,
            raise_required: self.raise_required,
            instructions_since_divide: self.instructions_since_divide,
            ticks_per_divide: self.ticks_per_divide,
        }
    }

    /// Restore a [`TimerState`].
    pub fn restore(&mut self, state: &TimerState) {
        self.data_interval = state.data_interval;
        self.data_counter = state.data_counter;
        self.data_f024 = state.data_f024;
        self.data_control = state.data_control;
        self.raise_required = state.raise_required;
        self.instructions_since_divide = state.instructions_since_divide;
        self.ticks_per_divide = state.ticks_per_divide;
    }

    /// A stopped timer.
    pub fn new() -> Self {
        Self {
            data_interval: 0,
            data_counter: 0,
            data_f024: 0,
            data_control: 0,
            raise_required: false,
            instructions_since_divide: 0,
            ticks_per_divide: TICKS_PER_DIVIDE,
        }
    }

    /// Override the divider period, for tests.
    pub fn set_ticks_per_divide(&mut self, ticks: u64) {
        self.ticks_per_divide = ticks.max(1);
    }

    /// The divider period currently in use.
    pub fn ticks_per_divide(&self) -> u64 {
        self.ticks_per_divide
    }

    /// How far the divider has counted towards the next tick.
    ///
    /// This is the counter the hardware actually divides by, so a debugger
    /// showing timer state needs it: `data_counter` only advances *after* a
    /// divide, and watching it alone makes the divider look stalled.
    pub fn instructions_since_divide(&self) -> u64 {
        self.instructions_since_divide
    }

    /// `Timer::Reset`.
    pub fn reset(&mut self) {
        self.data_counter = 0;
        self.instructions_since_divide = 0;
        self.raise_required = false;
        self.data_control = 0;
    }

    /// One instruction has elapsed (`Timer::Tick`).
    pub fn tick(&mut self, interrupts: &mut Interrupts) {
        self.instructions_since_divide += 1;
        if self.instructions_since_divide >= self.ticks_per_divide {
            self.instructions_since_divide = 0;
            self.divide_ticks(interrupts);
        }

        if self.raise_required {
            interrupts.try_raise(TIMER_INTERRUPT);
        }
    }

    /// `Timer::DivideTicks`, minus the hardware-id-gated branch.
    pub fn divide_ticks(&mut self, interrupts: &mut Interrupts) {
        if self.data_control & 0x01 != 0 {
            if self.data_counter == self.data_interval {
                self.data_counter = 0;
                if interrupts.enabled(TIMER_INTERRUPT) {
                    self.raise_required = true;
                }
            }
            self.data_counter = self.data_counter.wrapping_add(1);
        }
    }

    /// `Timer::TickAfterInterrupts`.
    pub fn tick_after_interrupts(&mut self, interrupts: &Interrupts) {
        if self.raise_required && interrupts.success(TIMER_INTERRUPT) {
            self.raise_required = false;
        }
    }

    /// Read an SFR this peripheral owns.
    pub fn read(&mut self, address: u32) -> Option<u8> {
        match address {
            _ if address == INTERVAL || address == INTERVAL + 1 => {
                let shift = ((address - INTERVAL) & 1) * 8;
                Some((self.data_interval >> shift) as u8)
            }
            _ if address == COUNTER || address == COUNTER + 1 => {
                let shift = ((address - COUNTER) & 1) * 8;
                Some((self.data_counter >> shift) as u8)
            }
            UNKNOWN_F024 => Some(self.data_f024),
            CONTROL => Some(self.data_control & 0x01),
            _ => None,
        }
    }

    /// Write an SFR this peripheral owns.
    pub fn write(&mut self, address: u32, value: u8) -> bool {
        match address {
            _ if address == INTERVAL || address == INTERVAL + 1 => {
                let shift = ((address - INTERVAL) & 1) * 8;
                self.data_interval &= !(0xFFu16 << shift);
                self.data_interval |= (value as u16) << shift;
                if self.data_interval == 0 {
                    self.data_interval = 1;
                }
                true
            }
            _ if address == COUNTER || address == COUNTER + 1 => {
                self.data_counter = 0; // a write clears the counter
                true
            }
            UNKNOWN_F024 => {
                self.data_f024 = value;
                true
            }
            CONTROL => {
                self.data_control = value & 0x01;
                self.raise_required = false;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interrupts::{Interrupts, MANAGED_INTERRUPT_BASE};

    fn running_timer(interval: u16, per_divide: u64) -> (Timer, Interrupts) {
        let mut timer = Timer::new();
        timer.set_ticks_per_divide(per_divide);
        timer.data_interval = interval;
        timer.data_control = 1;
        let mut interrupts = Interrupts::new();
        interrupts.mask = 1 << (TIMER_INTERRUPT - MANAGED_INTERRUPT_BASE);
        (timer, interrupts)
    }

    #[test]
    fn the_divider_is_two_hundred_nine_instructions_not_one() {
        assert_eq!(TICKS_PER_DIVIDE, 209);
    }

    #[test]
    fn the_counter_only_advances_on_divider_ticks() {
        let (mut timer, mut interrupts) = running_timer(1, 10);
        for _ in 0..9 {
            timer.tick(&mut interrupts);
        }
        assert_eq!(timer.data_counter, 0, "divider has not come due");

        // The tenth instruction runs DivideTicks: compare (0 == 1? no) then increment.
        timer.tick(&mut interrupts);
        assert_eq!(timer.data_counter, 1);
        assert!(!timer.raise_required);
        assert!(!interrupts.is_pending(TIMER_INTERRUPT));
    }

    #[test]
    fn interval_one_needs_the_compare_to_succeed_which_is_the_second_divider_tick() {
        // `data_counter == data_interval` is a comparison, not a threshold, so with
        // interval 1 it takes counter 1 -- the *second* divider tick.
        let (mut timer, mut interrupts) = running_timer(1, 10);
        for _ in 0..20 {
            timer.tick(&mut interrupts);
        }
        assert!(timer.raise_required, "fired on the second divider tick");
        assert_eq!(timer.data_counter, 1, "reset to 0 then incremented to 1");
    }

    #[test]
    fn interval_two_fires_on_the_third_divider_tick() {
        let (mut timer, mut interrupts) = running_timer(2, 10);
        for _ in 0..20 {
            timer.tick(&mut interrupts);
        }
        assert!(!timer.raise_required, "0 and 1 both compared unequal");
        for _ in 0..10 {
            timer.tick(&mut interrupts);
        }
        assert!(timer.raise_required, "2 == 2 on the third tick");
    }

    #[test]
    fn a_masked_out_timer_does_not_request() {
        let mut timer = Timer::new();
        timer.set_ticks_per_divide(1);
        timer.data_interval = 1;
        timer.data_control = 1;
        let mut interrupts = Interrupts::new();
        assert!(!interrupts.enabled(TIMER_INTERRUPT));
        for _ in 0..10 {
            timer.tick(&mut interrupts);
        }
        assert!(!timer.raise_required);
        assert!(!interrupts.is_pending(TIMER_INTERRUPT));
    }

    #[test]
    fn a_stopped_timer_counts_nothing() {
        let (mut timer, mut interrupts) = running_timer(1, 1);
        timer.data_control = 0;
        for _ in 0..10 {
            timer.tick(&mut interrupts);
        }
        assert_eq!(timer.data_counter, 0);
        assert!(!timer.raise_required);
    }

    #[test]
    fn writing_an_interval_of_zero_makes_it_one() {
        let mut timer = Timer::new();
        assert!(timer.write(INTERVAL, 0x00));
        assert!(timer.write(INTERVAL + 1, 0x00));
        assert_eq!(timer.data_interval, 1);
    }

    #[test]
    fn writing_the_counter_clears_it() {
        let mut timer = Timer::new();
        timer.data_counter = 1234;
        assert!(timer.write(COUNTER, 0x00));
        assert_eq!(timer.data_counter, 0);
    }

    #[test]
    fn the_control_register_is_one_bit() {
        let mut timer = Timer::new();
        timer.write(CONTROL, 0xFF);
        assert_eq!(timer.read(CONTROL), Some(0x01));
    }

    #[test]
    fn the_divider_period_is_the_reference_s_ratio_truncated() {
        // 2 Mi instructions/s over 10000 divider ticks/s is 209.7, and the counter is
        // an integer, so the period is 209.  What matters for the ROM is the order of
        // magnitude: two hundred instructions, not one.  Compare through values so
        // the assertion is about the ratio rather than a literal.
        let ratio = HW_CLASSWIZ_CYCLES_PER_SECOND / EXT_TO_INT_FREQUENCY;
        assert_eq!(TICKS_PER_DIVIDE, ratio);
        assert!((200..=210).contains(&ratio), "ratio is {ratio}");
    }
}
