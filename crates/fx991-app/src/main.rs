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

//! Two wgpu windows onto one calculator.
//!
//! The calculator is a fixed-size picture of a fixed-size object, with clickable
//! keys: left-click presses one, right-click latches it down (so you can hold
//! `SHIFT` and then click something else), and clicking `ON` power-cycles it.
//! `+`/`-` zoom it and `Esc` quits.
//!
//! `F12` opens a second window with the debugger's panels.  It is a second window
//! rather than a mode on the first because the ROM is debugged *through* its
//! display: a breakpoint on a menu handler is not much use if the menu cannot be
//! seen, and a menu cannot be driven if the keys cannot be clicked.  The two sit
//! side by side and share one machine.
//!
//! # One device, two surfaces
//!
//! Both windows are the same picture-making machine: the face is composed on the
//! CPU by `fx991-ui` and the panels by `fx991-dbgui`, and each is uploaded as one
//! texture and drawn as a single textured triangle.  So the expensive half -- the
//! instance, the adapter, the device, the sampler and the bind-group layout --
//! lives in [`gpu::GpuDevice`] and is opened once, and each window holds only what
//! is its own: a surface, a swapchain, and a pipeline for that swapchain's format.
//!
//! Two details of the pipeline matter:
//!
//! * the frame texture is `Rgba8Unorm`, **not** `Rgba8UnormSrgb`: compositing
//!   already happened in sRGB bytes on the CPU, and an sRGB texture would
//!   linearise them on read;
//! * the sampler is `Nearest`, because the display is a dot matrix and
//!   interpolating it makes the edges mush.
//!
//! The surface keeps the sRGB format the adapter prefers, and that pairs with the
//! linear texture rather than clashing with it -- [`gpu`] explains why, and why
//! changing it darkens both windows.
//!
//! Everything testable without a GPU lives in [`input`], [`debug`], and in
//! `fx991-ui`; this file is the glue.

mod debug;
mod gpu;
mod input;
mod window;
mod zoom;

use std::sync::Arc;
use std::time::Instant;

use fx991::throttle::Throttle;
use fx991::Emu;
use fx991_dbgui::{DebuggerUi, FontSet};
use fx991_ui::{Renderer, NATURAL_HEIGHT, NATURAL_WIDTH};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton as WinitButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowButtons, WindowId};

use debug::{DebugSession, Key as DebugKey};
use gpu::{Gpu, GpuDevice, SurfaceProblem};
use input::{Action, Mouse, MouseButton};
use window::block_maximise;
use zoom::{
    clamp_quarters, fit_quarters, max_quarters_for_texture_limit, window_size_for, MAX_QUARTERS,
    MIN_QUARTERS, QUARTERS_PER_UNIT,
};

/// Default data paths, overridable from the command line.
///
/// Nothing is bundled: the ROM image and the face texture are files the user
/// supplies, so the binary carries no copyrighted content.
const DEFAULT_ROM: &str = "data/rom_verF.bin";
const DEFAULT_SKIN: &str = "data/skin.rgba";
const DEFAULT_FONT: &str = "data/font.ttf";

/// What the command line asked for.
struct Options {
    rom: std::path::PathBuf,
    skin: std::path::PathBuf,
    /// The debugger's font, or `None` to use the default path.
    font: Option<std::path::PathBuf>,
    /// The zoom in quarter steps, or `None` to fit the screen.
    quarters: Option<u32>,
}

const USAGE: &str = "fx991cnx -- a clickable fx-991CN X

usage: fx991cnx [ZOOM] [--rom PATH] [--skin PATH] [--font PATH]

  ZOOM        0.5 to 8 (default: fit the screen)
  --rom PATH  ROM image           (default: data/rom_verF.bin)
  --skin PATH face texture, RGBA  (default: data/skin.rgba)
  --font PATH debugger font, TTF  (default: data/font.ttf, monospace)

Keys: left-click presses, right-click latches, +/- zoom, F12 opens the debugger,
Esc quits (or closes the debugger window when it is focused).";

