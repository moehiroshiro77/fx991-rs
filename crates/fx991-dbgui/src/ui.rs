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

//! The debugger window: state, input, and the frame it produces.
//!
//! [`DebuggerUi`] owns everything that is the *window's* business rather than the
//! engine's -- which panel has the cursor, how far each panel is scrolled, what
//! the last stop was -- and turns a machine plus a [`Debugger`] into a frame.
//!
//! It deliberately holds no `Emu` and no `Debugger`: both are passed in on every
//! call, because the front end owns them and the engine is borrowed mutably
//! elsewhere.  That also keeps the whole thing testable without a window.

use fx991::Emu;
use fx991_dbg::{Debugger, StopReason};

use crate::canvas::Canvas;
use crate::font::FontSet;
use crate::layout::{Layout, Panel, Rect};
use crate::panels::{self, Content, Marker, Row};
use crate::theme::{self, Metrics};

/// How a panel is scrolled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Scroll {
    /// Rows scrolled past the top.
    pub row: usize,
}

/// What the debugger wants the front end to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing; the frame was redrawn.
    None,
    /// Run until something stops the machine.
    Run,
    /// Execute one instruction.
    Step,
    /// Execute one instruction, stepping over a call.
    StepOver,
    /// Step out of the current call.
    StepOut,
    /// Leave the debugger and go back to the calculator.
    Exit,
}

/// The debugger's window state.
pub struct DebuggerUi {
    /// Which panel the keyboard is aimed at.
    focus: Panel,
    /// How many rows the memory panel is scrolled by.
    memory_scroll: usize,
    /// The address the memory panel shows.
    memory_address: u32,
    /// The address the disassembly panel's first line shows.
    ///
    /// Only consulted while [`DebuggerUi::following`] is set: otherwise the panel
    /// is anchored to the PC and this is ignored.  Stored rather than derived so
    /// that scrolling away and following the machine again both work.
    disassembly_start: u32,
    /// Whether the disassembly panel is anchored to the PC.
    ///
    /// True by default, because a debugger's listing is normally about where the
    /// machine is.  Scrolling clears it, so the view stays where the user put it
    /// instead of snapping back on the next step.
    following: bool,
    /// How the machine last stopped, for the toolbar.
    stop: Option<StopReason>,
    /// Whether a run is in progress, so the front end keeps ticking.
    running: bool,
    /// Metrics used when no font is loaded.
    metrics: Metrics,
    /// The loaded font, when there is one.
    font: Option<FontSet>,
}

impl Default for DebuggerUi {
    fn default() -> Self {
        Self::new()
    }
}

impl DebuggerUi {
    /// A debugger view with no font loaded.
    ///
    /// Without a font nothing can be drawn, but the state machine and the layout
    /// still work, which is what the tests exercise.
    pub fn new() -> Self {
        Self {
            focus: Panel::Disassembly,
            disassembly_start: 0,
            following: true,
            memory_scroll: 0,
            memory_address: 0x0_D180,
            stop: None,
            running: false,
            metrics: Metrics::default(),
            font: None,
        }
    }

    /// A debugger view that can draw, using a loaded font.
    pub fn with_font(font: FontSet) -> Self {
        let metrics = Metrics {
            cell_w: font.cell_w(),
            cell_h: font.cell_h(),
            ..Metrics::default()
        };
        Self {
            metrics,
            font: Some(font),
            ..Self::new()
        }
    }

    /// Take the font back out, for a front end closing the window.
    ///
    /// A window's GPU side goes with the window, but the font is the user's file
    /// and rasterising it is the slow part; handing it back lets a reopened
    /// window draw without reading and parsing it again.
    pub fn take_font(&mut self) -> Option<FontSet> {
        self.font.take()
    }

    /// Whether a font is loaded, i.e. whether [`DebuggerUi::render`] can draw.
    pub fn has_font(&self) -> bool {
        self.font.is_some()
    }

    /// The metrics in use.
    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// Which panel has the keyboard.
    pub fn focus(&self) -> Panel {
        self.focus
    }

    /// Set the panel the keyboard is aimed at.
    pub fn set_focus(&mut self, panel: Panel) {
        if panel.is_scrollable() {
            self.focus = panel;
        }
    }

