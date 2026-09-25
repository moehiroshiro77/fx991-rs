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

//! The fx-991CN X chipset: peripherals, interrupt arbitration, and the tick order.
//!
//! This crate is the *machine around the CPU*.  It owns the bus, the peripherals,
//! and the order in which they are ticked, but it knows nothing about how a caller
//! wants to drive it -- breakpoints, tracing and the calculator front end live in
//! `fx991`.
//!
//! # How the peripherals reach the bus
//!
//! `fx991-bus` owns the address map and asks each registered [`SfrDevice`] in turn.
//! The peripherals live in the chipset, so both sides hold an `Rc<RefCell<.>>` to
//! the same state: the bus's device adapter is built first, then the chipset is
//! assembled around the same handles.  That keeps the decoder free of peripheral
//! knowledge without any unsafe aliasing.
//!
//! # Tick order
//!
//! `Chipset::Tick` is load-bearing for reproducibility and
//! is copied exactly:
//!
//! `text
//! for peripheral in peripherals: peripheral.Tick
//! if pending_interrupt_count: AcceptInterrupt
//! for peripheral in peripherals: peripheral.TickAfterInterrupts
//! if run_mode == RM_RUN: cpu.Next
//! `
//!
//! # Example
//!
//! `no_run
//! use fx991_bus:{Bus, Rom};
//! use fx991_chipset:Chipset;
//!
//! let rom = std::fs::read("data/rom_verF.bin").unwrap();
//! let mut chipset = Chipset::new(Bus::new(Rom::new(rom)));
//! chipset.reset;
//! chipset.tick(None);
//! assert_eq!(chipset.cpu.regs.pc, 0x946E);
//! `

#![warn(missing_docs)]

pub mod interrupts;
pub mod keyboard;
pub mod misc;
pub mod screen;
pub mod timer;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use fx991_bus::{Bus, SfrDevice};
use fx991_model::address;
use nxu8_core::ops::ctrl::ControlSink;
use nxu8_core::{Cpu, Memory};

pub use interrupts::{RunMode, INT_COUNT, INT_MASKABLE, INT_RESET, INT_SOFTWARE};

use interrupts::Interrupts;
use keyboard::Keyboard;
use misc::{Misc, StandbyRequest};
use screen::Screen;
use timer::Timer;

type Shared<T> = Rc<RefCell<T>>;

/// The interrupt mask (`0xF010`) and pending (`0xF014`) registers.
///
/// These are *not* a peripheral here: the chipset owns them directly.  Without
/// that the ROM's very first `ST ER0,0F010h` faults, the mask never gets set, and
/// the timer can never request anything -- the machine parks in STOP at tick 170 and
/// stays there.
struct InterruptSfr {
    interrupts: Shared<Interrupts>,
}

impl SfrDevice for InterruptSfr {
    fn read(&mut self, address: u32) -> Option<u8> {
        // The byte offset within the register, not within the whole block: the two
        // registers are four bytes apart, so `address - 0xF010` would underflow for
        // the pending register.
        let shift = (address & 1) * 8;
        match address {
            0xF010 | 0xF011 => Some((self.interrupts.borrow().mask >> shift) as u8),
            0xF014 | 0xF015 => Some((self.interrupts.borrow().pending >> shift) as u8),
            _ => None,
        }
    }

    fn write(&mut self, address: u32, value: u8) -> bool {
        let shift = (address & 1) * 8;
        let field = 0xFFu16 << shift;
        match address {
            0xF010 | 0xF011 => {
                let mut ints = self.interrupts.borrow_mut();
                ints.mask = (ints.mask & !field) | ((value as u16) << shift);
                ints.mask &= interrupts::INTERRUPT_BITFIELD_MASK;
                true
            }
            0xF014 | 0xF015 => {
                let mut ints = self.interrupts.borrow_mut();
                ints.pending = (ints.pending & !field) | ((value as u16) << shift);
                ints.pending &= interrupts::INTERRUPT_BITFIELD_MASK;
                true
            }
            _ => false,
        }
    }