fn parse_args() -> Result<Options, String> {
    let mut options = Options {
        rom: DEFAULT_ROM.into(),
        skin: DEFAULT_SKIN.into(),
        font: None,
        quarters: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--rom" => options.rom = args.next().ok_or("--rom needs a path")?.into(),
            "--skin" => options.skin = args.next().ok_or("--skin needs a path")?.into(),
            "--font" => options.font = Some(args.next().ok_or("--font needs a path")?.into()),
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => {
                let scale: f64 = other
                    .parse()
                    .map_err(|_| format!("{other:?} is not a zoom factor"))?;
                if !(0.5..=8.0).contains(&scale) {
                    return Err(format!("zoom {scale} is out of range (0.5 to 8)"));
                }
                options.quarters = Some((scale * QUARTERS_PER_UNIT as f64).round() as u32);
            }
        }
    }
    Ok(options)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = parse_args()?;
    let rom = std::fs::read(&options.rom)
        .map_err(|err| format!("could not read {}: {err}", options.rom.display()))?;
    let skin = fx991_ui::Skin::from_file(&options.skin)
        .map_err(|err| format!("could not load {}: {err}", options.skin.display()))?;

    // The debugger's font is optional: without it the calculator runs and only
    // `F12` is unavailable, which is better than refusing to start.
    let font_path = options.font.clone().unwrap_or_else(|| DEFAULT_FONT.into());
    let font = match FontSet::from_file(&font_path) {
        Ok(font) => Some(font),
        Err(err) => {
            println!(
                "note: no debugger font at {} ({err}); F12 will report it",
                font_path.display()
            );
            None
        }
    };

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(rom, skin, font, options.quarters);
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// The calculator plus its pacing and input state.
struct Machine {
    emulator: Emu,
    renderer: Renderer,
    throttle: Throttle,
    mouse: Mouse,
    /// The zoom, in quarter steps: `4` is 1x, `5` is 1.25x, `8` is 2x.
    ///
    /// An integer rather than a float so repeated `+`/`-` cannot drift, and
    /// quarter steps because whole steps are too coarse -- 1x to 2x doubles the
    /// window, which is a jarring jump when you are nudging the size.
    quarters: u32,
    /// The largest zoom this machine may use, set from the GPU's texture limit.
    max_quarters: u32,
}

impl Machine {
    fn new(rom: Vec<u8>, renderer: Renderer, quarters: u32, max_quarters: u32) -> Self {
        let max_quarters = max_quarters.clamp(MIN_QUARTERS, MAX_QUARTERS);
        let quarters = quarters.clamp(MIN_QUARTERS, max_quarters);
        let (w, h) = window_size_for(quarters);
        Self {
            emulator: Emu::from_rom(rom),
            renderer,
            throttle: Throttle::default(),
            mouse: Mouse::new(w, h),
            quarters,
            max_quarters,
        }
    }

    /// Boot to the key-wait loop, so the first frame is the real face rather
    /// than the reset state.
    fn boot(&mut self) {
        // Bounded: a bug here must not hang the window before it appears.
        for _ in 0..20_000_000 {
            if self.emulator.chipset.ready_for_key() {
                return;
            }
            self.emulator.step();
        }
        eprintln!("warning: the ROM never reached its key-wait loop");
    }

    /// Run one batch of cycles.
    fn run_batch(&mut self) {
        self.throttle.run_batch(|| {
            self.emulator.step();
            true
        });
    }

    /// Change the zoom by whole quarter steps, clamped to the allowed range.
    ///
    /// The ceiling is `self.max_quarters`, not the global constant: going past
    /// what the GPU can allocate would panic inside `Surface:configure` rather
    /// than just drawing something wrong.
    fn nudge_scale(&mut self, steps: i32) {
        let next = (self.quarters as i32 + steps).max(0) as u32;
        self.quarters = next.clamp(MIN_QUARTERS, self.max_quarters);
        let (w, h) = self.window_size();
        self.mouse.set_client_size(w, h);
    }

    /// The zoom as a multiple of the skin size, for display.
    fn scale(&self) -> f64 {
        self.quarters as f64 / QUARTERS_PER_UNIT as f64
    }

    /// The window size for the current zoom.
    fn window_size(&self) -> (u32, u32) {
        window_size_for(self.quarters)
    }
}

/// The calculator window: the face, its input, and the machine behind it.
struct CalculatorWindow {
    window: Arc<Window>,
    gpu: Gpu,
    machine: Machine,
    /// Last cursor position, in this window's pixels.
    cursor: (f64, f64),
    /// The frame size this window's texture currently holds.
    frame_size: (u32, u32),
}

/// The debugger window: the panels, their input, and the session.
///
/// A second window rather than a mode on the first, so the calculator stays
/// visible and clickable while a breakpoint is being set.  Debugging this ROM
/// means watching that display -- a menu, a dialog, a value -- and a debugger
/// that hides it sends the user back to the other window to look, which is the
/// thing the second window exists to avoid.
struct DebugWindow {
    window: Arc<Window>,
    gpu: Gpu,
    session: DebugSession,
    /// Last cursor position, in this window's pixels.
    cursor: (f64, f64),
    /// The frame size this window's texture currently holds.
    frame_size: (u32, u32),
}