    /// How the machine last stopped.
    pub fn stop(&self) -> Option<&StopReason> {
        self.stop.as_ref()
    }

    /// Record how the machine stopped.
    pub fn set_stop(&mut self, reason: Option<StopReason>) {
        self.running = false;
        self.stop = reason;
    }

    /// Whether a run is in progress.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// The address the memory panel is showing.
    pub fn memory_address(&self) -> u32 {
        self.memory_address
    }

    /// Point the memory panel at an address.
    pub fn goto_memory(&mut self, address: u32) {
        self.memory_address = address & 0x00FF_FFFF;
        self.memory_scroll = 0;
    }

    /// Whether the disassembly is anchored to the PC.
    pub fn is_following(&self) -> bool {
        self.following
    }

    /// Anchor the disassembly to the PC again.
    ///
    /// What "follow the machine" means, and the way back from having scrolled
    /// somewhere else.
    pub fn follow_pc(&mut self) {
        self.following = true;
    }

    /// The address the disassembly panel's first line shows.
    ///
    /// The PC's window while following, and wherever the user scrolled otherwise.
    pub fn disassembly_start(
        &mut self,
        emu: &mut Emu,
        debugger: &mut Debugger,
        rows: usize,
    ) -> u32 {
        if self.following {
            panels::disassembly_start_at_pc(debugger, emu, rows)
        } else {
            self.disassembly_start
        }
    }

    /// The layout for a frame of this size.
    pub fn layout(&self, width: u32, height: u32) -> Layout {
        Layout::new(width, height, &self.metrics)
    }

    /// Scroll a panel by whole rows, in a frame of this size.
    ///
    /// The disassembly scrolls by moving its anchor one instruction at a time
    /// rather than by a row count: instructions are 2 or 4 bytes, so an address
    /// has to be walked to stay on an instruction boundary.  Scrolling also stops
    /// it following the PC, which is what makes the view stay put while the
    /// machine keeps running.
    ///
    /// The frame size is passed in for the same reason [`DebuggerUi::click`]
    /// takes it: how far the anchor moves depends on how many rows are visible,
    /// which is a property of the frame.
    pub fn scroll(
        &mut self,
        emu: &mut Emu,
        debugger: &mut Debugger,
        width: u32,
        height: u32,
        panel: Panel,
        rows: isize,
    ) {
        match panel {
            Panel::Disassembly => {
                if rows == 0 {
                    return;
                }
                let visible = self.layout(width, height).disassembly.rows(&self.metrics);
                // The anchor has to be resolved before it can move, because while
                // following it is derived from the PC rather than stored.
                if self.following {
                    self.disassembly_start =
                        panels::disassembly_start_at_pc(debugger, emu, visible);
                    self.following = false;
                }
                for _ in 0..rows.unsigned_abs() {
                    self.disassembly_start = if rows > 0 {
                        panels::next_row(debugger, emu, self.disassembly_start)
                    } else {
                        panels::previous_row(debugger, emu, self.disassembly_start)
                            .unwrap_or(self.disassembly_start)
                    };
                }
            }
            Panel::Memory => {
                // One row is 16 bytes, so a row of scroll is 16 bytes of space.
                let next = self.memory_scroll as isize + rows;
                self.memory_scroll = next.max(0) as usize;
                self.memory_address =
                    self.memory_address.wrapping_add((rows * 16) as u32) & 0x00FF_FFFF;
            }
            _ => {}
        }
    }

    /// Handle a click at a frame position, in a frame of this size.
    ///
    /// Returns what the front end should do.  A click in the disassembly toggles
    /// a breakpoint on the row it landed on, which is the interaction a native
    /// debugger has and the reason the row arithmetic is shared with drawing.
    ///
    /// The frame size is passed in rather than remembered, because the front end
    /// owns the window and the layout must be recomputed against the size the
    /// click was actually delivered in.
    pub fn click(
        &mut self,
        emu: &mut Emu,
        debugger: &mut Debugger,
        width: u32,
        height: u32,
        x: u32,
        y: u32,
    ) -> Action {
        let layout = Layout::new(width, height, &self.metrics);
        let Some(panel) = layout.panel_at(x, y) else {
            return Action::None;
        };
        self.set_focus(panel);

        // The disassembly's rows carry addresses, so a click there sets a
        // breakpoint.  The mapping is the same one the draw used: row `n` is the
        // `n`th instruction from the window's anchor.
        if panel == Panel::Disassembly {
            let rect = layout.disassembly;
            let Some(row) = rect.row_at(&self.metrics, y) else {
                return Action::None;
            };
            if let Some(address) = self.address_at(debugger, emu, rect, row) {
                toggle_breakpoint(debugger, address);
            }
        }
        Action::None
    }