    fn name(&self) -> &'static str {
        "interrupt-sfr"
    }
}

/// `0xF000`: the data segment register.
///
///  binds this straight to the CPU's `reg_dsr`, and the CPU
/// clears that at the top of every instruction.  A write is therefore
/// visible to exactly one instruction, which the shared shadow reproduces.
struct DsrSfr {
    dsr: Rc<Cell<u8>>,
}

impl SfrDevice for DsrSfr {
    fn read(&mut self, address: u32) -> Option<u8> {
        (address == 0xF000).then(|| self.dsr.get())
    }

    fn write(&mut self, address: u32, value: u8) -> bool {
        if address == 0xF000 {
            self.dsr.set(value);
            return true;
        }
        false
    }

    fn name(&self) -> &'static str {
        "dsr"
    }
}

/// Routes an SFR access to whichever peripheral owns the address.
///
/// The bus owns the *address map*; this adapter owns the *routing*.  Adding a
/// peripheral means adding one arm here, never touching the decoder.
struct Peripherals {
    screen: Shared<Screen>,
    keyboard: Shared<Keyboard>,
    timer: Shared<Timer>,
    misc: Shared<Misc>,
    /// A standby request raised by a write, drained by the chipset on the next tick.
    standby: Rc<RefCell<Option<StandbyRequest>>>,
}

impl SfrDevice for Peripherals {
    fn read(&mut self, address: u32) -> Option<u8> {
        if let Some(value) = self.screen.borrow_mut().read(address) {
            return Some(value);
        }
        if let Some(value) = self.keyboard.borrow_mut().read(address) {
            return Some(value);
        }
        if let Some(value) = self.timer.borrow_mut().read(address) {
            return Some(value);
        }
        self.misc.borrow_mut().read(address)
    }

    fn write(&mut self, address: u32, value: u8) -> bool {
        if self.screen.borrow_mut().write(address, value) {
            return true;
        }
        if self.keyboard.borrow_mut().write(address, value) {
            return true;
        }
        if self.timer.borrow_mut().write(address, value) {
            return true;
        }
        if self.misc.borrow().owns(address) {
            if let Some(request) = self.misc.borrow_mut().write(address, value) {
                if request != StandbyRequest::Armed {
                    *self.standby.borrow_mut() = Some(request);
                }
            }
            return true;
        }
        false
    }

    fn name(&self) -> &'static str {
        "peripherals"
    }
}

/// The machine.
pub struct Chipset {
    /// The data space.
    pub bus: Bus,
    /// The processor.
    pub cpu: Cpu,
    /// Interrupt controller and run mode.
    pub interrupts: Shared<Interrupts>,
    /// The display.
    pub screen: Shared<Screen>,
    /// The key matrix.
    pub keyboard: Shared<Keyboard>,
    /// The timer.
    pub timer: Shared<Timer>,
    /// DSR and the unknown SFR block.
    pub misc: Shared<Misc>,

    standby: Rc<RefCell<Option<StandbyRequest>>>,
    /// Shadow of `0xF000`, copied into the CPU before each instruction.
    dsr: Rc<Cell<u8>>,
    /// Whether the machine has ever been halted or stopped.
    saw_stop: bool,
    raised_software: Vec<u32>,
    breaks: u32,
}

/// The order peripherals are ticked in.
///
/// The peripherals are ROMWindow, BatteryBackedRAM, Screen, Keyboard,
/// StandbyControl, Miscellaneous, Timer; none of the Tick bodies depend on each
/// other within one pass, so only the relative order of the two that do work
/// (keyboard before timer) matters -- and that is preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peripheral {
    /// The display has no per-tick work.
    Screen,
    /// The keyboard raises its interrupt when a filtered key is held.
    Keyboard,
    /// DSR and the unknown block have no per-tick work.
    Misc,
    /// The timer advances its divider.
    Timer,
}