/// The winit application.
struct App {
    rom: Vec<u8>,
    skin: fx991_ui::Skin,
    /// The debugger's font, or `None` when the user has not supplied one.
    ///
    /// Held rather than consumed: the debugger window is opened on request, and
    /// a second `F12` after closing it needs the font again.
    font: Option<FontSet>,
    /// `None` means "fit the screen"; `Some` is an explicit user choice.
    /// In quarter steps.
    quarters: Option<u32>,
    /// The GPU resources both windows share, opened with the first one.
    device: Option<GpuDevice>,
    /// The calculator, once the event loop has resumed.
    calculator: Option<CalculatorWindow>,
    /// The debugger, once it has been opened.
    ///
    /// Kept across closing and reopening so breakpoints, scroll positions and
    /// snapshots are not thrown away by a look at the calculator.
    debug: Option<DebugWindow>,
}

impl App {
    fn new(
        rom: Vec<u8>,
        skin: fx991_ui::Skin,
        font: Option<FontSet>,
        quarters: Option<u32>,
    ) -> Self {
        Self {
            rom,
            skin,
            font,
            quarters,
            device: None,
            calculator: None,
            debug: None,
        }
    }

    /// Open the debugger window, or bring it to the front if it is already up.
    ///
    /// The calculator window is untouched: the two sit side by side, which is the
    /// point -- a breakpoint on a menu handler is not much use if the menu cannot
    /// be seen and clicked.
    fn open_debugger(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(debug) = self.debug.as_ref() {
            debug.window.focus_window();
            return;
        }
        let Some(device) = self.device.as_ref() else {
            return;
        };
        // The font is loaded once, at startup; without one the panels cannot be
        // drawn, and saying so is better than a blank window.
        let Some(font) = self.font.take() else {
            eprintln!(
                "the debugger needs a monospace font;                  pass --font PATH or put one at {DEFAULT_FONT}"
            );
            return;
        };

        let attributes = Window::default_attributes()
            .with_title("fx-991CN X -- debugger")
            .with_inner_size(winit::dpi::PhysicalSize::new(DEBUG_WIDTH, DEBUG_HEIGHT));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                eprintln!("could not open the debugger window: {err}");
                self.font = Some(font);
                return;
            }
        };
        let gpu = match device.view(&window) {
            Ok(gpu) => gpu,
            Err(err) => {
                eprintln!("could not open the debugger's GPU side: {err}");
                self.font = Some(font);
                return;
            }
        };
        // Clamp to what the device can hold: the frame is composed at the
        // window's size and uploaded as one texture.
        let (w, h) = clamp_to_texture(DEBUG_WIDTH, DEBUG_HEIGHT, device);
        let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(w, h));

        println!("debugger opened -- F7/F8 step, F9 run, F2 breakpoint, Esc closes");
        self.debug = Some(DebugWindow {
            window,
            gpu,
            session: DebugSession::new(DebuggerUi::with_font(font)),
            cursor: (0.0, 0.0),
            frame_size: (fx991_dbgui::MIN_WIDTH, fx991_dbgui::MIN_HEIGHT),
        });
    }

    /// Close the debugger window, keeping its session.
    ///
    /// The session outlives the window so breakpoints and scroll positions
    /// survive a look away, which is why only the window and its GPU side go.
    fn close_debugger(&mut self) {
        if let Some(mut debug) = self.debug.take() {
            // The font goes back so a reopened window can draw.
            self.font = Some(debug.session.take_font());
            println!("debugger closed");
        }
    }

    /// Compose and present the calculator window's frame.
    fn redraw_calculator(&mut self, event_loop: &ActiveEventLoop) {
        let Some(calc) = self.calculator.as_mut() else {
            return;
        };
        let frame = calc
            .machine
            .renderer
            .render(&mut calc.machine.emulator.chipset);
        present(
            event_loop,
            &calc.window,
            &mut calc.gpu,
            &mut calc.frame_size,
            &frame,
        );
    }

    /// Compose and present the debugger window's frame.
    fn redraw_debugger(&mut self, event_loop: &ActiveEventLoop) {
        let Some(device) = self.device.as_ref() else {
            return;
        };
        let Some(calc) = self.calculator.as_mut() else {
            return;
        };
        let Some(debug) = self.debug.as_mut() else {
            return;
        };
        let size = debug.window.inner_size();
        let (width, height) = clamp_to_texture(size.width, size.height, device);
        let Some(frame) = debug
            .session
            .render(&mut calc.machine.emulator, width, height)
        else {
            // No font, which `open_debugger` already reported.
            return;
        };
        present(
            event_loop,
            &debug.window,
            &mut debug.gpu,
            &mut debug.frame_size,
            &frame,
        );
    }
}

