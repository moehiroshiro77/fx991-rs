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

//! The command language, shared by the interactive prompt and by script files.
//!
//! One parser, one dispatcher, so a script and a REPL session do exactly the same
//! thing -- which is what makes a script a reproducible record of a session rather
//! than a second implementation of one.
//!
//! # Shape
//!
//! ```text
//! s [n]              step [n] instructions
//! n / over           step over a call
//! out                step out of the current call
//! g / continue       run until something stops the machine
//! r ADDR             run to an address
//! t N                run N ticks, ignoring breakpoints
//! bp ADDR [if EXPR] [once] [log]
//! bl / bc N / be N   list, clear, enable-or-disable breakpoints
//! wp ADDR [r|w|rw|x] [if EXPR]
//! wl / wc N
//! regs / get REG / set REG VALUE
//! m ADDR [LEN] / poke ADDR BYTES / stack [LEN]
//! disas [ADDR] [N] / screen / periph / ints / port
//! patch ADDR INSTRUCTION / patches / undo N
//! snap NAME / restore NAME / snaps / diff A B
//! trace on [N] / trace off / trace show [N] / trace save PATH
//! key NAME / keys KEYS / runmode run
//! echo TEXT / # comment / q
//! ```
//!
//! # Why the mnemonics are these
//!
//! The short ones (`s`, `t`, `g`, `m`, `sp`, `q`) are the Python prototype's, which
//! this project's notes already document, so they carry over.  The long ones
//! (`disas`, `patch`, `snap`, `restore`) match what `emu`'s subcommands already
//! call those things.  Where the two traditions disagree the short form wins,
//! because it is what gets typed.
//!
//! # Errors are values, not panics
//!
//! Every command returns a [`CommandResult`].  A script stops at the first failure
//! and the line number is reported, because a script that continued past a failed
//! assertion would report success for a run that proved nothing.

use fx991::Emu;

use crate::breakpoint::{Breakpoint, Watch, WatchKind};
use crate::condition::Expr;
use crate::patch;
use crate::session::{Debugger, StopReason};
use crate::view;

/// What running a command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandResult {
    /// Output to show, and continue.
    Output(String),
    /// Nothing to show.
    Silent,
    /// The session should end.
    Quit,
}

/// Why a command could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError {
    /// What went wrong.
    pub message: String,
}

impl CommandError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandError {}

type Result<T> = std::result::Result<T, CommandError>;

/// How a `run`-family command writes `StopReason`, and whether it prints anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Report {
    /// Print where it stopped.
    Stop,
    /// Print nothing on a normal stop, for a script that only cares about the next
    /// command's assertions.
    Quiet,
}

/// The default tick budget for a run, matching `emu run`'s.
///
/// A budget is not optional in spirit: the ROM parks in STOP for most of its life,
/// so an unbounded run is a hang rather than a result.
pub const DEFAULT_BUDGET: u64 = 10_000_000;

/// How long to wait for the ROM to *accept* a key, in ticks.
///
/// This is an upper bound, not a delay: the wait ends at the accept edge
/// (`rom:0F968`), so a key that is taken promptly costs only what it takes.  The
/// number matches `Calculator::DEFAULT_ACCEPT_BUDGET`, which has waited on the same
/// edge since the calculator front end was written.
pub const DEFAULT_ACCEPT_TICKS: u64 = 4_000_000;

/// How long to let the ROM settle before and after a keypress, in ticks.
///
/// A short fixed amount, not a "is it done yet" wait: the machine has to scan the
/// matrix with the key down, and then get back to its idle loop before the next
/// command looks at it.
pub const DEFAULT_SETTLE_TICKS: u64 = 120_000;

/// Kept for the places that only need a short run, so the intent reads clearly.
pub const DEFAULT_TAP_TICKS: u64 = DEFAULT_SETTLE_TICKS;

/// Everything a command needs to act on.
pub struct Session<'a> {
    /// The machine.
    pub emu: &'a mut Emu,
    /// The debugger's own state.
    pub debugger: &'a mut Debugger,
    /// Whether output is being produced for a person or a script.
    pub quiet: bool,
}

