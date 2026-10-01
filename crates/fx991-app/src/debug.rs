// SPDX-License-Identifier: GPL-3.0-only
//
// Copyright (C) 2026 fx991-rs contributors
//
// This program is free software: you can redistribute it and/or modify it under
// the terms of the GNU General Public License as published by the Free Software
// Foundation, version 3.  It is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY
// or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU General Public License in
// LICENSE for more details.

//! The debugger window's driver: run control, keys, and the mouse.
//!
//! This is the layer between the engine and the event loop.  It owns the
//! [`Debugger`] and the panel view, and it decides what a keystroke or a click
//! means; the event loop only forwards events and asks for a frame.
//!
//! Everything here works on an `Emu` and a [`Debugger`] rather than on a window,
//! so the whole run-control state machine is testable without a GPU.

use fx991::Emu;
use fx991_dbg::{Debugger, StopReason};
use fx991_dbgui::{DebuggerUi, Panel};

/// How many ticks one slice of a run executes before the caller regains control.
///
/// A run is a blocking loop with no way to interrupt it, so the only way to stay
/// responsive is to run in slices.  This is the slice: about a fifth of a second
/// of guest time at 2 Mi instructions/s, short enough that a keystroke is
/// answered promptly and long enough that the per-slice overhead disappears.
pub const RUN_SLICE: u64 = 400_000;

/// A slice for a machine that is parked.
///
/// The ROM spends most of its life in STOP waiting for a timer, and a parked
/// slice executes nothing and changes nothing a panel shows.  Running it in
/// bigger slices costs nothing and makes "let it settle" feel instant.
pub const PARKED_SLICE: u64 = 4_000_000;

/// Whether the debugger is running the machine or holding it stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// Holding the machine, showing its state.
    Paused,
    /// Running until something stops it.
    Running,
}

/// What a key press means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Run until something stops the machine.
    Run,
    /// Pause a run.
    Pause,
    /// Execute one instruction.
    Step,
    /// Execute one instruction, stepping over a call.
    StepOver,
    /// Step out of the current call.
    StepOut,
    /// Toggle a breakpoint at the current PC.
    ToggleBreakpoint,
    /// Point the memory panel at the PC.
    FollowPc,
    /// Move the focus to the next panel.
    NextPanel,
    /// Scroll the focused panel.
    Scroll(isize),
    /// Leave the debugger.
    Leave,
}

/// The debugger window's driver.
pub struct DebugSession {
    /// The engine.
    pub debugger: Debugger,
    /// The panels.
    pub ui: DebuggerUi,
    /// Whether the machine is being run or held.
    state: RunState,
    /// The reason the last run stopped.
    stop: Option<StopReason>,
}

impl DebugSession {
    /// A session with a font loaded, ready to draw.
    pub fn new(ui: DebuggerUi) -> Self {
        Self {
            debugger: Debugger::new(),
            ui,
            state: RunState::Paused,
            stop: None,
        }
    }

    /// Whether the machine is running.
    pub fn state(&self) -> RunState {
        self.state
    }

    /// The reason the last run stopped.
    ///
    /// `None` until the first run or step: a session that has just been entered
    /// has nothing to report, which is different from "it stopped for no reason".
    pub fn stop(&self) -> Option<&StopReason> {
        self.stop.as_ref()
    }

    /// The panel that has the keyboard.
    pub fn focus(&self) -> Panel {
        self.ui.focus()
    }

    /// Take the font back out, for a front end closing the window.
    pub fn take_font(&mut self) -> fx991_dbgui::FontSet {
        self.ui
            .take_font()
            .expect("a session is only built with a font")
    }

    /// Handle a key press, in a frame of `width` x `height`.
    ///
    /// The frame size is needed because scrolling the disassembly moves its
    /// anchor by whole instructions, and how far one step of scroll goes depends
    /// on how many rows are on screen.
    pub fn key(&mut self, emu: &mut Emu, width: u32, height: u32, key: Key) {
        match key {
            Key::Run => self.start_run(),
            Key::Pause => {
                self.state = RunState::Paused;
                self.stop = Some(StopReason::Paused);
            }
            Key::Step => self.step(emu, Step::Into),
            Key::StepOver => self.step(emu, Step::Over),
            Key::StepOut => self.step(emu, Step::Out),
            Key::ToggleBreakpoint => {
                let pc = emu.pc();
                self.toggle_breakpoint(pc);
            }
            Key::FollowPc => {
                self.ui.goto_pc(emu);
                // Both halves of "follow the machine": the memory panel points at
                // it, and the disassembly re-anchors to it.
                self.ui.follow_pc();
                self.ui.set_focus(Panel::Disassembly);
            }
            Key::NextPanel => self.cycle_focus(),
            Key::Scroll(rows) => {
                let panel = self.ui.focus();
                self.ui
                    .scroll(emu, &mut self.debugger, width, height, panel, rows);
            }
            Key::Leave => {
                self.state = RunState::Paused;
            }
        }
    }