/// Upload a frame to a window and present it.
///
/// Shared by both windows: they differ in what they compose, not in how it gets
/// to the screen.
fn present(
    event_loop: &ActiveEventLoop,
    window: &Arc<Window>,
    gpu: &mut Gpu,
    frame_size: &mut (u32, u32),
    frame: &fx991_ui::Frame,
) {
    if (frame.width, frame.height) != *frame_size {
        gpu.resize_texture(frame.width, frame.height);
        *frame_size = (frame.width, frame.height);
    }
    match gpu.draw(frame) {
        // Presented, or a surface state that only means "skip this frame".
        Ok(None) => {}
        Ok(Some(problem)) => match problem {
            // A lost or outdated surface needs reconfiguring before the next
            // frame; a minimised window or a slow present needs nothing.
            SurfaceProblem::Lost | SurfaceProblem::Outdated => {
                let size = window.inner_size();
                gpu.resize_surface(size.width, size.height);
            }
            SurfaceProblem::Timeout | SurfaceProblem::Occluded => {}
        },
        Err(err) => {
            eprintln!("draw failed: {err}");
            event_loop.exit();
        }
    }
    if gpu.take_reconfigure_request() {
        let size = window.inner_size();
        gpu.resize_surface(size.width, size.height);
    }
}

/// The size the debugger opens at.
const DEBUG_WIDTH: u32 = 1280;
const DEBUG_HEIGHT: u32 = 800;