impl Session<'_> {
    /// Run one command line.
    pub fn execute(&mut self, line: &str) -> Result<CommandResult> {
        let line = strip_comment(line).trim();
        if line.is_empty() {
            return Ok(CommandResult::Silent);
        }
        let (verb, rest) = split_verb(line);
        let words: Vec<&str> = rest.split_whitespace().collect();

        match verb {
            // ------------------------------------------------------------ stepping
            "s" | "step" => {
                let count = self.optional_number(&words, 0, "step")?.unwrap_or(1);
                for _ in 0..count {
                    let reason = self.debugger.step(self.emu);
                    self.report_step(&reason)?;
                }
                Ok(CommandResult::Output(view::summary(self.emu)))
            }
            "n" | "next" | "over" => {
                let reason = self.debugger.step_over(self.emu, DEFAULT_BUDGET);
                self.report_step(&reason)?;
                Ok(CommandResult::Output(view::summary(self.emu)))
            }
            "out" | "finish" => {
                let reason = self.debugger.step_out(self.emu, DEFAULT_BUDGET);
                self.report_step(&reason)?;
                Ok(CommandResult::Output(view::summary(self.emu)))
            }
            "g" | "continue" | "c" => {
                // An optional budget, because the ROM parks in STOP for most of its
                // life and a run that has to wait out a sleep needs a bigger one.
                let budget = self
                    .optional_number(&words, 0, "g")?
                    .unwrap_or(DEFAULT_BUDGET);
                let reason = self.debugger.run(self.emu, budget);
                Ok(CommandResult::Output(self.stop_text(&reason, Report::Stop)))
            }
            "r" | "run_to" => {
                let address = self.address(&words, 0, "run to an address")?;
                let reason = self.debugger.run_to(self.emu, address, DEFAULT_BUDGET);
                Ok(CommandResult::Output(self.stop_text(&reason, Report::Stop)))
            }
            "t" | "ticks" => {
                let count = self.required_number(&words, 0, "t wants a tick count")?;
                self.debugger.run_uninterrupted(self.emu, count);
                Ok(CommandResult::Output(view::summary(self.emu)))
            }
            "runmode" => {
                // The one command that does something about STOP, which a stepper
                // otherwise cannot leave.
                if words.first() != Some(&"run") {
                    return Err(CommandError::new("runmode wants `run`"));
                }
                self.debugger.force_run(self.emu);
                Ok(CommandResult::Output("running\n".to_string()))
            }

            // --------------------------------------------------------- breakpoints
            "bp" => self.breakpoint(&words),
            "bl" | "bps" => Ok(CommandResult::Output(view::state(self.debugger, self.emu))),
            "bc" => {
                self.debugger.clear_breakpoints();
                Ok(CommandResult::Output("breakpoints cleared\n".to_string()))
            }
            "be" | "bd" => {
                let id = self.breakpoint_id(&words, "be wants a breakpoint number")?;
                let enable = verb == "be";
                match self.debugger.breakpoint_mut(id) {
                    Some(breakpoint) => {
                        breakpoint.enabled = enable;
                        Ok(CommandResult::Output(format!(
                            "breakpoint {} {}\n",
                            id.0,
                            if enable { "enabled" } else { "disabled" }
                        )))
                    }
                    None => Err(CommandError::new(format!("no breakpoint {}", id.0))),
                }
            }

            // -------------------------------------------------------------- watches
            "wp" | "watch" => self.watch(&words),
            "wl" | "watches" => Ok(CommandResult::Output(view::state(self.debugger, self.emu))),
            "wc" => {
                self.debugger.clear_watches(self.emu);
                Ok(CommandResult::Output("watches cleared\n".to_string()))
            }

            // ------------------------------------------------------------ registers
            "regs" | "reg" => Ok(CommandResult::Output(view::registers(self.emu))),
            "get" => {
                let name = words
                    .first()
                    .ok_or_else(|| CommandError::new("get wants a register name"))?;
                let value = self.register_value(name)?;
                Ok(CommandResult::Output(format!("{name} = {value:04X}h\n")))
            }
            "set" => {
                let name = *words
                    .first()
                    .ok_or_else(|| CommandError::new("set wants a register name"))?;
                let text = *words
                    .get(1)
                    .ok_or_else(|| CommandError::new("set wants a value"))?;
                let value = parse_number(text)
                    .ok_or_else(|| CommandError::new(format!("cannot read {text:?}")))?;
                self.set_register(name, value as u64)?;
                Ok(CommandResult::Output(view::registers(self.emu)))
            }
            "sp" => {
                let text = *words
                    .first()
                    .ok_or_else(|| CommandError::new("sp wants a value"))?;
                let value = parse_number(text)
                    .ok_or_else(|| CommandError::new(format!("cannot read {text:?}")))?;
                self.emu.set_sp(value as u16);
                Ok(CommandResult::Output(view::registers(self.emu)))
            }

            // --------------------------------------------------------------- memory
            "m" | "mem" | "dump" => {
                let address = self.address(&words, 0, "m wants an address")?;
                let length = self.optional_number(&words, 1, "m")?.unwrap_or(0x40) as usize;
                Ok(CommandResult::Output(view::memory(
                    self.emu, address, length,
                )))
            }
            "poke" => {
                let address = self.address(&words, 0, "poke wants an address")?;
                if words.len() < 2 {
                    return Err(CommandError::new(
                        "poke wants hex bytes, e.g. `poke D180 41 42`",
                    ));
                }
                // Separators are optional, so the tail is joined and parsed as one.
                let parsed = parse_bytes(&words[1..].join(" "))?;
                self.emu.poke_block(address, &parsed);
                Ok(CommandResult::Output(format!(
                    "{} bytes written at {address:05X}h\n",
                    parsed.len()
                )))
            }
            "stack" | "st" => {
                let length = self.optional_number(&words, 0, "stack")?.unwrap_or(32) as usize;
                Ok(CommandResult::Output(view::stack(self.emu, length)))
            }

            // ---------------------------------------------------------------- views
            "disas" | "u" | "dis" => {
                let address = match words.first() {
                    Some(word) => parse_number(word)
                        .ok_or_else(|| CommandError::new(format!("cannot read {word:?}")))?,
                    None => self.emu.pc(),
                };
                let count = match words.get(1) {
                    Some(word) => parse_number(word)
                        .ok_or_else(|| CommandError::new(format!("cannot read {word:?}")))?
                        as usize,
                    None => 10,
                };
                Ok(CommandResult::Output(view::disassembly(
                    self.debugger,
                    self.emu,
                    address,
                    count,
                )))
            }
            "context" | "ctx" => Ok(CommandResult::Output(format!(
                "{}{}{}",
                view::summary(self.emu),
                view::disassembly_at_pc(self.debugger, self.emu, 4, 6),
                view::registers(self.emu)
            ))),
            "screen" => Ok(CommandResult::Output(view::screen(self.emu))),
            "periph" => Ok(CommandResult::Output(view::peripherals(self.emu))),
            "ints" | "interrupts" => Ok(CommandResult::Output(view::interrupts(self.emu))),
            "port" => Ok(CommandResult::Output(format!(
                "{}{}",
                view::interrupts(self.emu),
                view::peripherals(self.emu)
            ))),

            // -------------------------------------------------------------- patches
            "patch" => {
                let address = self.address(&words, 0, "patch wants an address")?;
                let text = words[1..].join(" ");
                if text.is_empty() {
                    return Err(CommandError::new("patch wants an instruction"));
                }
                let bytes = patch::assemble_instruction(&text)
                    .map_err(|error| CommandError::new(error.to_string()))?;
                let applied = self
                    .debugger
                    .apply_patch(self.emu, address, &bytes, &text)
                    .map_err(|error| CommandError::new(error.to_string()))?;
                Ok(CommandResult::Output(format!(
                    "{:07X}h: {} (was {}) -- {}\n",
                    applied.address,
                    applied.byte_text(),
                    applied.original_text(),
                    applied.source
                )))
            }
            "patches" => Ok(CommandResult::Output(view::state(self.debugger, self.emu))),
            "undo" => {
                let address = self.address(&words, 0, "undo wants an address")?;
                if self.debugger.undo_patch(self.emu, address) {
                    Ok(CommandResult::Output(format!("undid {address:07X}h\n")))
                } else {
                    Err(CommandError::new(format!("no patch at {address:07X}h")))
                }
            }
            "undoall" => {
                let count = self.debugger.undo_all_patches(self.emu);
                Ok(CommandResult::Output(format!("{count} patches undone\n")))
            }

            // ------------------------------------------------------------ snapshots
            "snap" => {
                let name = words
                    .first()
                    .ok_or_else(|| CommandError::new("snap wants a name"))?;
                self.debugger.save_snapshot(*name, self.emu);
                Ok(CommandResult::Output(format!("saved {name}\n")))
            }
            "restore" | "rs" => {
                let name = words
                    .first()
                    .ok_or_else(|| CommandError::new("restore wants a name"))?;
                if self.debugger.restore_snapshot(name, self.emu) {
                    Ok(CommandResult::Output(view::summary(self.emu)))
                } else {
                    Err(CommandError::new(format!("no snapshot called {name}")))
                }
            }
            "snaps" => Ok(CommandResult::Output(view::state(self.debugger, self.emu))),
            "diff" => {
                let left = *words
                    .first()
                    .ok_or_else(|| CommandError::new("diff wants two names"))?;
                let right = *words
                    .get(1)
                    .ok_or_else(|| CommandError::new("diff wants two names"))?;
                let diff = self.debugger.diff_snapshots(left, right).ok_or_else(|| {
                    CommandError::new(format!("no snapshot called {left} or {right}"))
                })?;
                Ok(CommandResult::Output(view::diff(&diff)))
            }
            "diffnow" => {
                let name = words
                    .first()
                    .ok_or_else(|| CommandError::new("diffnow wants a name"))?;
                let diff = self
                    .debugger
                    .diff_with_now(name, self.emu)
                    .ok_or_else(|| CommandError::new(format!("no snapshot called {name}")))?;
                Ok(CommandResult::Output(view::diff(&diff)))
            }

            // ----------------------------------------------------------------- trace
            "trace" => self.trace(&words),

            // ------------------------------------------------------------------ keys
            // `tap` is the one key command: press it, wait for the ROM to *accept*
            // it, then let go.
            //
            // The wait is on the accept edge (`rom:0F968`), not on a tick count.
            // That distinction is the whole point: the ROM only takes a key while
            // its input filter (`0xF042`) is armed, and it disarms again within a
            // few hundred thousand ticks.  A tick count is therefore a guess, and a
            // wrong guess drops the key silently rather than reporting anything.
            //
            // Waiting for the edge also makes a modifier work the way a person
            // expects: `tap SHIFT` arms it, and the ROM keeps it armed until the
            // next key, so `tap SHIFT` then `tap 8` opens the unit menu without
            // either key being held.
            "tap" | "press" => {
                let name = words
                    .first()
                    .ok_or_else(|| CommandError::new("tap wants a name, e.g. `tap 8`"))?;
                let code = self.key_code(name)?;
                // `tap EXE 0` runs nothing after the accept edge.  A script that
                // wants to watch what the key *causes* needs that: the interesting
                // work -- evaluating, copying the input area, running a ROP chain --
                // happens in the settle window, so a settle would step over all of
                // it before the next breakpoint could be armed.
                let settle = self
                    .optional_number(&words, 1, "tap")?
                    .unwrap_or(DEFAULT_SETTLE_TICKS);
                let stopped = self.tap_key(code, settle)?;
                Ok(CommandResult::Output(match stopped {
                    Some(reason) => format!("tapped {name} ({code:02X}h); {}\n", reason.text()),
                    None => format!("tapped {name} ({code:02X}h)\n"),
                }))
            }
            // `latch` holds a key down without a finger, and toggles it off again.
            //
            // This is the right-click behaviour from the window, and it is what the
            // two cases that genuinely need a key *down* use: the self-test
            // combination (SHIFT and 7 held through an ON reset) and a multi-key
            // chord.  A latched key also survives a reset, which is what makes the
            // self-test work at all -- see the note in `keyboard.rs`.
            "latch" => {
                if words.is_empty() {
                    return Err(CommandError::new("latch wants at least one key"));
                }
                let mut changed = Vec::new();
                for name in &words {
                    let code = self.key_code(name)?;
                    let was_latched = self.emu.chipset.keyboard.borrow().is_stuck(code);
                    if !self.emu.chipset.press_key_with(code, true) {
                        return Err(CommandError::new(format!(
                            "no such matrix code {code:02X}h"
                        )));
                    }
                    changed.push(format!(
                        "{name} ({code:02X}h)={}",
                        if was_latched { "off" } else { "on" }
                    ));
                }
                self.debugger
                    .run_uninterrupted(self.emu, DEFAULT_SETTLE_TICKS);
                Ok(CommandResult::Output(format!(
                    "latched {}\n",
                    changed.join(" ")
                )))
            }
            "keys" => {
                let text = words.join(" ");
                if text.is_empty() {
                    return Err(CommandError::new("keys wants something to press"));
                }
                let mut pressed = Vec::new();
                for name in tokenize_keys(&text) {
                    let code = self.key_code(&name)?;
                    if let Some(reason) = self.tap_key(code, DEFAULT_SETTLE_TICKS)? {
                        return Ok(CommandResult::Output(format!("{}\n", reason.text())));
                    }
                    pressed.push(name);
                }
                Ok(CommandResult::Output(format!(
                    "pressed {}\n",
                    pressed.join(" ")
                )))
            }

            // ------------------------------------------------------------- meta
            "echo" => Ok(CommandResult::Output(format!("{rest}\n"))),
            // `assert` is what makes a script evidence rather than a transcript: it
            // fails loudly when a claim is wrong, and the runner stops there.  The
            // condition language already knows how to compare, so this reuses it.
            "assert" => {
                if rest.is_empty() {
                    return Err(CommandError::new("assert wants a condition"));
                }
                let condition =
                    Expr::parse(rest).map_err(|error| CommandError::new(error.to_string()))?;
                let hits = 0;
                let holds = {
                    let mut context = crate::condition::Context {
                        regs: &self.emu.chipset.cpu.regs,
                        bus: &mut self.emu.chipset.bus,
                        hits,
                    };
                    condition.eval(&mut context)
                };
                if holds {
                    Ok(CommandResult::Silent)
                } else {
                    Err(CommandError::new(format!("assert failed: {rest}")))
                }
            }
            "fill" => {
                let address = self.address(&words, 0, "fill wants an address")?;
                let length = self.required_number(&words, 1, "fill wants a length")? as usize;
                let byte = match words.get(2) {
                    Some(word) => parse_number(word)
                        .ok_or_else(|| CommandError::new(format!("cannot read {word:?}")))?
                        as u8,
                    None => 0,
                };
                let block = vec![byte; length];
                self.emu.poke_block(address, &block);
                Ok(CommandResult::Output(format!(
                    "{length} bytes of {byte:02X}h at {address:05X}h\n"
                )))
            }
            "q" | "quit" | "exit" => Ok(CommandResult::Quit),
            "help" | "?" => Ok(CommandResult::Output(help())),
            other => Err(CommandError::new(format!(
                "unknown command {other:?}; try `help`"
            ))),
        }
    }

    // -------------------------------------------------------------- helpers

    fn breakpoint(&mut self, words: &[&str]) -> Result<CommandResult> {
        let mut address = None;
        let mut condition = None;
        let mut once = false;
        let mut log_only = false;
        let mut index = 0;
        while index < words.len() {
            match words[index] {
                "if" => {
                    // The condition is everything after `if`, so it can contain
                    // spaces without quoting -- which matters, because a condition
                    // with a space in it is common (`r0 == 1 && z`).
                    let text = words[index + 1..].join(" ");
                    if text.is_empty() {
                        return Err(CommandError::new("`if` wants a condition"));
                    }
                    condition = Some(
                        Expr::parse(&text).map_err(|error| CommandError::new(error.to_string()))?,
                    );
                    index = words.len();
                }
                "once" => {
                    once = true;
                    index += 1;
                }
                "log" => {
                    log_only = true;
                    index += 1;
                }
                word => {
                    if address.is_some() {
                        return Err(CommandError::new(format!("unexpected {word:?}")));
                    }
                    address = Some(
                        parse_number(word)
                            .ok_or_else(|| CommandError::new(format!("cannot read {word:?}")))?,
                    );
                    index += 1;
                }
            }
        }
        let address =
            address.ok_or_else(|| CommandError::new("bp wants an address, e.g. `bp 0F968h`"))?;

        let mut breakpoint = Breakpoint::auto(address);
        if let Some(condition) = condition {
            breakpoint = breakpoint.with_condition(condition);
        }
        if once {
            breakpoint = breakpoint.once();
        }
        if log_only {
            breakpoint = breakpoint.log_only();
        }
        let id = self.debugger.add_breakpoint(breakpoint);
        Ok(CommandResult::Output(format!(
            "breakpoint {} at {}\n",
            id.0,
            self.debugger
                .breakpoint(id)
                .map(|breakpoint| breakpoint.address_text())
                .unwrap_or_default()
        )))
    }

    fn watch(&mut self, words: &[&str]) -> Result<CommandResult> {
        let mut address = None;
        let mut kind = None;
        let mut condition = None;
        let mut index = 0;
        while index < words.len() {
            match words[index] {
                "if" => {
                    let text = words[index + 1..].join(" ");
                    if text.is_empty() {
                        return Err(CommandError::new("`if` wants a condition"));
                    }
                    condition = Some(
                        Expr::parse(&text).map_err(|error| CommandError::new(error.to_string()))?,
                    );
                    index = words.len();
                }
                word => {
                    if let Some(parsed) = WatchKind::parse(word) {
                        if kind.is_some() {
                            return Err(CommandError::new("wp got two access kinds"));
                        }
                        kind = Some(parsed);
                    } else if address.is_none() {
                        address =
                            Some(parse_number(word).ok_or_else(|| {
                                CommandError::new(format!("cannot read {word:?}"))
                            })?);
                    } else {
                        return Err(CommandError::new(format!("unexpected {word:?}")));
                    }
                    index += 1;
                }
            }
        }
        let address =
            address.ok_or_else(|| CommandError::new("wp wants an address, e.g. `wp D180 w`"))?;
        // A bare address watches both directions, which is what "watch this
        // variable" means and is the friendlier default.
        let mut watch = Watch::new(address, kind.unwrap_or(WatchKind::ReadWrite));
        if let Some(condition) = condition {
            watch = watch.with_condition(condition);
        }
        let id = self.debugger.add_watch(self.emu, watch);
        Ok(CommandResult::Output(format!(
            "watch {} at {address:05X}h {}\n",
            id.0,
            self.debugger
                .watch(id)
                .map(|watch| watch.kind.label())
                .unwrap_or("rw")
        )))
    }

    fn trace(&mut self, words: &[&str]) -> Result<CommandResult> {
        match words.first().copied() {
            None | Some("show") => {
                let count = match words.get(1) {
                    Some(word) => parse_number(word)
                        .ok_or_else(|| CommandError::new(format!("cannot read {word:?}")))?
                        as usize,
                    None => 16,
                };
                Ok(CommandResult::Output(view::trace_tail(
                    self.debugger,
                    count,
                )))
            }
            Some("on") | Some("start") => {
                let capacity = match words.get(1) {
                    Some(word) => parse_number(word)
                        .ok_or_else(|| CommandError::new(format!("cannot read {word:?}")))?
                        as usize,
                    None => 1024,
                };
                self.debugger.start_trace(capacity);
                Ok(CommandResult::Output(format!(
                    "tracing into {capacity} entries\n"
                )))
            }
            Some("off") | Some("stop") => {
                let trace = self.debugger.stop_trace();
                match trace {
                    Some(trace) => Ok(CommandResult::Output(format!(
                        "trace stopped: {} entries, {} recorded{}\n",
                        trace.len(),
                        trace.recorded(),
                        if trace.has_dropped() {
                            " (older ones dropped)"
                        } else {
                            ""
                        }
                    ))),
                    None => Err(CommandError::new("no trace is running")),
                }
            }
            Some("save") => {
                let path = words
                    .get(1)
                    .ok_or_else(|| CommandError::new("trace save wants a path"))?;
                let trace = self
                    .debugger
                    .trace()
                    .ok_or_else(|| CommandError::new("no trace is running"))?;
                std::fs::write(path, trace.to_csv())
                    .map_err(|error| CommandError::new(format!("cannot write {path}: {error}")))?;
                Ok(CommandResult::Output(format!("wrote {path}\n")))
            }
            Some(other) => Err(CommandError::new(format!(
                "trace wants on, off, show or save, not {other:?}"
            ))),
        }
    }

    fn report_step(&self, reason: &StopReason) -> Result<()> {
        match reason {
            StopReason::Stopped => Err(CommandError::new(
                "the machine is halted or stopped, so nothing ran; \
                 `runmode run` leaves standby",
            )),
            _ => Ok(()),
        }
    }

    fn stop_text(&self, reason: &StopReason, report: Report) -> String {
        match reason {
            StopReason::Breakpoint { .. } | StopReason::Watch { .. } => {
                format!("stopped: {}\n{}", reason.text(), view::registers(self.emu))
            }
            StopReason::Stopped => format!("stopped: {}\n", reason.text()),
            StopReason::Budget { .. } => {
                if report == Report::Quiet {
                    String::new()
                } else {
                    format!("{}\n", reason.text())
                }
            }
            StopReason::Paused => String::new(),
        }
    }

    fn register_value(&self, name: &str) -> Result<u64> {
        let regs = &self.emu.chipset.cpu.regs;
        let lower = name.to_ascii_lowercase();
        if let Some(index) = index_suffix(&lower, "er") {
            if index % 2 == 0 && index < 16 {
                return Ok(regs.er(index as usize) as u64);
            }
            return Err(CommandError::new(format!("no register {name}")));
        }
        if let Some(index) = index_suffix(&lower, "r") {
            if index < 16 {
                return Ok(regs.r[index as usize] as u64);
            }
            return Err(CommandError::new(format!("no register {name}")));
        }
        Ok(match lower.as_str() {
            "pc" => regs.physical_pc() as u64,
            "csr" => regs.csr as u64,
            "sp" => regs.sp as u64,
            "psw" => regs.psw() as u64,
            "ea" => regs.ea as u64,
            "lr" => regs.lr() as u64,
            "lcsr" => regs.lcsr() as u64,
            "dsr" => regs.dsr as u64,
            _ => return Err(CommandError::new(format!("no register {name}"))),
        })
    }

    fn set_register(&mut self, name: &str, value: u64) -> Result<()> {
        let lower = name.to_ascii_lowercase();
        if let Some(index) = index_suffix(&lower, "er") {
            if index % 2 == 0 && index < 16 {
                self.emu.set_er(index as usize, value as u16);
                return Ok(());
            }
        } else if let Some(index) = index_suffix(&lower, "r") {
            if index < 16 {
                self.emu.set_reg(index as usize, value as u8);
                return Ok(());
            }
        } else {
            match lower.as_str() {
                "sp" => {
                    self.emu.set_sp(value as u16);
                    return Ok(());
                }
                "pc" => {
                    self.emu.chipset.cpu.regs.pc = value as u16;
                    return Ok(());
                }
                "csr" => {
                    self.emu.chipset.cpu.regs.csr = value as u16;
                    return Ok(());
                }
                "psw" => {
                    self.emu.chipset.cpu.regs.set_psw(value as u8);
                    return Ok(());
                }
                "ea" => {
                    self.emu.chipset.cpu.regs.ea = value as u16;
                    return Ok(());
                }
                "lr" => {
                    self.emu.chipset.cpu.regs.set_lr(value as u16);
                    return Ok(());
                }
                "lcsr" => {
                    self.emu.chipset.cpu.regs.set_lcsr(value as u16);
                    return Ok(());
                }
                "dsr" => {
                    self.emu.chipset.cpu.regs.dsr = value as u8;
                    return Ok(());
                }
                _ => {}
            }
        }
        Err(CommandError::new(format!("cannot set {name}")))
    }

    /// A key by printed name, or by matrix code when the argument is a number.
    ///
    /// **The name lookup runs first, and that order matters.**  `tap 4` means the
    /// key printed `4`, whose matrix code is `0x01` -- but `4` also parses as the
    /// number 4, which is the code of a different key.  Resolving the number first
    /// silently pressed the wrong one.  A code therefore has to be written in a form
    /// that cannot be a name: `0x`-prefixed, or with an `h` suffix.
    fn key_code(&self, name: &str) -> Result<u8> {
        if let Some(code) = self.emu.chipset.keyboard.borrow().code_for_name(name) {
            return Ok(code);
        }

        // Not a known name, so a code is the only thing left.  The `0x`/`h` forms
        // are required here because a bare digit run was a name that did not exist.
        let looks_like_code = name.starts_with("0x")
            || name.starts_with("0X")
            || name.ends_with('h')
            || name.ends_with('H');
        if looks_like_code {
            if let Some(code) = parse_number(name) {
                if code <= 0xFF {
                    return Ok(code as u8);
                }
                return Err(CommandError::new(format!(
                    "{name} is not a matrix code (0-0FFh)"
                )));
            }
        }

        Err(CommandError::new(format!(
            "no key called {name}; the model leaves some keys unnamed, so give a \
             code as `0x05` or `05h`"
        )))
    }

    /// Press a key, wait for the ROM to accept it, then release it.
    ///
    /// The wait is on the **accept edge** (`rom:0F968`), which is the same signal
    /// [`fx991::Calculator::tap_code`] waits on, rather than on a tick count.  A
    /// tick count is a guess about when the ROM's key-wait loop is armed; the edge
    /// is the fact.  Guessing is what silently dropped keys.
    fn tap_key(&mut self, code: u8, settle: u64) -> Result<Option<StopReason>> {
        let power = self.emu.chipset.keyboard.borrow().is_power_key(code);
        if !self.emu.chipset.press_key(code) {
            return Err(CommandError::new(format!(
                "no such matrix code {code:02X}h"
            )));
        }

        // The power key resets the machine instead of entering the matrix, so it
        // never reaches the accept edge.  It still needs time to re-initialise, and
        // a reset is exactly the case where zero settle would leave the machine
        // mid-init, so the power path keeps a floor.
        if power {
            let ticks = settle.max(DEFAULT_SETTLE_TICKS);
            let stopped = self.run_settle(ticks);
            self.emu.chipset.keyboard.borrow_mut().release(Some(code));
            let after = self.run_settle(ticks);
            return Ok(stopped.or(after));
        }

        let accepted = self.run_to_accept_edge();

        // **The settle goes through the breakpoint-aware run, not a plain tick
        // loop.**  What a key press *causes* is the interesting part -- evaluating,
        // copying the input area, running a ROP chain -- so a breakpoint armed on
        // any of that has to fire during the settle, or it is silently skipped along
        // with the instructions.
        let stopped = self.run_settle(settle);
        self.emu.chipset.keyboard.borrow_mut().release(Some(code));
        let after = self.run_settle(settle);

        if !accepted {
            return Err(CommandError::new(format!(
                "the ROM never accepted key {code:02X}h within {DEFAULT_ACCEPT_TICKS} ticks; \
                 it is probably in a menu or a mode that ignores matrix keys"
            )));
        }
        Ok(stopped.or(after))
    }

    /// Let the machine run, reporting a breakpoint or watch that fired.
    fn run_settle(&mut self, ticks: u64) -> Option<StopReason> {
        match self.debugger.run(self.emu, ticks) {
            reason @ (StopReason::Breakpoint { .. } | StopReason::Watch { .. }) => Some(reason),
            _ => None,
        }
    }

    /// Run until the ROM reaches its key-accepted address.
    fn run_to_accept_edge(&mut self) -> bool {
        let wanted = fx991_model::address::ROM_KEY_ACCEPTED;
        for _ in 0..DEFAULT_ACCEPT_TICKS {
            if self.emu.pc() == wanted {
                return true;
            }
            self.debugger.run_uninterrupted(self.emu, 1);
        }
        false
    }

    fn address(&self, words: &[&str], index: usize, want: &str) -> Result<u32> {
        let word = words
            .get(index)
            .ok_or_else(|| CommandError::new(want.to_string()))?;
        parse_number(word).ok_or_else(|| CommandError::new(format!("cannot read {word:?}")))
    }

    fn required_number(&self, words: &[&str], index: usize, want: &str) -> Result<u64> {
        let word = words
            .get(index)
            .ok_or_else(|| CommandError::new(want.to_string()))?;
        parse_number(word)
            .map(u64::from)
            .ok_or_else(|| CommandError::new(format!("cannot read {word:?}")))
    }

    fn optional_number(&self, words: &[&str], index: usize, verb: &str) -> Result<Option<u64>> {
        match words.get(index) {
            None => Ok(None),
            Some(word) => parse_number(word)
                .map(|value| Some(u64::from(value)))
                .ok_or_else(|| CommandError::new(format!("{verb} cannot read {word:?}"))),
        }
    }

    fn breakpoint_id(&self, words: &[&str], want: &str) -> Result<crate::breakpoint::BreakpointId> {
        let word = words
            .first()
            .ok_or_else(|| CommandError::new(want.to_string()))?;
        let id = word
            .parse::<u64>()
            .map_err(|_| CommandError::new(format!("{want}, not {word:?}")))?;
        Ok(crate::breakpoint::BreakpointId(id))
    }
}