    /// Begin running.
    fn start_run(&mut self) {
        self.state = RunState::Running;
        self.stop = None;
    }

    /// Execute one instruction, and hold the machine.
    fn step(&mut self, emu: &mut Emu, how: Step) {
        let reason = match how {
            Step::Into => self.debugger.step(emu),
            // A budget: `step_over` runs to a return address, which on a machine
            // parked in STOP would never be reached, so it must be bounded.
            Step::Over => self.debugger.step_over(emu, PARKED_SLICE),
            Step::Out => self.debugger.step_out(emu, PARKED_SLICE),
        };
        self.state = RunState::Paused;
        self.stop = Some(reason);
    }

    /// Run one slice of the current run, if one is in progress.
    ///
    /// Returns whether the run is still going, so the caller knows whether to
    /// keep ticking or go back to waiting for an event.
    pub fn run_slice(&mut self, emu: &mut Emu) -> bool {
        if self.state != RunState::Running {
            return false;
        }
        // A parked machine is waited out in bigger slices: nothing executes, so
        // the only thing a smaller slice would buy is more wakeups.
        let slice = if emu.chipset.has_slept() && self.debugger.is_stopped(emu) {
            PARKED_SLICE
        } else {
            RUN_SLICE
        };
        let reason = self.debugger.run(emu, slice);
        self.stop = Some(reason);

        // `Budget` means the slice simply ended with the machine still running,
        // which is the ordinary case for a long run.  `Stopped` means the ROM
        // parked; that is also not an end, because a timer will wake it.  Every
        // other reason is a real stop.
        match self.stop {
            Some(StopReason::Budget { .. }) | Some(StopReason::Stopped) => true,
            _ => {
                self.state = RunState::Paused;
                false
            }
        }
    }

    /// Add a breakpoint at an address, or remove the one already there.
    pub fn toggle_breakpoint(&mut self, address: u32) {
        let existing: Vec<_> = self
            .debugger
            .breakpoints()
            .filter(|(_, bp)| bp.address == address)
            .map(|(id, _)| id)
            .collect();
        if existing.is_empty() {
            self.debugger.add_breakpoint_at(address);
        } else {
            for id in existing {
                self.debugger.remove_breakpoint(id);
            }
        }
    }

    /// Move the keyboard focus to the next scrollable panel.
    fn cycle_focus(&mut self) {
        let panels: Vec<Panel> = Panel::ALL
            .iter()
            .copied()
            .filter(|panel| panel.is_scrollable())
            .collect();
        let current = panels
            .iter()
            .position(|panel| *panel == self.ui.focus())
            .unwrap_or(0);
        let next = panels[(current + 1) % panels.len()];
        self.ui.set_focus(next);
    }

    /// Handle a click, in a frame of this size.
    pub fn click(&mut self, emu: &mut Emu, width: u32, height: u32, x: u32, y: u32) {
        self.ui.click(emu, &mut self.debugger, width, height, x, y);
    }

    /// Compose the frame for this session.
    pub fn render(&mut self, emu: &mut Emu, width: u32, height: u32) -> Option<fx991_ui::Frame> {
        self.ui.set_stop(self.stop.clone());
        self.ui.render(emu, &mut self.debugger, width, height)
    }

    /// A one-line status for the window title.
    ///
    /// Carries the step count and the focused panel as well as the run state:
    /// the title bar is the only place outside the frame that can say what the
    /// keyboard is aimed at, and `F4` changes that silently otherwise.
    pub fn status(&self) -> String {
        let state = match self.state {
            RunState::Running => "running",
            RunState::Paused => "paused",
        };
        let stop = match &self.stop {
            Some(reason) => format!(" -- {}", fx991_dbgui::stop_text(reason)),
            None => String::new(),
        };
        format!("{state}{stop} -- {}", self.focus().title())
    }
}

/// Which stepping command to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Into,
    Over,
    Out,
}

#[cfg(test)]
mod tests {
    use super::*;
    use fx991_dbg::BreakpointId;
    use fx991_dbgui::{MIN_HEIGHT, MIN_WIDTH};

    fn emu() -> Emu {
        let mut rom = vec![0u8; 0x4_0000];
        rom[0x0000] = 0x00;
        rom[0x0001] = 0xF0;
        rom[0x0002] = 0x00;
        rom[0x0003] = 0x90;
        // A few instructions at the entry so stepping has somewhere to go.
        rom[0x1_0000] = 0x00; // MOV R0, #0x11
        rom[0x1_0001] = 0x11;
        rom[0x1_0002] = 0x10; // MOV R1, #0x22
        rom[0x1_0003] = 0x22;
        let mut emu = Emu::from_rom(rom);
        // Take the tick that dispatches the reset, so the program at the entry is
        // the whole world.  With nothing armed `Emu::step` goes straight to the
        // chipset, which is the same path the calculator front end uses.
        emu.step();
        emu.chipset.cpu.regs.csr = 1;
        emu.chipset.cpu.regs.pc = 0x0000;
        emu
    }