/// Clamp a requested size to what the GPU can hold and the layout can use.
///
/// The frame is composed at the window's size and uploaded as one texture, so a
/// window larger than `max_texture_dimension_2d` would fail validation inside
/// `create_texture` -- a crash rather than a graceful failure.  The minimum comes
/// from the layout, which would otherwise collapse its panels.
fn clamp_to_texture(width: u32, height: u32, device: &GpuDevice) -> (u32, u32) {
    let max = device.max_texture_dimension;
    (
        width.clamp(fx991_dbgui::MIN_WIDTH, max),
        height.clamp(fx991_dbgui::MIN_HEIGHT, max),
    )
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.calculator.is_some() {
            return;
        }
        // Pick a zoom that fits the screen unless the user asked for one.
        let wanted = match self.quarters {
            Some(q) => clamp_quarters(q),
            None => event_loop
                .primary_monitor()
                .map(|monitor| {
                    let size = monitor.size();
                    fit_quarters(size.width, size.height)
                })
                .unwrap_or(QUARTERS_PER_UNIT),
        };
        // Provisional size; the real one is settled once the GPU has told us its
        // texture limit, which can be lower than the screen allows.
        let (w, h) = window_size_for(wanted);

        // A fixed window: the calculator has one size, and letting it stretch
        // would only let the skin distort.
        //
        // `with_resizable(false)` handles the drag-to-resize border everywhere.
        // `with_enabled_buttons` additionally removes the maximise button; it is
        // winit's portable API and is implemented on Windows and macOS.  On
        // X11/Wayland it is a no-op, but there `resizable(false)` already clears
        // the maximise hint (`platform_impl/linux/x11/window.rs:1424`), so the
        // behaviour is the same everywhere.
        let attributes = Window::default_attributes()
            .with_title("fx-991CN X")
            .with_inner_size(winit::dpi::PhysicalSize::new(w, h))
            .with_resizable(false)
            .with_maximized(false)
            .with_enabled_buttons(WindowButtons::CLOSE | WindowButtons::MINIMIZE);
        let window = Arc::new(event_loop.create_window(attributes).expect("window"));
        block_maximise(&window);

        let device = match GpuDevice::new(&window) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("could not start the GPU: {err}");
                event_loop.exit();
                return;
            }
        };
        let gpu = match device.view(&window) {
            Ok(gpu) => gpu,
            Err(err) => {
                eprintln!("could not draw to the window: {err}");
                event_loop.exit();
                return;
            }
        };

        // Now that the GPU's limit is known, settle the zoom.  A window taller
        // than `max_texture_dimension_2d` makes `Surface:configure` panic, so
        // the cap has to come from the adapter, not from the screen.
        let max_quarters = max_quarters_for_texture_limit(device.max_texture_dimension);
        let quarters = wanted.min(max_quarters);
        if quarters < wanted {
            println!(
                "note: this GPU caps textures at {}px, so the zoom is limited to {:.2}x",
                device.max_texture_dimension,
                quarters as f64 / QUARTERS_PER_UNIT as f64
            );
        }
        self.quarters = Some(quarters);
        let mut machine = Machine::new(
            self.rom.clone(),
            Renderer::new(self.skin.clone()),
            quarters,
            max_quarters,
        );
        let (w, h) = machine.window_size();
        let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(w, h));

        machine.boot();
        machine.throttle.start(Instant::now());
        window.set_title(&format!("fx-991CN X  --  {:.2}x", machine.scale()));
        println!(
            "ready at {:.2}x -- left-click presses, right-click latches, +/- zoom, \
             F12 debugger, Esc quits",
            machine.scale()
        );

        self.device = Some(device);
        self.calculator = Some(CalculatorWindow {
            window,
            gpu,
            machine,
            cursor: (0.0, 0.0),
            frame_size: (NATURAL_WIDTH, NATURAL_HEIGHT),
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // Which window it is decides what a key or a click means, so the two
        // handlers are separate rather than one that tests a mode.
        let is_debugger = self
            .debug
            .as_ref()
            .is_some_and(|debug| debug.window.id() == id);
        if is_debugger {
            self.debug_window_event(event_loop, event);
        } else {
            self.calculator_window_event(event_loop, event);
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        let Some(calc) = self.calculator.as_mut() else {
            return;
        };

        // The debugger holds the machine while it is stopped, and drives it in
        // slices while it runs.  Either way the calculator's own pacing is left
        // alone: the two would otherwise both advance the machine.
        if let Some(debug) = self.debug.as_mut() {
            if debug.session.state() == debug::RunState::Running {
                if debug.session.run_slice(&mut calc.machine.emulator) {
                    // Still going: come back for the next slice immediately.
                    debug.window.request_redraw();
                } else {
                    // The run ended.  A breakpoint or a watch is worth a line on
                    // stdout as well as in the title: a long run that stops
                    // somewhere unexpected is exactly when a log helps.
                    if let Some(reason) = debug.session.stop() {
                        println!("stopped: {}", fx991_dbgui::stop_text(reason));
                    }
                    debug.window.set_title(&format!(
                        "fx-991CN X -- debugger -- {}",
                        debug.session.status()
                    ));
                    debug.window.request_redraw();
                }
            }
            // The panels show the machine's state, which a run changes; repaint
            // them so the listing and registers keep up.
            debug.window.request_redraw();
            return;
        }

        // No debugger: pace the emulator to the real hardware's rate, then run
        // one batch.
        if let Some(delay) = calc.machine.throttle.next_delay(Instant::now()) {
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
        }
        calc.machine.run_batch();

        // Redraw only when the machine says something visible changed.  The
        // frame request covers the screen buffer and the key highlights; a
        // resize is handled by the surface itself.
        if calc.machine.emulator.chipset.take_frame_request() {
            calc.window.request_redraw();
            if std::env::var_os("FX991_TRACE_INPUT").is_some() {
                log_input_state(&mut calc.machine.emulator);
            }
        }
    }
}

impl App {
    /// Handle an event for the calculator window.
    fn calculator_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let Some(calc) = self.calculator.as_mut() else {
            return;
        };
        let window = calc.window.clone();

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                calc.gpu.resize_surface(size.width, size.height);
                // Keep the click mapping in step with the client area, so a
                // window manager's title bar or a scaled display cannot make
                // clicks land on the wrong key.
                calc.machine.mouse.set_client_size(size.width, size.height);
            }

            WindowEvent::CursorMoved { position, .. } => {
                calc.cursor = (position.x, position.y);
            }

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                use winit::keyboard::{Key, NamedKey};
                if matches!(&event.logical_key, Key::Named(NamedKey::F12)) {
                    self.open_debugger(event_loop);
                    return;
                }
                let delta = match &event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        event_loop.exit();
                        return;
                    }
                    // One quarter step per press: fine enough to nudge.
                    Key::Character(text) if matches!(text.as_str(), "+" | "=") => 1,
                    Key::Character(text) if matches!(text.as_str(), "-" | "_") => -1,
                    _ => return,
                };
                calc.machine.nudge_scale(delta);
                self.quarters = Some(calc.machine.quarters);
                window.set_title(&format!("fx-991CN X  --  {:.2}x", calc.machine.scale()));
                let (w, h) = calc.machine.window_size();
                let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(w, h));
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    WinitButton::Left => MouseButton::Left,
                    WinitButton::Right => MouseButton::Right,
                    _ => return,
                };
                let (x, y) = calc.cursor;
                let action = match state {
                    ElementState::Pressed => {
                        calc.machine
                            .mouse
                            .press(&mut calc.machine.emulator.chipset, button, x, y)
                    }
                    ElementState::Released if button == MouseButton::Left => calc
                        .machine
                        .mouse
                        .release(&mut calc.machine.emulator.chipset),
                    // A right-button release does nothing: the latch is toggled
                    // on the press, and stays until the next right-click.
                    ElementState::Released => return,
                };
                match action {
                    Action::Pressed { code, stuck } => {
                        println!("key {code:#04x}{}", if stuck { " (latched)" } else { "" })
                    }
                    Action::ReleasedAll => println!("key release-all"),
                    Action::Missed | Action::Outside => {}
                }
            }

            WindowEvent::RedrawRequested => self.redraw_calculator(event_loop),

            _ => {}
        }
    }

    /// Handle an event for the debugger window.
    fn debug_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        // Closing the window closes the window, not the program: the calculator
        // is the program, and the debugger is something you open over it.
        if matches!(event, WindowEvent::CloseRequested) {
            self.close_debugger();
            return;
        }

        let (Some(calc), Some(debug)) = (self.calculator.as_mut(), self.debug.as_mut()) else {
            return;
        };
        let window = debug.window.clone();

        match event {
            WindowEvent::Resized(size) => {
                debug.gpu.resize_surface(size.width, size.height);
            }

            WindowEvent::CursorMoved { position, .. } => {
                debug.cursor = (position.x, position.y);
            }

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                use winit::keyboard::{Key, NamedKey};
                // `F12` toggles from either window, so the debugger can be closed
                // from the window it opened.
                if matches!(&event.logical_key, Key::Named(NamedKey::F12)) {
                    self.close_debugger();
                    return;
                }
                let Some(key) = debug_key(&event.logical_key) else {
                    return;
                };
                if key == DebugKey::Leave {
                    self.close_debugger();
                    return;
                }
                let size = window.inner_size();
                debug
                    .session
                    .key(&mut calc.machine.emulator, size.width, size.height, key);
                window.set_title(&format!(
                    "fx-991CN X -- debugger -- {}",
                    debug.session.status()
                ));
                window.request_redraw();
            }

            WindowEvent::MouseInput { state, button, .. } => {
                if button != WinitButton::Left || state != ElementState::Pressed {
                    return;
                }
                let size = window.inner_size();
                let (x, y) = debug.cursor;
                debug.session.click(
                    &mut calc.machine.emulator,
                    size.width,
                    size.height,
                    x as u32,
                    y as u32,
                );
                window.request_redraw();
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => lines as isize,
                    // A pixel delta is reported by trackpads; three lines is a
                    // reasonable step and keeps a flick from flying.
                    MouseScrollDelta::PixelDelta(position) => {
                        if position.y > 0.0 {
                            3
                        } else if position.y < 0.0 {
                            -3
                        } else {
                            0
                        }
                    }
                };
                if lines == 0 {
                    return;
                }
                let size = window.inner_size();
                debug.session.key(
                    &mut calc.machine.emulator,
                    size.width,
                    size.height,
                    DebugKey::Scroll(lines),
                );
                window.request_redraw();
            }

            WindowEvent::RedrawRequested => self.redraw_debugger(event_loop),

            _ => {}
        }
    }
}