    /// The address at a content row of the disassembly window.
    ///
    /// Recomputed from the same window the drawing used rather than cached, so a
    /// click cannot land on an address that has since scrolled away.
    fn address_at(
        &mut self,
        debugger: &mut Debugger,
        emu: &mut Emu,
        rect: Rect,
        row: usize,
    ) -> Option<u32> {
        let rows = rect.rows(&self.metrics);
        let start = self.disassembly_start(emu, debugger, rows);
        debugger
            .disassemble_insns(emu, start, rows)
            .get(row)
            .map(|insn| insn.address)
    }

    /// Compose a frame for the current state.
    ///
    /// Returns `None` when no font is loaded: the panels are made of text and
    /// there is nothing meaningful to draw without one.  The front end turns that
    /// into a message rather than a blank window.
    pub fn render(
        &mut self,
        emu: &mut Emu,
        debugger: &mut Debugger,
        width: u32,
        height: u32,
    ) -> Option<fx991_ui::Frame> {
        // The font is taken out of `self` for the duration: the drawing helpers
        // need `&mut self` for the scroll state and `&mut FontSet` for the glyph
        // cache, and those are two fields of the same struct.
        let mut font = self.font.take()?;
        let layout = Layout::new(width, height, &self.metrics);
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        {
            let mut canvas = Canvas::new(&mut rgba, width, height);
            self.draw_all(&mut canvas, &mut font, emu, debugger, &layout);
        }
        self.font = Some(font);
        Some(fx991_ui::Frame {
            width,
            height,
            rgba,
        })
    }

    /// Draw every panel.
    fn draw_all(
        &mut self,
        canvas: &mut Canvas<'_>,
        font: &mut FontSet,
        emu: &mut Emu,
        debugger: &mut Debugger,
        layout: &Layout,
    ) {
        canvas.fill(canvas.bounds(), theme::BACKGROUND);

        // The toolbar first: it is the one panel whose content is a single row of
        // chrome rather than a list.
        let toolbar_row = panels::toolbar(emu, self.stop.as_ref(), debugger.is_tracing());
        self.draw_panel(
            canvas,
            font,
            layout.toolbar,
            Panel::Toolbar,
            &Content::new(vec![toolbar_row]),
        );

        let rows = layout.disassembly.rows(&self.metrics);
        let start = self.disassembly_start(emu, debugger, rows);
        let content = panels::disassembly(debugger, emu, start, rows);
        self.draw_panel(
            canvas,
            font,
            layout.disassembly,
            Panel::Disassembly,
            &content,
        );

        let rows = layout.memory.rows(&self.metrics);
        let content = panels::memory(emu, self.memory_address, rows);
        self.draw_panel(canvas, font, layout.memory, Panel::Memory, &content);

        let content = panels::registers(emu);
        self.draw_panel(canvas, font, layout.registers, Panel::Registers, &content);

        let rows = layout.stack.rows(&self.metrics);
        let content = panels::stack(debugger, emu, rows);
        self.draw_panel(canvas, font, layout.stack, Panel::Stack, &content);

        let content = panels::breakpoints(debugger);
        self.draw_panel(
            canvas,
            font,
            layout.breakpoints,
            Panel::Breakpoints,
            &content,
        );
    }