impl Chipset {
    /// Build a machine, registering the peripherals on the bus.
    pub fn new(mut bus: Bus) -> Self {
        let screen = Rc::new(RefCell::new(Screen::new()));
        let keyboard = Rc::new(RefCell::new(Keyboard::new()));
        let timer = Rc::new(RefCell::new(Timer::new()));
        let misc = Rc::new(RefCell::new(Misc::new()));
        let interrupts = Rc::new(RefCell::new(Interrupts::new()));
        let standby = Rc::new(RefCell::new(None));
        let dsr = Rc::new(Cell::new(0));

        bus.register(Box::new(DsrSfr {
            dsr: Rc::clone(&dsr),
        }));
        bus.register(Box::new(InterruptSfr {
            interrupts: Rc::clone(&interrupts),
        }));
        bus.register(Box::new(Peripherals {
            screen: Rc::clone(&screen),
            keyboard: Rc::clone(&keyboard),
            timer: Rc::clone(&timer),
            misc: Rc::clone(&misc),
            standby: Rc::clone(&standby),
        }));

        Self {
            bus,
            cpu: Cpu::new(),
            interrupts,
            screen,
            keyboard,
            timer,
            misc,
            standby,
            dsr,
            saw_stop: false,
            raised_software: Vec::new(),
            breaks: 0,
        }
    }

    /// The order peripherals are ticked in, for documentation and tests.
    pub fn tick_order() -> [Peripheral; 4] {
        [
            Peripheral::Screen,
            Peripheral::Keyboard,
            Peripheral::Misc,
            Peripheral::Timer,
        ]
    }

    /// `Chipset::Reset`.
    pub fn reset(&mut self) {
        self.screen.borrow_mut().reset();
        self.keyboard.borrow_mut().reset();
        self.timer.borrow_mut().reset();
        self.misc.borrow_mut().reset();
        self.cpu.reset(&mut self.bus);
        self.interrupts.borrow_mut().reset();
        *self.standby.borrow_mut() = None;
        self.dsr.set(0);
        self.saw_stop = false;
        self.raised_software.clear();
        self.breaks = 0;
    }

    /// `Chipset::Tick`.
    ///
    /// `pre_execute` is called with the physical PC the CPU is about to execute,
    /// *after* interrupt dispatch and before the instruction fetch, and **even when
    /// the machine is stopped** -- a ROM parked on a STOP instruction stays on that
    /// address, and a watcher needs to see it repeat.  Returning `true` suppresses
    /// the instruction, which is how a caller observes an address that exists only
    /// between interrupt dispatch and the fetch (the reset vector at `0x946A` is the
    /// case that matters).
    pub fn tick(&mut self, mut pre_execute: Option<&mut dyn FnMut(u32) -> bool>) -> TickOutcome {
        // --- for peripheral in peripherals: peripheral.Tick ---------------
        self.screen.borrow_mut().tick();
        self.keyboard
            .borrow_mut()
            .tick(&mut self.interrupts.borrow_mut());
        {
            let timer = Rc::clone(&self.timer);
            let interrupts = Rc::clone(&self.interrupts);
            timer.borrow_mut().tick(&mut interrupts.borrow_mut());
        }

        // --- a standby write takes effect before the interrupt pass ---------
        if let Some(request) = self.standby.borrow_mut().take() {
            Misc::apply(request, &mut self.interrupts.borrow_mut());
        }

        // --- if pending_interrupt_count: AcceptInterrupt ------------------
        if self.interrupts.borrow().pending_count > 0 {
            self.accept_interrupt();
        }

        // --- for peripheral in peripherals: peripheral.TickAfterInterrupts -
        let interrupts = self.interrupts.borrow();
        self.timer.borrow_mut().tick_after_interrupts(&interrupts);
        drop(interrupts);

        if self.interrupts.borrow().run_mode != RunMode::Run {
            self.saw_stop = true;
        }

        // --- if run_mode == RM_RUN: cpu.Next ------------------------------
        if let Some(hook) = pre_execute.as_mut() {
            if hook(self.cpu.regs.physical_pc()) {
                return TickOutcome::Suppressed;
            }
        }

        if self.interrupts.borrow().run_mode != RunMode::Run {
            return TickOutcome::Stopped;
        }

        // A write to `0xF000` is visible to the instruction that follows it.
        self.cpu.regs.dsr = self.dsr.get();

        let mut sink = ChipsetSink {
            software: &mut self.raised_software,
            breaks: &mut self.breaks,
        };
        match self.cpu.step(&mut self.bus, &mut sink) {
            Some(handler) => TickOutcome::Executed(handler),
            None => TickOutcome::Skipped,
        }
    }