    fn session() -> DebugSession {
        DebugSession::new(DebuggerUi::new())
    }

    /// Press a key at a frame size the panels have room in.
    ///
    /// The size only matters for scrolling the disassembly, which moves its
    /// anchor by whole instructions; the rest of the state machine ignores it.
    fn press(session: &mut DebugSession, emu: &mut Emu, key: Key) {
        session.key(emu, MIN_WIDTH, MIN_HEIGHT, key);
    }

    #[test]
    fn a_new_session_is_paused_and_has_done_nothing() {
        let session = session();
        assert_eq!(session.state(), RunState::Paused);
        assert!(session.stop().is_none());
        assert_eq!(session.focus(), Panel::Disassembly);
    }

    #[test]
    fn stepping_advances_one_instruction_and_holds_the_machine() {
        let mut emu = emu();
        let mut session = session();
        let before = emu.pc();
        press(&mut session, &mut emu, Key::Step);
        assert_ne!(emu.pc(), before, "the machine moved");
        assert_eq!(session.state(), RunState::Paused, "and is held");
        assert!(session.stop().is_some());
    }

    #[test]
    fn stepping_three_times_runs_three_instructions() {
        let mut emu = emu();
        let mut session = session();
        for _ in 0..3 {
            press(&mut session, &mut emu, Key::Step);
        }
        assert_eq!(emu.pc(), 0x1_0006, "three two-byte instructions");
    }

    #[test]
    fn a_breakpoint_stops_a_run_and_holds_the_machine() {
        let mut emu = emu();
        let mut session = session();
        // Break at the third instruction.
        session.toggle_breakpoint(0x1_0004);
        press(&mut session, &mut emu, Key::Run);
        assert_eq!(session.state(), RunState::Running);

        // Drive slices until the run ends.
        let mut guard = 0;
        while session.run_slice(&mut emu) {
            guard += 1;
            assert!(guard < 100, "the run never stopped");
        }
        assert_eq!(session.state(), RunState::Paused, "the run ended");
        assert_eq!(emu.pc(), 0x1_0004, "it stopped at the breakpoint");
        assert!(
            matches!(session.stop(), Some(StopReason::Breakpoint { .. })),
            "reported as a breakpoint: {:?}",
            session.stop()
        );
    }

    #[test]
    fn a_run_with_no_breakpoints_keeps_going() {
        let mut emu = emu();
        let mut session = session();
        press(&mut session, &mut emu, Key::Run);
        // One slice is a budget, not a stop: the machine is still running.
        assert!(session.run_slice(&mut emu), "the run continues");
        assert_eq!(session.state(), RunState::Running);
    }

    #[test]
    fn pausing_ends_a_run() {
        let mut emu = emu();
        let mut session = session();
        press(&mut session, &mut emu, Key::Run);
        assert!(session.run_slice(&mut emu));
        press(&mut session, &mut emu, Key::Pause);
        assert_eq!(session.state(), RunState::Paused);
        assert_eq!(session.stop(), Some(&StopReason::Paused));
        // And a slice does nothing once paused.
        assert!(!session.run_slice(&mut emu));
    }

    #[test]
    fn toggling_a_breakpoint_twice_leaves_none() {
        let mut session = session();
        session.toggle_breakpoint(0x1_0002);
        assert_eq!(session.debugger.breakpoint_count(), 1);
        session.toggle_breakpoint(0x1_0002);
        assert_eq!(session.debugger.breakpoint_count(), 0);
    }

    #[test]
    fn the_breakpoint_key_uses_the_current_pc() {
        let mut emu = emu();
        let mut session = session();
        press(&mut session, &mut emu, Key::ToggleBreakpoint);
        let armed: Vec<u32> = session
            .debugger
            .breakpoints()
            .map(|(_, bp)| bp.address)
            .collect();
        assert_eq!(armed, vec![emu.pc()], "the breakpoint is at the PC");
    }

    #[test]
    fn following_the_pc_re_anchors_both_panels_that_track_it() {
        let mut emu = emu();
        let mut session = session();

        // Scroll the disassembly away first, so following has something to undo.
        press(&mut session, &mut emu, Key::Scroll(3));
        assert!(!session.ui.is_following(), "scrolling left the PC");

        press(&mut session, &mut emu, Key::FollowPc);
        assert_eq!(session.ui.memory_address(), emu.pc(), "the dump follows it");
        assert!(session.ui.is_following(), "and the listing re-anchors");
        // The listing is what "follow the machine" is about, so that is where the
        // keyboard goes.
        assert_eq!(session.focus(), Panel::Disassembly);
    }