    /// Draw a panel's background, border and title.
    fn draw_chrome(
        &mut self,
        canvas: &mut Canvas<'_>,
        font: &mut FontSet,
        rect: Rect,
        panel: Panel,
    ) {
        let focused = panel == self.focus && panel.is_scrollable();
        canvas.fill(
            rect,
            if focused {
                theme::PANEL_FOCUS
            } else {
                theme::PANEL
            },
        );
        canvas.stroke(rect, theme::BORDER);
        canvas.fill(
            Rect {
                x: rect.x + 1,
                y: rect.y + 1,
                w: rect.w.saturating_sub(2),
                h: self.metrics.title_h,
            },
            theme::TITLE,
        );

        let title_rect = Rect {
            x: rect.x + self.metrics.pad,
            y: rect.y + 2,
            w: rect.w.saturating_sub(self.metrics.pad * 2),
            h: self.metrics.title_h,
        };
        font.draw_text(
            canvas,
            0,
            0,
            panel.title(),
            if focused { theme::CURRENT } else { theme::TEXT },
            title_rect,
        );
    }

    /// Draw one text panel: its chrome and then its rows.
    fn draw_panel(
        &mut self,
        canvas: &mut Canvas<'_>,
        font: &mut FontSet,
        rect: Rect,
        panel: Panel,
        content: &Content,
    ) {
        self.draw_chrome(canvas, font, rect, panel);
        let content_rect = rect.content(&self.metrics);
        let rows = rect.rows(&self.metrics);
        for (index, row) in content.rows.iter().take(rows).enumerate() {
            self.draw_row(canvas, font, content_rect, index, row);
        }
    }

    /// Draw one content row, including its gutter mark and background.
    fn draw_row(
        &mut self,
        canvas: &mut Canvas<'_>,
        font: &mut FontSet,
        content: Rect,
        row_index: usize,
        row: &Row,
    ) {
        let y = content.y + row_index as u32 * self.metrics.cell_h;

        // The gutter is two cells wide: the marker plus a space.
        let gutter = Rect {
            x: content.x,
            y,
            w: self.metrics.cell_w * 2,
            h: self.metrics.cell_h,
        };
        let text_x = content.x + self.metrics.cell_w * 2;
        let text_rect = Rect {
            x: text_x,
            y,
            w: content.w.saturating_sub(self.metrics.cell_w * 2),
            h: self.metrics.cell_h,
        };

        let (marker, marker_colour) = match row.marker {
            Marker::None => (' ', theme::TEXT_DIM),
            Marker::Current => ('>', theme::CURRENT),
            Marker::Breakpoint => ('*', theme::BREAKPOINT),
            Marker::Selected => ('-', theme::CHOSEN),
        };
        font.draw_cell(canvas, 0, 0, marker, marker_colour, gutter);

        let mut column = 0;
        for span in &row.spans {
            column = font.draw_text(canvas, column, 0, &span.text, span.colour, text_rect);
        }
    }

    /// Set the memory panel's address to wherever the PC is.
    pub fn goto_pc(&mut self, emu: &Emu) {
        self.goto_memory(emu.pc());
    }
}