    /// `Chipset::AcceptInterrupt`.
    pub fn accept_interrupt(&mut self) -> Option<usize> {
        let old_level = self.cpu.exception_level();
        let index = self.interrupts.borrow().select(old_level)?;

        let exception_level = Interrupts::exception_level_for(index);

        if (INT_MASKABLE..INT_SOFTWARE).contains(&index) {
            if self.interrupts.borrow().enabled(index) {
                self.interrupts.borrow_mut().set_pending(index);
                if self.cpu.master_interrupt_enable() {
                    self.cpu
                        .raise_interrupt(&mut self.bus, exception_level, index as u32);
                }
            }
            // A source whose mask bit is clear is retired anyway, which is why a
            // request can be "accepted" without the CPU ever seeing it.
        } else {
            self.cpu
                .raise_interrupt(&mut self.bus, exception_level, index as u32);
        }

        self.interrupts.borrow_mut().run_mode = RunMode::Run;
        self.interrupts.borrow_mut().retire(index);
        Some(index)
    }

    /// Software interrupts the CPU has executed.
    pub fn raised_software(&self) -> &[u32] {
        &self.raised_software
    }

    /// `BRK` instructions executed.
    pub fn breaks(&self) -> u32 {
        self.breaks
    }

    /// Current run mode.
    pub fn run_mode(&self) -> RunMode {
        self.interrupts.borrow().run_mode
    }

    /// Force the machine into the running mode, discarding any standby request.
    ///
    /// A debugger needs this after parking the ROM: the STOP is requested by a write
    /// and applied on the *next* tick, so a script that reaches the idle loop and
    /// immediately overwrites the instruction would otherwise be stopped a tick later
    /// and never execute its own code.
    pub fn force_run(&mut self) {
        *self.standby.borrow_mut() = None;
        self.interrupts.borrow_mut().run_mode = RunMode::Run;
    }

    /// Whether the machine has ever halted or stopped.
    ///
    /// The ROM spends most of its life parked, so "did it ever sleep" is a more
    /// useful boot check than "is it asleep right now" -- the exact tick it wakes on
    /// depends on the timer.
    pub fn has_slept(&self) -> bool {
        self.saw_stop
    }

    /// True when the interrupt mask enables this source.
    pub fn interrupt_enabled(&self, index: usize) -> bool {
        self.interrupts.borrow().enabled(index)
    }

    /// True when this source's pending bit is latched.
    pub fn interrupt_pending(&self, index: usize) -> bool {
        self.interrupts.borrow().is_pending(index)
    }

    /// Convenience: read a byte from the data space.
    pub fn peek(&mut self, address: u32) -> u8 {
        self.bus.read_data(address)
    }

    /// Convenience: write a byte to the data space.
    pub fn poke(&mut self, address: u32, value: u8) {
        self.bus.write_data(address, value)
    }

    /// Convenience: read a block from the data space.
    pub fn peek_block(&mut self, address: u32, length: usize) -> Vec<u8> {
        (0..length as u32)
            .map(|offset| self.bus.read_data(address + offset))
            .collect()
    }

    /// Convenience: write a block to the data space.
    pub fn poke_block(&mut self, address: u32, bytes: &[u8]) {
        for (offset, byte) in bytes.iter().enumerate() {
            self.bus.write_data(address + offset as u32, *byte);
        }
    }

    /// The keyboard input filter (`0xF042`).
    pub fn key_filter(&mut self) -> u8 {
        self.bus.read_data(address::KEY_INPUT_FILTER)
    }

    /// True while the ROM's key-wait loop is armed: the observable "ready" edge.
    pub fn ready_for_key(&mut self) -> bool {
        self.key_filter() == address::KEY_INPUT_FILTER_ARMED
    }

