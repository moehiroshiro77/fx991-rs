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

//! A wgpu window showing the calculator's face, with clickable keys.
//!
//! `text
//! cargo run --release -p fx991-app
//! `
//!
//! Left-click a key to press it, right-click to latch it down (so you can hold
//! `SHIFT` and then click something else), and click `ON` to power-cycle.
//! `+`/`-` zoom, `Esc` quits.
//!
//! The design follows  §M3.  The whole face is composed on the
//! CPU by `fx991-ui` and uploaded as one texture per changed frame -- option (a)
//! in that section -- so the GPU side is a single textured triangle.  Two
//! details matter:
//!
//! * the texture is `Rgba8Unorm`, **not** `Rgba8UnormSrgb`: compositing already
//!   happened in sRGB bytes on the CPU, and an sRGB texture would linearise them
//!   a second time and come out too dark (§5.6);
//! * the sampler is `Nearest`, because the display is a dot matrix and
//!   interpolating it makes the edges mush.
//!
//! Everything testable without a GPU lives in [`input`] and in `fx991-ui`; this
//! file is the glue.

mod gpu;
mod input;
mod window;
mod zoom;

use std::sync::Arc;
use std::time::Instant;

use fx991::throttle::Throttle;
use fx991::Emu;
use fx991_ui::{Renderer, NATURAL_HEIGHT, NATURAL_WIDTH};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton as WinitButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowButtons, WindowId};

use gpu::{Gpu, SurfaceProblem};
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

/// What the command line asked for.
struct Options {
    rom: std::path::PathBuf,
    skin: std::path::PathBuf,
    /// The zoom in quarter steps, or `None` to fit the screen.
    quarters: Option<u32>,
}

const USAGE: &str = "fx991cnx -- a clickable fx-991CN X

usage: fx991cnx [ZOOM] [--rom PATH] [--skin PATH]

  ZOOM        0.5 to 8 (default: fit the screen)
  --rom PATH  ROM image           (default: data/rom_verF.bin)
  --skin PATH face texture, RGBA  (default: data/skin.rgba)

Keys: left-click presses, right-click latches, +/- zoom, Esc quits.";

fn parse_args() -> Result<Options, String> {
    let mut options = Options {
        rom: DEFAULT_ROM.into(),
        skin: DEFAULT_SKIN.into(),
        quarters: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--rom" => options.rom = args.next().ok_or("--rom needs a path")?.into(),
            "--skin" => options.skin = args.next().ok_or("--skin needs a path")?.into(),
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

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(rom, skin, options.quarters);
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

/// The winit application.
struct App {
    rom: Vec<u8>,
    skin: fx991_ui::Skin,
    /// `None` means "fit the screen"; `Some` is an explicit user choice.
    /// In quarter steps.
    quarters: Option<u32>,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    machine: Option<Machine>,
    /// Last cursor position, in *window* pixels.
    cursor: (f64, f64),
    /// The frame size the GPU texture currently holds.
    frame_size: (u32, u32),
}

impl App {
    fn new(rom: Vec<u8>, skin: fx991_ui::Skin, quarters: Option<u32>) -> Self {
        Self {
            rom,
            skin,
            quarters,
            window: None,
            gpu: None,
            machine: None,
            cursor: (0.0, 0.0),
            frame_size: (NATURAL_WIDTH, NATURAL_HEIGHT),
        }
    }

    /// Re-upload the frame and present it.
    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let (Some(window), Some(gpu), Some(machine)) = (
            self.window.clone(),
            self.gpu.as_mut(),
            self.machine.as_mut(),
        ) else {
            return;
        };
        let frame = machine.renderer.render(&mut machine.emulator.chipset);
        if (frame.width, frame.height) != self.frame_size {
            gpu.resize_texture(frame.width, frame.height);
            self.frame_size = (frame.width, frame.height);
        }
        match gpu.draw(&frame) {
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
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
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

        let gpu = match Gpu::new(window.clone()) {
            Ok(gpu) => gpu,
            Err(err) => {
                eprintln!("could not start the GPU: {err}");
                event_loop.exit();
                return;
            }
        };

        // Now that the GPU's limit is known, settle the zoom.  A window taller
        // than `max_texture_dimension_2d` makes `Surface:configure` panic, so
        // the cap has to come from the adapter, not from the screen.
        let max_quarters = max_quarters_for_texture_limit(gpu.max_texture_dimension);
        let quarters = wanted.min(max_quarters);
        if quarters < wanted {
            println!(
                "note: this GPU caps textures at {}px, so the zoom is limited to {:.2}x",
                gpu.max_texture_dimension,
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
            "ready at {:.2}x -- left-click presses, right-click latches, +/- zoom, Esc quits",
            machine.scale()
        );

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.machine = Some(machine);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let (Some(window), Some(machine)) = (self.window.clone(), self.machine.as_mut()) else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize_surface(size.width, size.height);
                }
                // Keep the click mapping in step with the client area, so a
                // window manager's title bar or a scaled display cannot make
                // clicks land on the wrong key.
                machine.mouse.set_client_size(size.width, size.height);
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
            }

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                use winit::keyboard::{Key, NamedKey};
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
                machine.nudge_scale(delta);
                self.quarters = Some(machine.quarters);
                window.set_title(&format!("fx-991CN X  --  {:.2}x", machine.scale()));
                let (w, h) = machine.window_size();
                let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(w, h));
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    WinitButton::Left => MouseButton::Left,
                    WinitButton::Right => MouseButton::Right,
                    _ => return,
                };
                let (x, y) = self.cursor;
                let action = match state {
                    ElementState::Pressed => {
                        machine
                            .mouse
                            .press(&mut machine.emulator.chipset, button, x, y)
                    }
                    ElementState::Released if button == MouseButton::Left => {
                        machine.mouse.release(&mut machine.emulator.chipset)
                    }
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

            WindowEvent::RedrawRequested => self.redraw(event_loop),

            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        let (Some(window), Some(machine)) = (self.window.clone(), self.machine.as_mut()) else {
            return;
        };

        // Pace the emulator to the real hardware's rate, then run one batch.
        if let Some(delay) = machine.throttle.next_delay(Instant::now()) {
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
        }
        machine.run_batch();

        // Redraw only when the machine says something visible changed.  The
        // frame request covers the screen buffer and the key highlights; a
        // resize is handled by the surface itself.
        if machine.emulator.chipset.take_frame_request() {
            window.request_redraw();
            if std::env::var_os("FX991_TRACE_INPUT").is_some() {
                log_input_state(&mut machine.emulator);
            }
        }
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