/// Add a breakpoint at an address, or remove the one already there.
///
/// Toggling is what a click on a disassembly row means in a native debugger, and
/// it is why this returns nothing: the panel re-reads the list on the next frame.
fn toggle_breakpoint(debugger: &mut Debugger, address: u32) {
    let existing: Vec<_> = debugger
        .breakpoints()
        .filter(|(_, bp)| bp.address == address)
        .map(|(id, _)| id)
        .collect();
    if existing.is_empty() {
        debugger.add_breakpoint_at(address);
    } else {
        for id in existing {
            debugger.remove_breakpoint(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{MIN_HEIGHT, MIN_WIDTH};

    fn emu() -> Emu {
        let mut rom = vec![0u8; 0x4_0000];
        rom[0x0001] = 0xF0;
        rom[0x0003] = 0x90;
        let mut emu = Emu::from_rom(rom);
        {
            let mut interrupts = emu.chipset.interrupts.borrow_mut();
            interrupts.active = [false; fx991_chipset::INT_COUNT];
            interrupts.pending_count = 0;
        }
        emu.chipset.cpu.regs.csr = 1;
        emu.chipset.cpu.regs.pc = 0x0000;
        emu
    }

    #[test]
    fn the_focus_never_lands_on_the_toolbar() {
        let mut ui = DebuggerUi::new();
        assert_eq!(ui.focus(), Panel::Disassembly);
        ui.set_focus(Panel::Memory);
        assert_eq!(ui.focus(), Panel::Memory);
        // The toolbar is not a view onto data, so it cannot take the keyboard.
        ui.set_focus(Panel::Toolbar);
        assert_eq!(ui.focus(), Panel::Memory, "the toolbar kept the focus");
    }

    /// A frame size for the scroll tests: any size with room for rows works, and
    /// the row count only decides how far the PC's window reaches.
    const FRAME: (u32, u32) = (MIN_WIDTH, MIN_HEIGHT);

    #[test]
    fn the_memory_panel_moves_sixteen_bytes_a_row() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        let mut ui = DebuggerUi::new();
        let start = ui.memory_address();
        ui.scroll(&mut emu, &mut debugger, FRAME.0, FRAME.1, Panel::Memory, 1);
        assert_eq!(ui.memory_address(), start + 16);
        ui.scroll(&mut emu, &mut debugger, FRAME.0, FRAME.1, Panel::Memory, -1);
        assert_eq!(ui.memory_address(), start);
    }

    #[test]
    fn the_memory_panel_wraps_rather_than_running_off_the_top() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        let mut ui = DebuggerUi::new();
        ui.goto_memory(0);
        // Scrolling above zero wraps in the 24-bit space, which is what the
        // address arithmetic does everywhere else.
        ui.scroll(&mut emu, &mut debugger, FRAME.0, FRAME.1, Panel::Memory, -1);
        assert_eq!(ui.memory_address(), 0x00FF_FFF0);
    }

    #[test]
    fn goto_memory_masks_to_the_address_space() {
        let mut ui = DebuggerUi::new();
        ui.goto_memory(0xFFFF_FFFF);
        assert_eq!(ui.memory_address(), 0x00FF_FFFF);
    }

    #[test]
    fn a_click_on_a_disassembly_row_toggles_a_breakpoint() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        let mut ui = DebuggerUi::new();
        let layout = ui.layout(MIN_WIDTH, MIN_HEIGHT);
        let rect = layout.disassembly;
        let content = rect.content(&ui.metrics);

        // Click the row the PC is on.
        let row = rect.rows(&ui.metrics) / 2;
        let y = content.y + row as u32 * ui.metrics.cell_h + 2;
        let x = rect.x + 40;
        ui.click(&mut emu, &mut debugger, MIN_WIDTH, MIN_HEIGHT, x, y);
        assert_eq!(debugger.breakpoint_count(), 1, "a breakpoint was added");

        // Clicking the same row again removes it.
        ui.click(&mut emu, &mut debugger, MIN_WIDTH, MIN_HEIGHT, x, y);
        assert_eq!(debugger.breakpoint_count(), 0, "the breakpoint was removed");
    }

    #[test]
    fn a_click_on_the_toolbar_sets_no_breakpoint() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        let mut ui = DebuggerUi::new();
        ui.click(&mut emu, &mut debugger, MIN_WIDTH, MIN_HEIGHT, 40, 5);
        assert_eq!(debugger.breakpoint_count(), 0);
    }

    #[test]
    fn a_click_outside_every_panel_is_ignored() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        let mut ui = DebuggerUi::new();
        let layout = ui.layout(MIN_WIDTH, MIN_HEIGHT);
        // The gap between the columns.
        let x = layout.disassembly.w + 1;
        let y = layout.disassembly.y + 10;
        ui.click(&mut emu, &mut debugger, MIN_WIDTH, MIN_HEIGHT, x, y);
        assert_eq!(debugger.breakpoint_count(), 0);
    }

    #[test]
    fn the_stop_reason_is_remembered() {
        let mut ui = DebuggerUi::new();
        assert!(ui.stop().is_none());
        ui.set_stop(Some(StopReason::Paused));
        assert_eq!(ui.stop(), Some(&StopReason::Paused));
        assert!(!ui.is_running());
    }

    #[test]
    fn rendering_without_a_font_produces_no_frame() {
        let mut emu = emu();
        let mut debugger = Debugger::new();
        let mut ui = DebuggerUi::new();
        assert!(!ui.has_font());
        assert!(
            ui.render(&mut emu, &mut debugger, MIN_WIDTH, MIN_HEIGHT)
                .is_none(),
            "text panels need a font"
        );
    }
}