/// Strip a `#` comment, which a script uses for its own narration.
///
/// `#` is genuinely ambiguous here: it opens a comment the way a shell does, and it
/// prefixes an immediate the way the assembly syntax does.  The rule is therefore
/// positional *and* lexical -- a `#` at the start of a token begins a comment unless
/// what follows could be a number, so:
///
/// ```text
/// set r0 #0x41        #0x41 is an immediate, not a comment
/// set r0 #10          #10 is an immediate
/// s  # one step       a comment
/// # a note            a comment
/// ```
///
/// A comment that *starts* with a digit at the beginning of a token is the one
/// casualty; `# 1st attempt` works because of the space.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'#' {
            continue;
        }
        let at_token_start = index == 0 || bytes[index - 1].is_ascii_whitespace();
        if !at_token_start {
            // Mid-token, as in `mov_r_imm#` -- not a comment and not our business.
            continue;
        }
        let next = bytes.get(index + 1).copied();
        let looks_numeric = matches!(next, Some(b'0'..=b'9') | Some(b'x') | Some(b'X'));
        if !looks_numeric {
            return &line[..index];
        }
    }
    line
}

/// Split a line into its verb and the rest, on the first whitespace.
fn split_verb(line: &str) -> (&str, &str) {
    match line.find(char::is_whitespace) {
        Some(index) => (&line[..index], line[index..].trim_start()),
        None => (line, ""),
    }
}