/// Map a key to a debugger command, or `None` for a key the debugger ignores.
///
/// The bindings are the ones a native debugger uses, so the muscle memory
/// carries: `F7`/`F8` step, `F9` runs, `F2` toggles a breakpoint.  `Esc` leaves
/// the debugger rather than quitting the program -- it is the way back to the
/// calculator, and `F12` does the same from either side.
fn debug_key(key: &winit::keyboard::Key) -> Option<DebugKey> {
    use winit::keyboard::{Key as WKey, NamedKey};
    match key {
        WKey::Named(NamedKey::F2) => Some(DebugKey::ToggleBreakpoint),
        WKey::Named(NamedKey::F5) | WKey::Named(NamedKey::F9) => Some(DebugKey::Run),
        WKey::Named(NamedKey::F7) => Some(DebugKey::Step),
        WKey::Named(NamedKey::F8) => Some(DebugKey::StepOver),
        WKey::Named(NamedKey::F6) => Some(DebugKey::StepOut),
        WKey::Named(NamedKey::F3) => Some(DebugKey::FollowPc),
        WKey::Named(NamedKey::F4) => Some(DebugKey::NextPanel),
        WKey::Named(NamedKey::Escape) => Some(DebugKey::Leave),
        WKey::Named(NamedKey::Space) => Some(DebugKey::Pause),
        WKey::Named(NamedKey::ArrowUp) => Some(DebugKey::Scroll(-1)),
        WKey::Named(NamedKey::ArrowDown) => Some(DebugKey::Scroll(1)),
        WKey::Named(NamedKey::PageUp) => Some(DebugKey::Scroll(-10)),
        WKey::Named(NamedKey::PageDown) => Some(DebugKey::Scroll(10)),
        _ => None,
    }
}