    #[test]
    fn cycling_the_focus_visits_every_scrollable_panel_and_never_the_toolbar() {
        let mut emu = emu();
        let mut session = session();
        let mut seen = Vec::new();
        for _ in 0..Panel::ALL.len() {
            press(&mut session, &mut emu, Key::NextPanel);
            seen.push(session.focus());
        }
        assert!(
            !seen.contains(&Panel::Toolbar),
            "the toolbar is not a view onto data: {seen:?}"
        );
        // Every scrollable panel was visited.
        for panel in Panel::ALL.iter().filter(|p| p.is_scrollable()) {
            assert!(seen.contains(panel), "{panel:?} was never focused");
        }
    }

    #[test]
    fn scrolling_moves_the_focused_panel() {
        let mut emu = emu();
        let mut session = session();

        // The memory dump scrolls by whole 16-byte rows.
        session.ui.set_focus(Panel::Memory);
        let before = session.ui.memory_address();
        press(&mut session, &mut emu, Key::Scroll(2));
        assert_eq!(session.ui.memory_address(), before + 32);

        // The disassembly scrolls by whole instructions instead, and stops
        // following the machine when it does.
        session.ui.set_focus(Panel::Disassembly);
        let rows = session
            .ui
            .layout(MIN_WIDTH, MIN_HEIGHT)
            .disassembly
            .rows(session.ui.metrics());
        let start = session
            .ui
            .disassembly_start(&mut emu, &mut session.debugger, rows);
        press(&mut session, &mut emu, Key::Scroll(1));
        assert!(!session.ui.is_following(), "it stopped following");
        let moved = session
            .ui
            .disassembly_start(&mut emu, &mut session.debugger, rows);
        assert_ne!(moved, start, "the listing moved");
    }

    #[test]
    fn the_status_line_reports_the_state_and_the_stop() {
        let mut emu = emu();
        let mut session = session();
        assert_eq!(session.status(), "paused -- Disassembly");

        session.toggle_breakpoint(0x1_0004);
        press(&mut session, &mut emu, Key::Run);
        assert!(
            session.status().starts_with("running"),
            "{}",
            session.status()
        );

        let mut guard = 0;
        while session.run_slice(&mut emu) {
            guard += 1;
            assert!(guard < 100);
        }
        let status = session.status();
        assert!(status.starts_with("paused -- breakpoint"), "{status}");
    }

    #[test]
    fn rendering_without_a_font_yields_no_frame() {
        let mut emu = emu();
        let mut session = session();
        assert!(session.render(&mut emu, MIN_WIDTH, MIN_HEIGHT).is_none());
    }

    #[test]
    fn a_click_toggles_a_breakpoint_through_the_session() {
        let mut emu = emu();
        let mut session = session();
        let layout = session.ui.layout(MIN_WIDTH, MIN_HEIGHT);
        let rect = layout.disassembly;
        let content = rect.content(session.ui.metrics());
        let row = rect.rows(session.ui.metrics()) / 2;
        let y = content.y + row as u32 * session.ui.metrics().cell_h + 2;
        session.click(&mut emu, MIN_WIDTH, MIN_HEIGHT, rect.x + 40, y);
        assert_eq!(session.debugger.breakpoint_count(), 1);
    }

    #[test]
    fn leaving_holds_the_machine() {
        let mut emu = emu();
        let mut session = session();
        press(&mut session, &mut emu, Key::Run);
        press(&mut session, &mut emu, Key::Leave);
        assert_eq!(session.state(), RunState::Paused);
    }

    #[test]
    fn a_step_over_a_call_does_not_run_into_the_callee() {
        // `step_over` puts a temporary breakpoint on the return address, so a
        // `BL` is crossed in one step.  With no call in the program this is just
        // a step, which is what the assertion below pins.
        let mut emu = emu();
        let mut session = session();
        press(&mut session, &mut emu, Key::StepOver);
        assert_eq!(emu.pc(), 0x1_0002);
        assert_eq!(session.state(), RunState::Paused);
    }

    #[test]
    fn a_breakpoint_id_is_reported_by_the_stop_reason() {
        let mut emu = emu();
        let mut session = session();
        let id = session.debugger.add_breakpoint_at(0x1_0002);
        press(&mut session, &mut emu, Key::Run);
        let mut guard = 0;
        while session.run_slice(&mut emu) {
            guard += 1;
            assert!(guard < 100);
        }
        match session.stop() {
            Some(StopReason::Breakpoint {
                id: reported,
                address,
            }) => {
                assert_eq!(*reported, id);
                assert_eq!(*address, 0x1_0002);
            }
            other => panic!("expected a breakpoint stop, got {other:?}"),
        }
        let _ = BreakpointId(0);
    }
}