/// A number in any notation the plans and the command line use.
///
/// `0x`/`0X`/`$` prefix, a trailing `h`, or decimal -- plus the bare hex the
/// project's own notes write addresses in (`D180`, `F042`), accepted only when the
/// run contains a hex letter and so cannot be decimal.
///
/// # The five-digit ambiguity, and why it is not guessed
///
/// `22110` is a valid decimal *and* a valid hex address, and the two are different
/// places (`0x565E` versus `0x22110`).  Guessing would be the worst outcome: a
/// breakpoint would silently land somewhere plausible and the user would believe a
/// wrong result.  So a bare all-digit run is decimal, and the address is written
/// `22110h` or `0x22110` to mean hex.  A hex *letter* is unambiguous, so `121A8`
/// works unqualified.
pub fn parse_number(text: &str) -> Option<u32> {
    // A leading `#` is the assembly notation for an immediate, accepted here so
    // `set r0 #0x41` reads the way the patch commands do.
    let text = text.trim();
    let text = text.strip_prefix('#').unwrap_or(text);
    if text.is_empty() {
        return None;
    }
    if let Some(hex) = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .or_else(|| text.strip_prefix('$'))
    {
        return u32::from_str_radix(hex, 16).ok();
    }
    if let Some(hex) = text.strip_suffix(['h', 'H']) {
        if let Ok(value) = u32::from_str_radix(hex, 16) {
            return Some(value);
        }
    }
    // A run containing a hex letter cannot be decimal, so it is an address: this is
    // what lets `121A8` and `D180` be written the way the notes write them.
    let has_hex_letter = text
        .bytes()
        .any(|byte| matches!(byte, b'a'..=b'f' | b'A'..=b'F'));
    if has_hex_letter && text.len() >= 3 {
        if let Ok(value) = u32::from_str_radix(text, 16) {
            return Some(value);
        }
    }
    if let Ok(value) = text.parse::<u32>() {
        return Some(value);
    }
    // Only now, when it cannot be decimal: a bare hex address like `D180`.
    //
    // Three or more digits, because a two-digit run is overwhelmingly a small
    // decimal.  That leaves five-digit addresses ambiguous -- `22110` is both a
    // decimal and `0x22110` -- and the decimal reading wins, because writing an
    // address as five bare hex digits is unusual and `0x` is always available.
    if text.bytes().all(|byte| byte.is_ascii_hexdigit()) && text.len() >= 3 {
        if let Ok(value) = u32::from_str_radix(text, 16) {
            return Some(value);
        }
    }
    None
}

