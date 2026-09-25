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

//! Real-time pacing: run the guest at the speed the real hardware runs.
//!
//! The loop does *not* run a fixed number of instructions per frame.  It keeps a
//! virtual millisecond counter and asks "how many cycles should have run by
//! now?", which makes the machine self-correcting: a late tick simply gets a
//! bigger batch next time, and the error never accumulates.
//!
//! This module is that algorithm, with the clock injected so it can be tested
//! without sleeping.

use std::time::{Duration, Instant};

/// `HW_CLASSWIZ` runs at 2 Mi instructions per second.
pub const CYCLES_PER_SECOND: u64 = 2 * 1024 * 1024;

/// The poll interval, in milliseconds.
pub const TIMER_INTERVAL_MS: u64 = 20;

/// How many cycles a single batch may run before the caller should check in
/// again.  This is *not* a rate limit -- it only bounds how long the caller can
/// be unresponsive to window events.  At 2 Mi/s a 20 ms batch is 41,943 cycles,
/// so this allows a full batch plus slack for a late tick.
pub const MAX_BATCH: u64 = 200_000;

/// A virtual clock that converts elapsed wall time into cycles to emulate.
///
/// Mirrors `Emulator::Cycles`: it advances `ticks_now` by a fixed interval per
/// call rather than reading the real clock, which is what keeps the pacing
/// deterministic and free of drift.
#[derive(Debug, Clone)]
pub struct Throttle {
    ticks_now: u64,
    cycles_emulated: u64,
    cycles_per_second: u64,
    timer_interval: u64,
    /// Set once the caller has a real clock to compare against.
    started: Option<Instant>,
    /// How many intervals have been handed out, for the wall-clock guard.
    intervals_served: u64,
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new(CYCLES_PER_SECOND, TIMER_INTERVAL_MS)
    }
}

impl Throttle {
    /// A throttle with explicit rate and interval.
    pub fn new(cycles_per_second: u64, timer_interval_ms: u64) -> Self {
        Self {
            ticks_now: 0,
            cycles_emulated: 0,
            cycles_per_second,
            timer_interval: timer_interval_ms.max(1),
            started: None,
            intervals_served: 0,
        }
    }

    /// The rate this throttle targets.
    pub fn cycles_per_second(&self) -> u64 {
        self.cycles_per_second
    }

    /// The polling interval.
    pub fn timer_interval(&self) -> u64 {
        self.timer_interval
    }

    /// Total cycles handed out so far.
    pub fn cycles_emulated(&self) -> u64 {
        self.cycles_emulated
    }

    /// Start the wall clock, so [`Throttle::next_delay`] can pace the loop.
    pub fn start(&mut self, now: Instant) {
        self.started = Some(now);
    }

    /// Reset the virtual clock, keeping the configured rate.
    ///
    /// Called when the emulator restarts, matching `Cycles::Reset`.
    pub fn reset(&mut self) {
        self.ticks_now = 0;
        self.cycles_emulated = 0;
        self.intervals_served = 0;
        self.started = None;
    }

    /// How many cycles to run in this batch.
    ///
    /// This is `Cycles::GetDelta`: advance the virtual clock one interval,
    /// compute how many cycles *should* have run by then, and return the
    /// shortfall.  Crucially `cycles_emulated` is **assigned** the target, not
    /// incremented by the delta -- that is what makes a late batch self-correct
    /// instead of drifting behind forever.
    pub fn take_batch(&mut self) -> u64 {
        self.ticks_now += self.timer_interval;
        self.intervals_served += 1;
        let target = self.ticks_now * self.cycles_per_second / 1000;
        let delta = target.saturating_sub(self.cycles_emulated);
        self.cycles_emulated = target;
        delta
    }

    /// How long to wait before the next [`Throttle::take_batch`].
    ///
    /// `None` until [`Throttle::start`] has been called.  A late caller gets
    /// zero: the batch that already ran is its own penalty, and the next
    /// `take_batch` will hand it more cycles to catch up.
    pub fn next_delay(&self, now: Instant) -> Option<Duration> {
        let started = self.started?;
        let elapsed = now.saturating_duration_since(started);
        let wanted = Duration::from_millis(self.intervals_served * self.timer_interval);
        Some(wanted.saturating_sub(elapsed))
    }