/// Print what the ROM did after a visible change, for correlating with key presses.
///
/// Enabled by `FX991_TRACE_INPUT=1`.  The input area and the screen's lit-pixel count
/// are the two things a key sequence actually changes, and printing them next to the
/// `key` lines makes a session's behaviour readable from the log alone -- which is
/// how a UI-driven session can be compared against a scripted one.
fn log_input_state(emu: &mut fx991::Emu) {
    let input: Vec<String> = (0..12)
        .map(|offset| format!("{:02X}", emu.peek_quiet(0xD180 + offset)))
        .collect();
    let (lit, mode) = {
        let screen = emu.chipset.screen.borrow();
        (screen.lit_pixels(), screen.mode)
    };
    println!(
        "state tick={} input={} lit={} mode={}",
        emu.ticks,
        input.join(" "),
        lit,
        mode
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A machine for the zoom tests.
    ///
    /// They only care about window geometry, so the renderer gets a blank skin of
    /// the right size rather than the real texture -- that keeps them runnable
    /// without the data files.
    fn machine(quarters: u32) -> Machine {
        let blank = vec![0u8; (NATURAL_WIDTH * NATURAL_HEIGHT * 4) as usize];
        let skin = fx991_ui::Skin::from_rgba(blank, NATURAL_WIDTH, NATURAL_HEIGHT)
            .expect("a blank skin of the right size");
        Machine::new(vec![0u8; 1024], Renderer::new(skin), quarters, MAX_QUARTERS)
    }

    #[test]
    fn the_startup_zoom_fits_the_screen() {
        // A 1080p monitor: 2x would be 1194px tall, which does not fit.  With
        // quarter steps it lands on 1.5x (426x896) rather than dropping a whole
        // step to 1x -- that is the point of the finer granularity.
        assert_eq!(fit_quarters(1920, 1080), 6);
        assert_eq!(window_size_for(6), (426, 896));
        // A tiny screen still gets the minimum rather than collapsing.
        assert_eq!(fit_quarters(320, 240), MIN_QUARTERS);
    }

    #[test]
    fn the_startup_zoom_uses_quarter_steps() {
        // The window must never exceed the usable height (screen minus chrome).
        for (w, h) in [(1920, 1080), (2560, 1440), (3840, 2160), (1366, 768)] {
            let q = fit_quarters(w, h);
            let (ww, hh) = window_size_for(q);
            assert!(ww <= w, "{w}x{h}: window {ww} wider than the screen");
            assert!(hh + 80 <= h, "{w}x{h}: window {hh} too tall for the screen");
        }
    }

    #[test]
    fn the_zoom_is_capped() {
        assert!(fit_quarters(7680, 4320) <= MAX_QUARTERS);
        assert_eq!(clamp_quarters(9999), MAX_QUARTERS);
    }

    #[test]
    fn the_zoom_range_is_three_quarters_to_two() {
        // The range the UI is specified to offer.  Pinned because going past 2x
        // used to grow the window until `Surface:configure` panicked.
        assert_eq!(MIN_QUARTERS, 3, "minimum should be 0.75x");
        assert_eq!(MAX_QUARTERS, 8, "maximum should be 2x");
        assert_eq!(window_size_for(MIN_QUARTERS), (213, 448));
        assert_eq!(window_size_for(MAX_QUARTERS), (568, 1194));
    }

    #[test]
    fn the_largest_window_fits_a_1080p_screen() {
        // 2x is 1194px tall against 1080px of screen, so it does not fit *whole*
        // -- but it is still the useful ceiling, and the window is deliberately
        // not resizable so it cannot be dragged larger.  What matters is that it
        // stays well inside the texture limit, which is what used to panic.
        let (w, h) = window_size_for(MAX_QUARTERS);
        assert!(h <= 2048, "2x must stay inside a 2048px texture: {h}");
        assert!(w <= 2048);
    }

    #[test]
    fn pressing_plus_forever_never_passes_the_cap() {
        // The reported bug: holding `+` grew the window until wgpu panicked.
        let mut machine = machine(QUARTERS_PER_UNIT);
        for _ in 0..500 {
            machine.nudge_scale(1);
            let (w, h) = machine.window_size();
            assert!(
                w <= 2048 && h <= 2048,
                "grew past the texture limit: {w}x{h}"
            );
        }
        assert_eq!(machine.quarters, MAX_QUARTERS);
        assert_eq!(machine.scale(), 2.0);
    }

    #[test]
    fn a_weak_gpu_lowers_the_cap_instead_of_crashing() {
        // A software adapter reporting 1024px cannot host 2x (1194px), so the
        // ceiling drops rather than letting `configure` panic.
        let weak = max_quarters_for_texture_limit(1024);
        assert!(weak < MAX_QUARTERS);
        let (_, h) = window_size_for(weak);
        assert!(h + 120 <= 1024, "window {h} does not fit a 1024px texture");

        // A generous adapter keeps the full range.
        assert_eq!(max_quarters_for_texture_limit(32768), MAX_QUARTERS);
    }

    #[test]
    fn width_can_be_the_binding_constraint() {
        // Tall but narrow: 1.5x would be 426px wide, which does not fit in 400.
        assert_eq!(fit_quarters(400, 4000), 5);
        assert_eq!(window_size_for(5), (355, 747));
    }

    #[test]
    fn window_size_rounds_to_whole_pixels() {
        // A quarter step of 597 is 149.25, which must not become a half pixel.
        let (w, h) = window_size_for(5); // 1.25x
        assert_eq!(w, (NATURAL_WIDTH * 5).div_ceil(QUARTERS_PER_UNIT));
        assert_eq!(h, (NATURAL_HEIGHT * 5).div_ceil(QUARTERS_PER_UNIT));
        assert_eq!((w, h), (355, 747));
    }

    #[test]
    fn one_step_is_a_quarter_not_a_whole() {
        // The point of the change: pressing `+` nudges, it does not double.
        let mut machine = machine(QUARTERS_PER_UNIT);
        let before = machine.window_size();
        machine.nudge_scale(1);
        let after = machine.window_size();
        assert!(after.1 > before.1, "the window should grow");
        assert!(
            after.1 < before.1 * 3 / 2,
            "one step should be a nudge, not a jump: {before:?} -> {after:?}"
        );
        assert_eq!(machine.scale(), 1.25);
    }

    #[test]
    fn the_whole_range_is_reachable_in_quarter_steps() {
        let mut machine = machine(MIN_QUARTERS);
        let mut sizes = Vec::new();
        while machine.quarters < MAX_QUARTERS {
            machine.nudge_scale(1);
            sizes.push(machine.window_size().1);
        }
        // Every step grew, and none of them jumped by more than a quarter.
        for pair in sizes.windows(2) {
            assert!(pair[1] > pair[0], "a step did not grow: {pair:?}");
            let jump = pair[1] - pair[0];
            assert!(
                jump <= NATURAL_HEIGHT.div_ceil(QUARTERS_PER_UNIT) + 1,
                "a step jumped {jump}px, which is more than a quarter"
            );
        }
        assert_eq!(sizes.len() as u32, MAX_QUARTERS - MIN_QUARTERS);
    }

    #[test]
    fn nudging_clamps_at_both_ends() {
        let mut machine = machine(QUARTERS_PER_UNIT);
        for _ in 0..100 {
            machine.nudge_scale(1);
        }
        assert_eq!(machine.quarters, MAX_QUARTERS);
        for _ in 0..100 {
            machine.nudge_scale(-1);
        }
        assert_eq!(machine.quarters, MIN_QUARTERS, "must not shrink to nothing");
    }

    #[test]
    fn the_reported_scale_matches_the_window() {
        let machine = machine(8);
        assert_eq!(machine.scale(), 2.0);
        assert_eq!(
            machine.window_size(),
            (NATURAL_WIDTH * 2, NATURAL_HEIGHT * 2)
        );
    }
}