    /// Press a key by matrix code, applying the power key's special behaviour.
    ///
    /// This is the entry point a UI should use instead of
    /// [`Keyboard::press`](crate::keyboard::Keyboard::press): the power key
    /// (`0xFF`) resets the machine rather than entering the matrix, and the
    /// reset needs the whole chipset, so it cannot live in `Keyboard` itself.
    ///
    /// Returns false for a code the model lacks.
    pub fn press_key(&mut self, code: u8) -> bool {
        self.press_key_with(code, false)
    }

    /// [`Chipset::press_key`] with the "stick" flag.
    ///
    /// `stick` latches the key down instead of holding it, which is what a
    /// right-click does.  A power key press always
    /// resets, latched or not, and only on the transition from up to down
    /// (: `pressed && !old_pressed`).
    pub fn press_key_with(&mut self, code: u8, stick: bool) -> bool {
        let power = self.keyboard.borrow().is_power_key(code);
        let was_pressed = self.keyboard.borrow().is_pressed(code);
        if !self.keyboard.borrow_mut().press_with(code, stick) {
            return false;
        }
        let now_pressed = self.keyboard.borrow().is_pressed(code);
        if power && now_pressed && !was_pressed {
            self.reset();
        }
        true
    }

    /// Whether anything visible changed since the last call, clearing the flags.
    ///
    /// This is the aggregate of every peripheral that can change what the UI
    /// draws -- currently the screen (dot matrix, indicators, mode, contrast)
    /// and the keyboard (pressed-button highlights).
    ///
    /// A renderer should call this once per frame and redraw when it is true.
    /// Both underlying flags are only ever set by writes, so a renderer that
    /// ignores this redraws too often but never misses a change.
    pub fn take_frame_request(&mut self) -> bool {
        // Both must be evaluated: `|` short-circuits nothing, and skipping the
        // keyboard would leave its flag set for a later frame to consume.
        let screen = self.screen.borrow_mut().take_dirty();
        let keyboard = self.keyboard.borrow_mut().take_dirty();
        screen | keyboard
    }
}

/// What one tick did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    /// One instruction ran.
    Executed(&'static str),
    /// The opcode was unmapped and skipped.
    Skipped,
    /// The machine is halted or stopped; no instruction ran.
    Stopped,
    /// A pre-execute hook suppressed the instruction.
    Suppressed,
}

struct ChipsetSink<'a> {
    software: &'a mut Vec<u32>,
    breaks: &'a mut u32,
}

