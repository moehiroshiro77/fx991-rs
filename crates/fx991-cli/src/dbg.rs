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

//! `emu dbg` -- the interactive and scripted debugger front end.
//!
//! Two ways in, one engine:
//!
//! * `emu dbg --script FILE` reads commands from a file and stops at the first
//!   failure with a non-zero exit code.  This is how an experiment becomes
//!   reproducible: the script *is* the record.
//! * `emu dbg` reads commands from stdin.  Piped in from a file it behaves like
//!   `--script`; typed at a terminal it prompts.
//!
//! The command language itself lives in `fx991_dbg::command`, so both modes and the
//! library tests go through the same parser.
//!
//! # No line editing
//!
//! Input is read a line at a time with no history and no editing keys.  That is the
//! price of this workspace's zero-dependency policy; the engine and the command
//! language do not know or care, so a terminal library can be added later without
//! touching them.

use std::io::{BufRead, Write};

use fx991::Emu;
use fx991_dbg::command::{CommandResult, Session};
use fx991_dbg::Debugger;

/// How `emu dbg` was asked to run.
pub struct Options {
    /// A script to run, or `None` to read stdin.
    pub script: Option<String>,
    /// Whether to print a banner and a prompt.
    pub interactive: bool,
    /// Run this many ticks before accepting commands, so the ROM is at its idle
    /// loop rather than mid-boot.
    pub boot: Option<u64>,
}

/// Where a script's commands come from.
enum Source {
    File(String),
    Stdin,
}

/// Run the debugger.
pub fn run(emu: &mut Emu, options: &Options) -> Result<(), String> {
    let mut debugger = Debugger::new();

    if let Some(ticks) = options.boot {
        // The ROM spends its first few hundred thousand ticks booting and clearing
        // RAM; starting there means every first command is about the boot code,
        // which is rarely what is wanted.
        eprintln!("booting: running {ticks} ticks");
        emu.run(ticks);
    }

    let source = match &options.script {
        Some(path) => Source::File(path.clone()),
        None => Source::Stdin,
    };

    if options.interactive {
        print_banner(emu);
    }

    match source {
        Source::File(path) => run_script(emu, &mut debugger, &path),
        Source::Stdin => run_stdin(emu, &mut debugger, options.interactive),
    }
}

fn print_banner(emu: &mut Emu) {
    println!("fx991 dbg -- nX-U8 debugger for the fx-991CN X");
    println!("ROM   {} bytes", emu.chipset.bus.rom().len());
    println!("PC    {:07X}h", emu.pc());
    println!("type `help` for commands, `context` for the machine, `q` to quit");
    println!();
}

fn run_script(emu: &mut Emu, debugger: &mut Debugger, path: &str) -> Result<(), String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("cannot read {path}: {error}"))?;
    let mut session = Session {
        emu,
        debugger,
        quiet: false,
    };
    execute_lines(&mut session, text.lines(), Some(path), false)
}

fn run_stdin(emu: &mut Emu, debugger: &mut Debugger, interactive: bool) -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut session = Session {
        emu,
        debugger,
        quiet: !interactive,
    };

    // A terminal gets a prompt per line; a pipe gets none, so the output stays
    // parseable.
    let mut lines: Vec<String> = Vec::new();
    let mut handle = stdin.lock();
    loop {
        if interactive {
            print!("dbg> ");
            let _ = std::io::stdout().flush();
        }
        let mut line = String::new();
        match handle.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => lines.push(line),
            Err(error) => return Err(format!("cannot read input: {error}")),
        }
        // Executed one at a time in interactive mode so output interleaves with the
        // prompt, and buffered otherwise.
        if interactive {
            let result = execute_lines(&mut session, lines.drain(..), None, false);
            if let Err(message) = result {
                eprintln!("error: {message}");
            }
        }
    }
    execute_lines(&mut session, lines.drain(..), None, false)
}

/// Run commands, stopping at the first failure.
///
/// A script stops rather than continuing because a script that continued past a
/// failed assertion would report success for a run that proved nothing -- and these
/// scripts exist to prove things.
fn execute_lines(
    session: &mut Session<'_>,
    lines: impl Iterator<Item = impl AsRef<str>>,
    origin: Option<&str>,
    echo: bool,
) -> Result<(), String> {
    for (number, line) in lines.enumerate() {
        let line = line.as_ref();
        if echo {
            println!("{}", line.trim_end());
        }
        match session.execute(line) {
            Ok(CommandResult::Output(text)) => {
                if !text.is_empty() {
                    print!("{text}");
                }
            }
            Ok(CommandResult::Silent) => {}
            Ok(CommandResult::Quit) => return Ok(()),
            Err(error) => {
                return Err(match origin {
                    Some(path) => format!("{path}:{}: {error}", number + 1),
                    None => format!("line {}: {error}", number + 1),
                });
            }
        }
    }
    Ok(())
}

/// The ROM a debug session needs, loaded from a path.
pub fn load_rom(path: &str) -> Result<Emu, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("cannot read {path}: {error}"))?;
    Ok(Emu::from_rom(bytes))
}