/// Parse hex bytes, with or without separators.
fn parse_bytes(text: &str) -> Result<Vec<u8>> {
    let cleaned: String = text
        .chars()
        .filter(|character| !character.is_whitespace() && *character != ',')
        .collect();
    if cleaned.is_empty() {
        return Err(CommandError::new("no bytes given"));
    }
    if !cleaned.len().is_multiple_of(2) {
        return Err(CommandError::new(format!(
            "{} hex digits is an odd count; bytes need two each",
            cleaned.len()
        )));
    }
    let mut out = Vec::with_capacity(cleaned.len() / 2);
    for pair in cleaned.as_bytes().chunks(2) {
        let text = std::str::from_utf8(pair).unwrap_or("");
        let byte = u8::from_str_radix(text, 16)
            .map_err(|_| CommandError::new(format!("{text:?} is not a hex byte")))?;
        out.push(byte);
    }
    Ok(out)
}

/// Split a key sequence: `keys 1+2EXE` and `keys 1 + 2 EXE` both mean the same.
///
/// A name that is one character long is a key on its own; anything longer is
/// matched greedily against the key table, so `SHIFT` and `EXE` survive while
/// `1+2` splits into three.
fn tokenize_keys(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let remaining: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    let mut index = 0;
    while index < remaining.len() {
        // Try the longest match first, so a multi-character name is not split.
        let mut matched = None;
        for length in (1..=(remaining.len() - index).min(8)).rev() {
            let candidate: String = remaining[index..index + length].iter().collect();
            if known_key_name(&candidate) {
                matched = Some(candidate);
                break;
            }
        }
        match matched {
            Some(name) => {
                index += name.chars().count();
                out.push(name);
            }
            None => {
                out.push(remaining[index].to_string());
                index += 1;
            }
        }
    }
    out
}