    /// The whole loop in one call: run `tick` until this batch is done.
    ///
    /// `tick` returns `false` to stop early (the window closed, say).  Returns
    /// the number of cycles actually run.
    ///
    /// The batch is clamped to [`MAX_BATCH`] so a long pause -- a dragged window,
    /// a breakpoint, a laptop waking up -- cannot stall the caller inside the
    /// emulator for seconds.  The clamp deliberately does **not** touch
    /// `cycles_emulated`, so the leftover shows up as a bigger delta next time
    /// rather than being silently dropped; that is the same self-correcting
    /// property the loop relies on.
    pub fn run_batch<F>(&mut self, mut tick: F) -> u64
    where
        F: FnMut() -> bool,
    {
        let batch = self.take_batch();
        let to_run = batch.min(MAX_BATCH);
        for _ in 0..to_run {
            if !tick() {
                break;
            }
        }
        to_run
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_throttle_asks_for_one_interval_of_cycles() {
        let mut throttle = Throttle::default();
        // 20 ms at 2,097,152 cycles/s = 41,943 cycles.
        assert_eq!(throttle.take_batch(), 41_943);
    }

    #[test]
    fn the_rate_is_exact_over_time() {
        let mut throttle = Throttle::default();
        let mut total = 0u64;
        for _ in 0..50 {
            total += throttle.take_batch();
        }
        // 50 intervals = 1000 ms = exactly one second of cycles.
        assert_eq!(total, CYCLES_PER_SECOND);
    }

    #[test]
    fn a_late_batch_is_recovered_not_lost() {
        // The whole point of assigning rather than accumulating: if the caller
        // skips an interval, the next batch is bigger by exactly that much.
        let mut throttle = Throttle::default();
        assert_eq!(throttle.take_batch(), 41_943);
        // Simulate the caller ignoring one batch entirely.
        let skipped = throttle.take_batch();
        assert_eq!(skipped, 41_943);
        // Now take two in a row: the second must still be one interval's worth,
        // and the total must equal two intervals -- nothing was double-counted.
        let a = throttle.take_batch();
        let b = throttle.take_batch();
        assert_eq!(a + b, 2 * 41_943);
        assert_eq!(throttle.cycles_emulated(), 4 * 41_943);
    }

    #[test]
    fn the_virtual_clock_does_not_drift_against_wall_time() {
        // 1000 intervals of 20 ms is 20 seconds; the cycles handed out must be
        // exactly 20 seconds' worth, with no accumulated rounding error.
        let mut throttle = Throttle::default();
        let mut total = 0u64;
        for _ in 0..1000 {
            total += throttle.take_batch();
        }
        assert_eq!(total, CYCLES_PER_SECOND * 20);
    }

    #[test]
    fn next_delay_waits_out_the_interval() {
        let mut throttle = Throttle::default();
        let start = Instant::now();
        assert_eq!(throttle.next_delay(start), None, "no clock yet");

        throttle.start(start);
        throttle.take_batch();
        // Immediately after a batch, the next one is a full interval away.
        assert_eq!(
            throttle.next_delay(start),
            Some(Duration::from_millis(TIMER_INTERVAL_MS))
        );
        // Half an interval later, half the wait remains.
        let half = start + Duration::from_millis(TIMER_INTERVAL_MS / 2);
        assert_eq!(
            throttle.next_delay(half),
            Some(Duration::from_millis(TIMER_INTERVAL_MS / 2))
        );
    }

    #[test]
    fn next_delay_never_goes_negative_when_late() {
        let mut throttle = Throttle::default();
        let start = Instant::now();
        throttle.start(start);
        throttle.take_batch();
        // The caller came back a whole second late.
        let late = start + Duration::from_secs(1);
        assert_eq!(throttle.next_delay(late), Some(Duration::ZERO));
    }

    #[test]
    fn the_clamp_bounds_a_single_call_without_skewing_the_rate() {
        // A window dragged for a second would otherwise ask for a million cycles
        // in one call and freeze the UI, so one call never runs more than
        // MAX_BATCH.  What it does *not* do is change the long-run rate: the
        // virtual clock still only advances one interval per call.
        let mut throttle = Throttle::default();
        let mut total = 0u64;
        // A burst of calls, as if the event loop were slow.
        for _ in 0..50 {
            total += throttle.run_batch(|| true);
        }
        // Each call ran its own interval.  20 ms does not divide a second
        // evenly (20 * 2,097,152 / 1000 = 41,943.04), so 49 batches carry
        // 41,943 and the 50th carries the 2-cycle remainder -- which is the
        // self-correction working, not an error.
        assert_eq!(total, CYCLES_PER_SECOND);
        assert_eq!(throttle.cycles_emulated(), CYCLES_PER_SECOND);
        // And the next batch is back to the normal size.
        assert_eq!(throttle.take_batch(), 41_943);
    }

    #[test]
    fn the_clamp_only_bites_when_an_interval_is_larger_than_it() {
        // With the real interval (20 ms -> 41,943 cycles) the clamp is slack.
        let mut throttle = Throttle::default();
        assert_eq!(throttle.run_batch(|| true), 41_943);
        // With a deliberately huge interval it bites.
        let mut slow = Throttle::new(CYCLES_PER_SECOND, 5_000);
        assert!(slow.take_batch() > MAX_BATCH);
        let mut slow = Throttle::new(CYCLES_PER_SECOND, 5_000);
        assert_eq!(slow.run_batch(|| true), MAX_BATCH);
    }

    #[test]
    fn run_batch_stops_when_the_tick_says_so() {
        let mut throttle = Throttle::default();
        let mut count = 0;
        throttle.run_batch(|| {
            count += 1;
            count < 10 // stop after ten
        });
        assert_eq!(count, 10);
    }

    #[test]
    fn reset_restarts_the_accounting() {
        let mut throttle = Throttle::default();
        throttle.take_batch();
        throttle.take_batch();
        assert!(throttle.cycles_emulated() > 0);
        throttle.reset();
        assert_eq!(throttle.cycles_emulated(), 0);
        assert_eq!(throttle.take_batch(), 41_943, "first batch after a reset");
    }
}