impl ControlSink for ChipsetSink<'_> {
    fn raise_software(&mut self, index: u32) {
        self.software.push(index);
    }

    fn break_(&mut self) {
        *self.breaks += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fx991_bus::Rom;

    fn test_rom() -> Rom {
        let mut data = vec![0u8; 0x4_0000];
        data[0x0000] = 0x00;
        data[0x0001] = 0xF0; // SP = 0xF000
        data[0x0002] = 0x6A;
        data[0x0003] = 0x94; // reset entry 0x946A
        data[0x0_946A] = 0x01;
        data[0x0_946B] = 0xCE; // BC AL,0946Eh
        data[0x0_946C] = 0x8F;
        data[0x0_946D] = 0xFE; // NOP
        Rom::new(data)
    }

    fn chipset() -> Chipset {
        Chipset::new(Bus::new(test_rom()))
    }

    #[test]
    fn reset_takes_sp_from_the_word_at_rom_zero() {
        let mut machine = chipset();
        machine.reset();
        assert_eq!(machine.cpu.regs.sp, 0xF000);
        assert_eq!(machine.cpu.regs.psw(), 0);
        assert_eq!(machine.interrupts.borrow().pending_count, 1, "reset armed");
    }

    #[test]
    fn the_first_tick_dispatches_reset_and_runs_the_trampoline() {
        let mut machine = chipset();
        machine.reset();
        let outcome = machine.tick(None);
        // The reset vector's `BC AL` has already executed within this tick.
        assert_eq!(outcome, TickOutcome::Executed("OP_BC"));
        assert_eq!(machine.cpu.regs.pc, 0x946E);
    }

    #[test]
    fn the_reset_vector_is_observable_only_through_the_hook() {
        let mut machine = chipset();
        machine.reset();
        let mut seen = Vec::new();
        let outcome = machine.tick(Some(&mut |pc| {
            seen.push(pc);
            false
        }));
        assert_eq!(seen, vec![0x0_946A]);
        assert_eq!(outcome, TickOutcome::Executed("OP_BC"));
    }

    #[test]
    fn a_hook_can_suppress_the_instruction() {
        let mut machine = chipset();
        machine.reset();
        let outcome = machine.tick(Some(&mut |pc| pc == 0x0_946A));
        assert_eq!(outcome, TickOutcome::Suppressed);
        assert_eq!(machine.cpu.regs.pc, 0x946A, "PC stayed at the entry");
        assert_eq!(machine.tick(None), TickOutcome::Executed("OP_BC"));
    }

    #[test]
    fn a_timer_request_wakes_a_stopped_machine() {
        let mut machine = chipset();
        machine.reset();
        // Consume the reset request first: `AcceptInterrupt` always ends by setting
        // the run mode back to Run, so poking things before then measures the reset,
        // not the poke.
        machine.tick(None);
        machine.interrupts.borrow_mut().stop();
        assert_eq!(machine.tick(None), TickOutcome::Stopped);

        machine.interrupts.borrow_mut().mask =
            1 << (interrupts::TIMER_INTERRUPT - interrupts::MANAGED_INTERRUPT_BASE);
        {
            let mut timer = machine.timer.borrow_mut();
            timer.set_ticks_per_divide(1);
            timer.data_interval = 1;
            timer.data_control = 1;
        }
        // `data_interval = 1` compares equal only on the divider's second pass, so
        // it takes two ticks before anything is requested.
        let mut woke = false;
        for _ in 0..4 {
            if machine.tick(None) != TickOutcome::Stopped {
                woke = true;
                break;
            }
        }
        assert!(woke, "the timer never woke the machine");
        assert_eq!(machine.run_mode(), RunMode::Run);
    }

    #[test]
    fn a_stopped_machine_still_reports_its_pc_to_the_hook() {
        let mut machine = chipset();
        machine.reset();
        machine.tick(None);
        machine.interrupts.borrow_mut().stop();
        let before = machine.cpu.regs.pc;
        let mut seen = Vec::new();
        machine.tick(Some(&mut |pc| {
            seen.push(pc);
            false
        }));
        assert_eq!(seen, vec![before as u32], "a parked PC repeats");
    }

    #[test]
    fn a_standby_write_takes_effect_on_the_next_tick() {
        let mut machine = chipset();
        machine.reset();
        machine.tick(None); // consume the reset request before sleeping
        machine.poke(misc::STPACP, 0x50);
        machine.poke(misc::STPACP, 0xA0);
        machine.poke(misc::SBYCON, 0x02);
        assert_eq!(machine.tick(None), TickOutcome::Stopped);
        assert_eq!(machine.run_mode(), RunMode::Stop);
    }

    #[test]
    fn the_screen_control_registers_are_reachable_through_the_bus() {
        let mut machine = chipset();
        machine.poke(screen::CONTROL_MODE, 0x05);
        assert_eq!(machine.peek(screen::CONTROL_MODE), 0x05);
        assert_eq!(machine.screen.borrow().mode, 0x05);
    }

    #[test]
    fn the_keyboard_filter_is_reachable_through_the_bus() {
        let mut machine = chipset();
        assert_eq!(machine.key_filter(), 0x00);
        assert!(!machine.ready_for_key());
        machine.poke(address::KEY_INPUT_FILTER, 0xFF);
        assert!(machine.ready_for_key());
    }

    #[test]
    fn the_unknown_sfr_block_is_reachable_through_the_bus() {
        let mut machine = chipset();
        machine.poke(0xF033, 0x77);
        assert_eq!(machine.peek(0xF033), 0x77);
    }
}