/// `er10` -> `Some(10)`; `e` -> `None`.
fn index_suffix(name: &str, prefix: &str) -> Option<u8> {
    let rest = name.strip_prefix(prefix)?;
    if rest.is_empty() || !rest.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

/// The help text.
pub fn help() -> String {
    "\
stepping
  s [n]              step n instructions (default 1)
  n / over           step over a call
  out                run until the current call returns
  g [BUDGET]         run until a breakpoint, watch, or the budget
  r ADDR             run to an address
  t N                run N ticks, ignoring breakpoints
  runmode run        leave STOP/HALT so instructions run again
breakpoints and watches
  bp ADDR [if EXPR] [once] [log]     set a breakpoint; `log` records without stopping
  bl / bc / be N / bd N              list, clear, enable, disable
  wp ADDR [r|w|rw|x] [if EXPR]       watch memory (default rw)
  wl / wc                            list, clear
registers and memory
  regs / get REG / set REG VALUE / sp VALUE
  m ADDR [LEN] / poke ADDR BYTES / stack [LEN]
views
  context            summary, disassembly around the PC, and registers
  disas [ADDR] [N]   disassemble
  screen / periph / ints / port
patches
  patch ADDR INSTRUCTION   e.g. `patch 10000 POP PC`
  patches / undo ADDR / undoall
snapshots and trace
  snap NAME / restore NAME / snaps / diff A B / diffnow NAME
  trace on [N] / trace off / trace show [N] / trace save PATH
keys and misc
  tap NAME [SETTLE] / keys 1+2EXE   press a key and release it once the ROM takes
                                    it; SETTLE=0 runs nothing after, so a breakpoint
                                    can catch what the key causes
  latch SHIFT [SHIFT ...]           toggle keys down without a finger, and lift them
                                    again on a second `latch`; this is the one case
                                    that needs a key *held* (the self-test's SHIFT+7
                                    through an ON reset), and a latch survives a reset
  fill ADDR LEN [BYTE]              fill memory, default byte 0
  assert EXPR                       fail the script unless EXPR holds
  echo TEXT / # comment / help / q

conditions: registers (r0, er4, xr8, qr0, sp, pc, psw, c, z, s, ov, mie),
memory ([0xD180], [filter]), symbols (input, ans, filter, key_accepted, idle),
and `hits`.  Operators: == != < <= > >= && || ! ( ).  Numbers: 10, 0x0A, 0Ah.
"
    .to_string()
}

/// Whether the key model knows this name, so a key sequence can be split.
///
/// The table is owned by the chipset and needs a machine to query it, but splitting
/// a sequence happens before a key is pressed; the model answers on its own.
fn known_key_name(name: &str) -> bool {
    fx991_model::KeyTable::new().by_name(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxu8_asm::asm;

    fn machine() -> Emu {
        let mut rom = vec![0u8; 0x4_0000];
        rom[0x0000] = 0x00;
        rom[0x0001] = 0xF0;
        let mut program = Vec::new();
        program.extend_from_slice(&asm::mov_r_imm(0, 0x41)); // 0x10000
        program.extend_from_slice(&asm::mov_r_imm(1, 0x42)); // 0x10002
        program.extend_from_slice(&asm::mov_r_imm(2, 0x43)); // 0x10004
        rom[0x1_0000..0x1_0000 + program.len()].copy_from_slice(&program);
        let mut emu = Emu::from_rom(rom);
        {
            let mut interrupts = emu.chipset.interrupts.borrow_mut();
            interrupts.active = [false; fx991_chipset::INT_COUNT];
            interrupts.pending_count = 0;
        }
        emu.chipset.cpu.regs.csr = 1;
        emu.chipset.cpu.regs.pc = 0;
        emu
    }

    /// Run one command against a fresh session, returning the output.
    fn run(emu: &mut Emu, debugger: &mut Debugger, line: &str) -> Result<String> {
        let mut session = Session {
            emu,
            debugger,
            quiet: false,
        };
        match session.execute(line)? {
            CommandResult::Output(text) => Ok(text),
            CommandResult::Silent => Ok(String::new()),
            CommandResult::Quit => Ok("<quit>".to_string()),
        }
    }

    #[test]
    fn stepping_moves_one_instruction_at_a_time() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        let out = run(&mut emu, &mut debugger, "s").unwrap();
        assert!(out.contains("PC 0010002h"), "{out}");
        assert_eq!(emu.reg(0), 0x41);

        run(&mut emu, &mut debugger, "s 2").unwrap();
        assert_eq!(emu.pc(), 0x1_0006);
    }

    #[test]
    fn a_bare_step_defaults_to_one() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "step").unwrap();
        assert_eq!(emu.pc(), 0x1_0002);
    }

    #[test]
    fn a_breakpoint_stops_the_run_and_prints_the_registers() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        let out = run(&mut emu, &mut debugger, "bp 0x10004").unwrap();
        assert!(out.contains("breakpoint 1"), "{out}");

        let out = run(&mut emu, &mut debugger, "g").unwrap();
        assert!(out.contains("stopped"), "{out}");
        assert!(out.contains("PC  0010004h"), "{out}");
        assert_eq!(emu.pc(), 0x1_0004);
    }

    #[test]
    fn a_conditional_breakpoint_takes_the_rest_of_the_line() {
        // The condition contains spaces, so it must not need quoting.
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(
            &mut emu,
            &mut debugger,
            "bp 0x10002 if r0 == 0x41 && z == 0",
        )
        .unwrap();
        let id = crate::breakpoint::BreakpointId(1);
        let condition = debugger.breakpoint(id).unwrap().condition.as_ref().unwrap();
        assert!(!condition.registers().is_empty());

        // And it still works: R0 is 0x41 by then.  A breakpoint stop prints the
        // register view, which pads the label column.
        let out = run(&mut emu, &mut debugger, "g").unwrap();
        assert!(out.contains("PC  0010002h"), "{out}");
    }

    #[test]
    fn a_breakpoint_with_a_condition_that_never_holds_runs_on() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "bp 0x10002 if r0 == 0xFF").unwrap();
        let out = run(&mut emu, &mut debugger, "g").unwrap();
        assert!(out.contains("budget"), "{out}");
    }

    #[test]
    fn once_and_log_flags_are_recognised_in_any_order() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "bp 0x10002 log once").unwrap();
        let breakpoint = debugger
            .breakpoint(crate::breakpoint::BreakpointId(1))
            .unwrap();
        assert!(breakpoint.log_only);
        assert!(breakpoint.once);
    }

    #[test]
    fn enabling_and_disabling_a_breakpoint_by_number() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "bp 0x10002").unwrap();

        let out = run(&mut emu, &mut debugger, "bd 1").unwrap();
        assert!(out.contains("disabled"), "{out}");
        assert!(
            !debugger
                .breakpoint(crate::breakpoint::BreakpointId(1))
                .unwrap()
                .enabled
        );

        let out = run(&mut emu, &mut debugger, "be 1").unwrap();
        assert!(out.contains("enabled"), "{out}");
    }

    #[test]
    fn a_missing_breakpoint_number_is_an_error() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        assert!(run(&mut emu, &mut debugger, "be 9").is_err());
        assert!(run(&mut emu, &mut debugger, "be").is_err());
    }

    #[test]
    fn clearing_breakpoints() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "bp 0x10002").unwrap();
        run(&mut emu, &mut debugger, "bc").unwrap();
        assert_eq!(debugger.breakpoint_count(), 0);
    }

    #[test]
    fn a_watch_defaults_to_both_directions() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "wp D180").unwrap();
        assert_eq!(
            debugger.watch(crate::breakpoint::WatchId(1)).unwrap().kind,
            WatchKind::ReadWrite
        );
        assert_eq!(emu.chipset.bus.watches().len(), 1);
    }

    #[test]
    fn a_watch_takes_a_direction_and_a_condition() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "wp D180 w if r0 == 1").unwrap();
        let watch = debugger.watch(crate::breakpoint::WatchId(1)).unwrap();
        assert_eq!(watch.kind, WatchKind::Write);
        assert!(watch.condition.is_some());
    }

    #[test]
    fn a_watch_stops_a_run_that_writes_it() {
        let mut emu = machine();
        let mut debugger = Debugger::new();

        // Plant a program that writes the watched RAM address: `LEA 0D180h` then
        // `ST R0,[EA]`.  Without planting it there is nothing to write — a store to
        // a ROM address is dropped, so the watch would never fire.
        run(&mut emu, &mut debugger, "patch 0x10000 LEA 0D180h").unwrap();
        run(&mut emu, &mut debugger, "patch 0x10004 ST R0,[EA]").unwrap();
        run(&mut emu, &mut debugger, "wp D180 w").unwrap();

        let out = run(&mut emu, &mut debugger, "g").unwrap();
        assert!(out.contains("watch 1"), "{out}");
        assert!(out.contains("write"), "{out}");
        assert_eq!(emu.peek_quiet(0xD180), 0x00, "R0 was 0 when it stored");
    }

    #[test]
    fn clearing_watches_uninstalls_them() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "wp D180").unwrap();
        run(&mut emu, &mut debugger, "wc").unwrap();
        assert!(emu.chipset.bus.watches().is_empty());
    }

    #[test]
    fn registers_can_be_read_and_written_by_the_names_a_person_uses() {
        let mut emu = machine();
        let mut debugger = Debugger::new();

        run(&mut emu, &mut debugger, "set r0 0x99").unwrap();
        assert_eq!(emu.reg(0), 0x99);

        run(&mut emu, &mut debugger, "set er2 0xBEEF").unwrap();
        assert_eq!(emu.er(2), 0xBEEF);

        let out = run(&mut emu, &mut debugger, "get er2").unwrap();
        assert!(out.contains("BEEF"), "{out}");

        run(&mut emu, &mut debugger, "sp F100").unwrap();
        assert_eq!(emu.sp(), 0xF100);
        assert!(run(&mut emu, &mut debugger, "set nonsense 1").is_err());
        assert!(run(&mut emu, &mut debugger, "get nonsense").is_err());
    }

    #[test]
    fn the_address_notations_all_work() {
        assert_eq!(parse_number("16"), Some(16));
        assert_eq!(parse_number("0x10"), Some(16));
        assert_eq!(parse_number("10h"), Some(16));
        assert_eq!(parse_number("$10"), Some(16));
        // A bare three-or-more digit hex run that is not decimal, as the notes write
        // addresses.
        assert_eq!(parse_number("D180"), Some(0xD180));
        assert_eq!(parse_number("F042"), Some(0xF042));
        // Two digits stay decimal, so a small number is not silently hex.
        assert_eq!(parse_number("10"), Some(10));
        // A hex letter anywhere makes it an address, which is how the notes write
        // `22110` and `121A8`.
        assert_eq!(parse_number("121A8"), Some(0x1_21A8));
        assert_eq!(
            parse_number("22110"),
            Some(22110),
            "all digits stays decimal"
        );
        assert_eq!(parse_number("0x10004"), Some(0x1_0004));
        assert_eq!(parse_number(""), None);
        assert_eq!(parse_number("zzz"), None);
    }

    #[test]
    fn memory_can_be_dumped_and_written() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "poke D180 41 42 43").unwrap();
        let out = run(&mut emu, &mut debugger, "m D180 8").unwrap();
        assert!(out.contains("41 42 43"), "{out}");

        // Separators are optional.
        run(&mut emu, &mut debugger, "poke D190 4142").unwrap();
        assert_eq!(emu.peek_quiet(0xD190), 0x41);

        assert!(
            run(&mut emu, &mut debugger, "poke D180 4").is_err(),
            "odd digits"
        );
        assert!(
            run(&mut emu, &mut debugger, "poke D180").is_err(),
            "no bytes"
        );
    }

    #[test]
    fn the_disassembler_defaults_to_ten_at_the_pc() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        let out = run(&mut emu, &mut debugger, "disas").unwrap();
        assert!(out.contains("MOV"), "{out}");
        assert!(out.lines().count() >= 10, "{out}");

        let out = run(&mut emu, &mut debugger, "disas 0x10004 1").unwrap();
        assert_eq!(out.lines().count(), 1);
    }

    #[test]
    fn the_context_command_shows_everything_at_once() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        let out = run(&mut emu, &mut debugger, "context").unwrap();
        assert!(out.contains("PC  0010000h"), "{out}");
        assert!(out.contains("MOV"), "{out}");
        assert!(out.contains("PSW"), "{out}");
        assert!(out.contains("level"), "{out}");
    }

    #[test]
    fn running_ticks_ignores_breakpoints() {
        // `t` is "let it settle", so it must not stop on a breakpoint in between.
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "bp 0x10002").unwrap();
        run(&mut emu, &mut debugger, "t 3").unwrap();
        assert_eq!(emu.pc(), 0x1_0006);
    }

    #[test]
    fn running_to_an_address() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "r 0x10004").unwrap();
        assert_eq!(emu.pc(), 0x1_0004);
    }

    #[test]
    fn a_stopped_machine_is_reported_as_an_error_not_a_silent_no_op() {
        // The user-visible half of the STOP design decision: `s` must say why
        // nothing happened rather than looking like a broken stepper.
        let mut emu = machine();
        let mut debugger = Debugger::new();
        emu.chipset.interrupts.borrow_mut().stop();
        let error = run(&mut emu, &mut debugger, "s").unwrap_err();
        assert!(error.message.contains("runmode run"), "{error}");

        // And the escape hatch works.
        run(&mut emu, &mut debugger, "runmode run").unwrap();
        run(&mut emu, &mut debugger, "s").unwrap();
        assert_eq!(emu.pc(), 0x1_0002);
    }

    #[test]
    fn a_patch_is_applied_listed_and_undone() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        let out = run(&mut emu, &mut debugger, "patch 0x10000 POP PC").unwrap();
        assert!(out.contains("8E F2"), "{out}");
        assert!(out.contains("was 41 00"), "the ROM bytes: {out}");

        // The patch is what runs now.
        run(&mut emu, &mut debugger, "s").unwrap();
        assert_eq!(
            emu.pc(),
            0x0_0000,
            "POP PC took a return address off the stack"
        );

        let out = run(&mut emu, &mut debugger, "patches").unwrap();
        assert!(out.contains("POP PC"), "{out}");

        // Undo restores the ROM's own instruction.
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "patch 0x10000 POP PC").unwrap();
        run(&mut emu, &mut debugger, "undo 0x10000").unwrap();
        run(&mut emu, &mut debugger, "s").unwrap();
        assert_eq!(emu.pc(), 0x1_0002, "the original MOV ran");
    }

    #[test]
    fn undoall_removes_every_patch() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "patch 0x10000 POP PC").unwrap();
        run(&mut emu, &mut debugger, "patch 0x10002 POP PC").unwrap();
        let out = run(&mut emu, &mut debugger, "undoall").unwrap();
        assert!(out.contains('2'), "{out}");
        assert!(debugger.patches().is_empty());
    }

    #[test]
    fn the_disassembly_view_shows_a_patch_which_is_the_point() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "patch 0x10000 POP PC").unwrap();
        let out = run(&mut emu, &mut debugger, "disas 0x10000 1").unwrap();
        assert!(out.contains("POP"), "{out}");
        assert!(out.contains("8E F2"), "{out}");
    }

    #[test]
    fn an_unassemblable_patch_is_rejected_with_the_supported_list() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        let error = run(&mut emu, &mut debugger, "patch 0x10000 FROB R0").unwrap_err();
        assert!(error.message.contains("no assembler"), "{error}");
        assert!(error.message.contains("NOP"), "{error}");
        assert!(run(&mut emu, &mut debugger, "patch 0x10000").is_err());
    }

    #[test]
    fn a_snapshot_can_be_restored_and_diffed() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "snap start").unwrap();
        run(&mut emu, &mut debugger, "s 2").unwrap();
        run(&mut emu, &mut debugger, "snap later").unwrap();

        let out = run(&mut emu, &mut debugger, "diff start later").unwrap();
        assert!(out.contains("R0"), "{out}");

        run(&mut emu, &mut debugger, "restore start").unwrap();
        assert_eq!(emu.pc(), 0x1_0000);
        assert_eq!(emu.reg(0), 0, "rolled back");

        assert!(run(&mut emu, &mut debugger, "restore nothing").is_err());
        assert!(run(&mut emu, &mut debugger, "diff start nothing").is_err());
    }

    #[test]
    fn a_snapshot_can_be_diffed_against_now() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "snap here").unwrap();
        run(&mut emu, &mut debugger, "s").unwrap();
        let out = run(&mut emu, &mut debugger, "diffnow here").unwrap();
        assert!(out.contains("PC"), "{out}");
        assert!(out.contains("0010000h"), "{out}");
    }

    #[test]
    fn the_trace_can_be_started_shown_saved_and_stopped() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        let out = run(&mut emu, &mut debugger, "trace on 8").unwrap();
        assert!(out.contains('8'), "{out}");

        run(&mut emu, &mut debugger, "s 3").unwrap();
        let out = run(&mut emu, &mut debugger, "trace show").unwrap();
        assert!(out.contains("MOV"), "{out}");
        assert!(out.contains("R0=00>41"), "{out}");

        let path = std::env::temp_dir().join("fx991-dbg-trace-test.csv");
        let path_text = path.to_string_lossy().to_string();
        run(&mut emu, &mut debugger, &format!("trace save {path_text}")).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.starts_with("tick,pc,handler"), "{written}");
        assert!(written.contains("OP_MOV"), "{written}");
        let _ = std::fs::remove_file(&path);

        let out = run(&mut emu, &mut debugger, "trace off").unwrap();
        assert!(out.contains("3 entries"), "{out}");
        assert!(run(&mut emu, &mut debugger, "trace off").is_err());
    }

    #[test]
    fn trace_show_says_so_when_no_trace_is_running() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        let out = run(&mut emu, &mut debugger, "trace show").unwrap();
        assert!(out.contains("no trace"), "{out}");
    }

    #[test]
    fn a_key_name_wins_over_a_matrix_code() {
        // `4` must resolve to the key printed `4` (code 01h), not to the number 4
        // read as a matrix code (which is `x^2`).  Getting this backwards silently
        // presses a different key, so the order is pinned.  `latch` is used because
        // it needs no key-accept loop, and it echoes the code it resolved.
        let mut emu = machine();
        let mut debugger = Debugger::new();

        let out = run(&mut emu, &mut debugger, "latch 4").unwrap();
        assert!(out.contains("01h"), "the 4 key is code 01h: {out}");
        run(&mut emu, &mut debugger, "latch 4").unwrap(); // unlatch

        // A code has to be written so it cannot be a name.
        let out = run(&mut emu, &mut debugger, "latch 0x04").unwrap();
        assert!(out.contains("04h"), "{out}");
        run(&mut emu, &mut debugger, "latch 0x04").unwrap();
        let out = run(&mut emu, &mut debugger, "latch 04h").unwrap();
        assert!(out.contains("04h"), "{out}");
        run(&mut emu, &mut debugger, "latch 04h").unwrap();

        // A bare number that is not a name is refused rather than guessed at.
        let error = run(&mut emu, &mut debugger, "latch 77").unwrap_err();
        assert!(error.message.contains("0x05"), "{error}");
    }

    #[test]
    fn latch_toggles_a_key_down_and_up() {
        // The self-test's SHIFT+7 is the case that genuinely needs a key held, and
        // a latch is how a script expresses "held" without a finger.
        let mut emu = machine();
        let mut debugger = Debugger::new();

        let out = run(&mut emu, &mut debugger, "latch SHIFT").unwrap();
        assert!(out.contains("on"), "{out}");
        assert!(emu.chipset.keyboard.borrow().is_pressed(0x07));
        assert!(emu.chipset.keyboard.borrow().is_stuck(0x07));

        let out = run(&mut emu, &mut debugger, "latch SHIFT").unwrap();
        assert!(out.contains("off"), "{out}");
        assert!(!emu.chipset.keyboard.borrow().is_pressed(0x07));
        assert!(!emu.chipset.keyboard.borrow().is_stuck(0x07));
    }

    #[test]
    fn a_latched_key_survives_a_release_all() {
        // `release(None)` deliberately spares latched keys -- the self-test needs a
        // reset not to lift the finger -- so a latch is only cleared by its own
        // toggle.  This pins that a latch cannot be swept away by accident.
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "latch SHIFT").unwrap();
        run(&mut emu, &mut debugger, "latch 7").unwrap();

        emu.chipset.keyboard.borrow_mut().release(None);
        let keyboard = emu.chipset.keyboard.borrow();
        assert!(keyboard.is_stuck(0x07), "the latch held");
        assert!(keyboard.is_stuck(0x02), "the latch held");
    }

    #[test]
    fn a_latched_key_holds_a_second_key_that_was_only_pressed() {
        // The mixed case a key sequence needs: latch the modifier, then tap the key
        // it modifies.  The modifier is still down afterwards, which is what makes
        // `latch SHIFT`, `tap 8` behave like a person pressing SHIFT then 8.
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "latch SHIFT").unwrap();

        // `press` without the accept-edge wait would hang on this scratch ROM, so
        // the plain press is done through the keyboard directly.
        emu.chipset.keyboard.borrow_mut().press(0x12);
        assert!(emu.chipset.keyboard.borrow().is_pressed(0x12));
        emu.chipset.keyboard.borrow_mut().release(Some(0x12));

        let keyboard = emu.chipset.keyboard.borrow();
        assert!(keyboard.is_pressed(0x07), "SHIFT stayed down");
        assert!(!keyboard.is_pressed(0x12), "8 came back up");
    }

    #[test]
    fn an_unknown_key_name_is_an_error() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        assert!(run(&mut emu, &mut debugger, "latch NOPE").is_err());
    }

    #[test]
    fn a_key_sequence_splits_into_the_keys_the_model_knows() {
        // `1+2EXE` must become four keys, and a long name must not be split.
        let keys = tokenize_keys("1+2EXE");
        assert_eq!(keys, vec!["1", "+", "2", "EXE"]);

        let keys = tokenize_keys("SHIFT");
        assert_eq!(keys, vec!["SHIFT"], "a long name survives");

        let keys = tokenize_keys("1 + 2 EXE");
        assert_eq!(keys, vec!["1", "+", "2", "EXE"], "spaces are ignored");
    }

    #[test]
    fn a_key_sequence_needs_the_rom_to_accept_each_key() {
        // On a scratch ROM there is no key-accept loop, so `keys` reports that
        // rather than pretending the keys landed.  That failure mode is the point:
        // a silently dropped key is how a script reports success for a run that
        // proved nothing.
        let mut emu = machine();
        let mut debugger = Debugger::new();
        let error = run(&mut emu, &mut debugger, "keys 1+2").unwrap_err();
        assert!(error.message.contains("never accepted"), "{error}");
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        assert_eq!(run(&mut emu, &mut debugger, "").unwrap(), "");
        assert_eq!(run(&mut emu, &mut debugger, "   ").unwrap(), "");
        assert_eq!(run(&mut emu, &mut debugger, "# a note").unwrap(), "");
        assert_eq!(run(&mut emu, &mut debugger, "  # indented").unwrap(), "");
        // A comment after a command is stripped, and the command still runs.
        let out = run(&mut emu, &mut debugger, "s  # one step").unwrap();
        assert!(out.contains("PC 0010002h"), "{out}");
    }

    #[test]
    fn a_comment_does_not_eat_an_immediate() {
        // `#0x41` follows a non-space, so it is an immediate rather than a comment.
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "set r0 #0x41").unwrap();
        assert_eq!(emu.reg(0), 0x41);
    }

    #[test]
    fn echo_prints_its_argument_and_help_prints_itself() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        let out = run(&mut emu, &mut debugger, "echo RESULT: measured 42").unwrap();
        assert_eq!(out, "RESULT: measured 42\n");

        let out = run(&mut emu, &mut debugger, "help").unwrap();
        assert!(out.contains("breakpoints"), "{out}");
        assert!(out.contains("conditions:"), "{out}");
    }

    #[test]
    fn an_assert_that_holds_is_silent_and_one_that_fails_stops_the_script() {
        // This is what makes a script evidence: a wrong claim has to fail.
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "set r0 0x41").unwrap();

        assert_eq!(
            run(&mut emu, &mut debugger, "assert r0 == 0x41").unwrap(),
            ""
        );

        let error = run(&mut emu, &mut debugger, "assert r0 == 0x42").unwrap_err();
        assert!(error.message.contains("assert failed"), "{error}");

        // A syntax error is an error too, not a silent pass.
        assert!(run(&mut emu, &mut debugger, "assert r0 ==").is_err());
        assert!(run(&mut emu, &mut debugger, "assert").is_err());
    }

    #[test]
    fn fill_writes_a_run_of_bytes() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        run(&mut emu, &mut debugger, "fill D180 4 0x31").unwrap();
        assert_eq!(emu.peek_quiet_block(0xD180, 4), vec![0x31; 4]);

        // The default is zero, which is what clearing an area wants.
        run(&mut emu, &mut debugger, "fill D180 4").unwrap();
        assert_eq!(emu.peek_quiet_block(0xD180, 4), vec![0; 4]);
    }

    #[test]
    fn quitting_is_reported() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        assert_eq!(run(&mut emu, &mut debugger, "q").unwrap(), "<quit>");
    }

    #[test]
    fn an_unknown_command_suggests_help() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        let error = run(&mut emu, &mut debugger, "frobnicate").unwrap_err();
        assert!(error.message.contains("help"), "{error}");
    }

    #[test]
    fn the_views_all_produce_something_without_panicking() {
        let mut emu = machine();
        let mut debugger = Debugger::new();
        for line in [
            "regs", "stack", "screen", "periph", "ints", "port", "bl", "wl", "snaps", "patches",
            "context",
        ] {
            run(&mut emu, &mut debugger, line)
                .unwrap_or_else(|error| panic!("{line} failed: {error}"));
        }
    }
}
