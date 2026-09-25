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

//! `emu` -- command line front end for the fx-991CN X emulator.
//!
//! The subcommands mirror, and every one of them is a thin wrapper over
//! the library so a shell one-liner and a Rust program do the same thing.
//!
//! `text
//! emu boot [--steps N]         cold-boot and print the reset state
//! emu run --until ADDR         run to an address, with a tick budget
//! emu disas ADDR [--count N]   disassemble; the ROM by default, --from-listing
//!                              for the pre-generated text
//! emu listing                  summarise the disassembly listing
//! emu screen PATH              render the 0xF800 buffer to a PNG
//! emu render PATH              render the whole face (skin + LCD) to a PNG
//! emu key KEYS [--enter]       press keys and dump the input area
//! emu calc EXPR                evaluate an expression and print the answer
//! emu dump ADDR [LEN]          hex dump of the data space
//! `

use std::process::ExitCode;

mod dbg;

use fx991::listing::Listing;
use fx991::{Calculator, Emu};
use nxu8_core::Memory;

const DEFAULT_ROM: &str = "data/rom_verF.bin";
const DEFAULT_LISTING: &str = "data/_disas_verF.txt";
const DEFAULT_SKIN: &str = "data/skin.rgba";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }

    let command = args[0].as_str();
    let rest = &args[1..];
    let result = match command {
        "boot" => cmd_boot(rest),
        "run" => cmd_run(rest),
        "disas" => cmd_disas(rest),
        "listing" => cmd_listing(rest),
        "screen" => cmd_screen(rest),
        "render" => cmd_render(rest),
        "key" => cmd_key(rest),
        "calc" => cmd_calc(rest),
        "dump" => cmd_dump(rest),
        "dbg" => cmd_dbg(rest),
        other => {
            eprintln!("unknown command {other:?}; try --help");
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

const HELP: &str = "\
emu -- fx-991CN X (VerF) emulator

usage: emu <command> [options]

Options go after the command: `emu calc 1+2 --rom custom.bin`, not
`emu --rom custom.bin calc 1+2`.

commands:
  boot [--steps N]              cold-boot and print the reset state
  run --until ADDR [--max N]    run to a physical address
  disas ADDR [--count N]        disassemble, decoding the ROM by default
  listing [--limit N]           summarise the text listing
  screen PATH [--scale N]       render the display buffer to a PNG
  render PATH                   render the whole face (skin + LCD) to a PNG
  key KEYS [--enter]            press keys, then dump the input area
  calc EXPR                     evaluate an expression, e.g. \"1+2*3\"
  dump ADDR [LEN]               hex dump of the data space
  dbg [--script FILE]           interactive or scripted debugger

options:
  --rom PATH                    ROM image      (default data/rom_verF.bin)
  --skin PATH                   face texture   (default data/skin.rgba)
  --listing PATH                text listing   (default data/_disas_verF.txt)
  --run N                       ticks to run first (screen, render, dump)
  --keys KEYS                   keys to press first (render)
  --enter                       press EXE after --keys (render)
  --variables, --display        extra output for calc
  --screen PATH                 also render the display (calc)
  --script FILE                 commands for `dbg` (default: stdin)
  --boot N                      ticks to run before `dbg` accepts commands
  --listing                     `disas` reads the text listing instead of the ROM
";

// ------------------------------------------------------------------ arg helpers

struct Args<'a> {
    positional: Vec<&'a str>,
    flags: Vec<(&'a str, Option<&'a str>)>,
    rom: String,
    skin: String,
    listing: String,
}

impl<'a> Args<'a> {
    fn parse(raw: &'a [String]) -> Result<Self, String> {
        let mut positional = Vec::new();
        let mut flags: Vec<(&str, Option<&str>)> = Vec::new();
        let mut rom = DEFAULT_ROM.to_string();
        let mut skin = DEFAULT_SKIN.to_string();
        let mut listing = DEFAULT_LISTING.to_string();

        let mut index = 0;
        while index < raw.len() {
            let token = raw[index].as_str();
            index += 1;

            if let Some(name) = token.strip_prefix("--") {
                // A flag takes the next token as its value unless that token is
                // itself a flag or there is none left, so `--variables` works bare.
                let value = raw
                    .get(index)
                    .filter(|next| !next.starts_with("--"))
                    .map(|next| {
                        index += 1;
                        next.as_str()
                    });
                if name == "rom" {
                    rom = value.ok_or("--rom needs a path")?.to_string();
                    continue;
                }
                if name == "skin" {
                    skin = value.ok_or("--skin needs a path")?.to_string();
                    continue;
                }
                if name == "listing" {
                    listing = value.ok_or("--listing needs a path")?.to_string();
                    continue;
                }
                flags.push((name, value));
            } else {
                positional.push(token);
            }
        }
        Ok(Self {
            positional,
            flags,
            rom,
            skin,
            listing,
        })
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|(key, _)| *key == name)
    }

    fn skin(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.skin)
    }

    fn listing_path(&self) -> &str {
        &self.listing
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| *value)
    }

    fn number(&self, name: &str) -> Result<Option<u64>, String> {
        match self.value(name) {
            None => Ok(None),
            Some(text) => parse_number(text)
                .map(Some)
                .ok_or_else(|| format!("--{name} wants a number, got {text:?}")),
        }
    }
}

/// Accept `123` and `0x7B`, so addresses can be written the way the plans do.
fn parse_number(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok();
    }
    text.parse::<u64>().ok()
}

// -------------------------------------------------------------------- commands

fn cmd_boot(raw: &[String]) -> Result<(), String> {
    let args = Args::parse(raw)?;
    let steps = args.number("steps")?.unwrap_or(0);

    let mut emu = load(&args)?;
    emu.run(steps);

    let regs = &emu.chipset.cpu.regs;
    println!("SP=0x{:04X} CSR={} PC=0x{:04X}", regs.sp, regs.csr, regs.pc);
    println!("physical PC = {:#07x}  (ticks={})", emu.pc(), emu.ticks);
    if steps == 0 {
        // A bare reset leaves PC/CSR alone until the reset interrupt is dispatched on
        // the first tick, so show what the vector table says instead.
        let entry = emu.chipset.bus.read_code(2);
        println!("reset vector word@0x000002 = 0x{entry:04X}");
    }
    report_faults(&emu);
    Ok(())
}

fn cmd_run(raw: &[String]) -> Result<(), String> {
    let args = Args::parse(raw)?;
    let until = args.number("until")?.ok_or("run needs --until ADDR")? as u32;
    let budget = args.number("max")?.unwrap_or(10_000_000);

    let mut emu = load(&args)?;
    match emu.run_until(until, budget) {
        Ok(()) => {
            let regs = &emu.chipset.cpu.regs;
            println!(
                "reached CSR={} PC=0x{:04X} SP=0x{:04X} ticks={}",
                regs.csr, regs.pc, regs.sp, emu.ticks
            );
        }
        Err(err) => {
            println!("stopped: {err}");
            println!("  state: pc={:#07x} sp=0x{:04X}", emu.pc(), emu.sp());
        }
    }
    report_faults(&emu);
    Ok(())
}

fn cmd_disas(raw: &[String]) -> Result<(), String> {
    let args = Args::parse(raw)?;
    let address = args
        .positional
        .first()
        .and_then(|text| parse_number(text))
        .ok_or("disas needs an address")? as u32;
    let count = args.number("count")?.unwrap_or(10) as usize;

    // The default is to decode the ROM: that is what a debugger does, and it is what
    // makes a patch visible.  `--from-listing` reads the pre-generated text instead,
    // which is the reference this decoder is tested against and what a static
    // analysis pass wants.
    if args.flag("from-listing") {
        let listing = load_listing(args.listing_path())?;
        print!("{}", listing.disassemble(address, count));
        return Ok(());
    }

    let mut emu = load(&args)?;
    let mut read = |at: u32| emu.chipset.bus.read_code_quiet(at);
    print!(
        "{}",
        nxu8_asm::disasm::disassemble(&mut read, address, count)
    );
    Ok(())
}

fn cmd_listing(raw: &[String]) -> Result<(), String> {
    let args = Args::parse(raw)?;
    let limit = args.number("limit")?.unwrap_or(20) as usize;

    let listing = load_listing(args.listing_path())?;
    println!("{}: {} instructions", args.listing_path(), listing.len());
    for entry in listing.entries().iter().take(limit) {
        let hex: Vec<String> = entry.bytes.iter().map(|b| format!("{b:02X}")).collect();
        println!(
            "{:#06x}  {:<12}  {}",
            entry.address,
            hex.join(" "),
            entry.text
        );
    }
    Ok(())
}

fn cmd_dbg(raw: &[String]) -> Result<(), String> {
    let args = Args::parse(raw)?;
    // `--rom` is required here rather than defaulted, because a debugger without the
    // right image would silently report nonsense.
    let rom = args.value("rom").unwrap_or(DEFAULT_ROM);
    let mut emu = dbg::load_rom(rom)?;
    let options = dbg::Options {
        script: args.value("script").map(str::to_string),
        // A prompt only makes sense on a terminal; a piped stdin gets no banner.
        interactive: args.value("script").is_none()
            && std::io::IsTerminal::is_terminal(&std::io::stdin()),
        boot: args.number("boot")?,
    };
    dbg::run(&mut emu, &options)
}

fn cmd_screen(raw: &[String]) -> Result<(), String> {
    let args = Args::parse(raw)?;
    let path = args
        .positional
        .first()
        .ok_or("screen needs an output path")?
        .to_string();
    let scale = args.number("scale")?.unwrap_or(4) as usize;
    let run_ticks = args.number("run")?.unwrap_or(0);

    let mut emu = load(&args)?;
    emu.run(run_ticks);
    let png = emu.chipset.screen.borrow().to_png(scale);
    std::fs::write(&path, png).map_err(|err| format!("could not write {path}: {err}"))?;
    println!("wrote {path} ({} ticks)", emu.ticks);
    Ok(())
}

fn cmd_render(raw: &[String]) -> Result<(), String> {
    let args = Args::parse(raw)?;
    let path = args
        .positional
        .first()
        .ok_or("render needs an output path")?
        .to_string();
    let run_ticks = args.number("run")?.unwrap_or(0);
    let keys = args.value("keys").map(str::to_string);

    let mut calculator = load_calculator(&args)?;
    if let Some(sequence) = keys {
        calculator.press(&sequence).map_err(|err| err.to_string())?;
    }
    if args.flag("enter") {
        calculator.tap("EXE").map_err(|err| err.to_string())?;
    }
    calculator.emu.run(run_ticks);

    let skin_path = args.skin();
    let skin = fx991_ui::Skin::from_file(&skin_path)
        .map_err(|err| format!("could not load {}: {err}", skin_path.display()))?;
    let frame = fx991_ui::Renderer::new(skin).render(&mut calculator.emu.chipset);
    std::fs::write(&path, frame.to_png())
        .map_err(|err| format!("could not write {path}: {err}"))?;
    println!(
        "wrote {path} ({}x{}, {} ticks, digest {:#018x})",
        frame.width,
        frame.height,
        calculator.emu.ticks,
        frame.digest()
    );
    Ok(())
}

fn cmd_key(raw: &[String]) -> Result<(), String> {
    let args = Args::parse(raw)?;
    if args.positional.is_empty() {
        return Err("key needs at least one key name".to_string());
    }

    let mut calculator = load_calculator(&args)?;
    // Join the positional arguments so both `key 1 + 2` and `key 1+2` work.
    let sequence = args.positional.join("");
    calculator.press(&sequence).map_err(|err| err.to_string())?;
    if args.flag("enter") {
        calculator.tap("EXE").map_err(|err| err.to_string())?;
    }

    println!("input  : {:?}", calculator.input());
    let bytes = calculator.input_bytes(8);
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
    println!("0xD180 : {}", hex.join(" "));
    if args.flag("enter") {
        let raw_answer = calculator.answer_bytes();
        let hex: Vec<String> = raw_answer.iter().map(|b| format!("{b:02X}")).collect();
        println!(
            "answer : {}   ({})",
            calculator.answer_text(),
            hex.join(" ")
        );
    }
    Ok(())
}

fn cmd_calc(raw: &[String]) -> Result<(), String> {
    let args = Args::parse(raw)?;
    let expression = args
        .positional
        .first()
        .ok_or("calc needs an expression, e.g. \"1+2\"")?
        .to_string();

    let mut calculator = load_calculator(&args)?;
    let answer = calculator
        .eval(&expression)
        .map_err(|err| err.to_string())?;
    println!("{answer}");

    if args.flag("variables") {
        for (name, text) in calculator.variables() {
            println!("{name:>7} = {text}");
        }
    }
    if args.flag("display") {
        for row in calculator.display() {
            println!("{row}");
        }
    }
    if let Some(path) = args.value("screen") {
        let scale = args.number("scale")?.unwrap_or(4) as usize;
        std::fs::write(path, calculator.screen_png(scale))
            .map_err(|err| format!("could not write {path}: {err}"))?;
    }
    Ok(())
}

fn cmd_dump(raw: &[String]) -> Result<(), String> {
    let args = Args::parse(raw)?;
    let address = args
        .positional
        .first()
        .and_then(|text| parse_number(text))
        .ok_or("dump needs an address")? as u32;
    let length = match args.positional.get(1) {
        Some(text) => parse_number(text).ok_or("dump length wants a number")? as usize,
        None => 0x40,
    };
    let run_ticks = args.number("run")?.unwrap_or(0);

    let mut emu = load(&args)?;
    emu.run(run_ticks);
    let data = emu.peek_block(address, length);
    for (row, chunk) in data.chunks(16).enumerate() {
        let base = address + (row * 16) as u32;
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02X}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|b| {
                if (0x20..0x7F).contains(b) {
                    *b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!("{base:05X}  {:<47}  {ascii}", hex.join(" "));
    }
    Ok(())
}

// --------------------------------------------------------------------- helpers

fn load(args: &Args) -> Result<Emu, String> {
    Emu::load_rom(&args.rom).map_err(|err| format!("could not read {}: {err}", args.rom))
}

fn load_calculator(args: &Args) -> Result<Calculator, String> {
    Calculator::from_rom_path(&args.rom)
        .map_err(|err| format!("could not boot {}: {err}", args.rom))
}

fn load_listing(path: &str) -> Result<Listing, String> {
    let text =
        std::fs::read_to_string(path).map_err(|err| format!("could not read {path}: {err}"))?;
    Ok(Listing::parse(&text))
}

fn report_faults(emu: &Emu) {
    if emu.chipset.bus.fault_count > 0 {
        let faults = emu.chipset.bus.fault_kinds();
        println!(
            "bus faults: {} ({} distinct)",
            emu.chipset.bus.fault_count,
            faults.len()
        );
        for fault in faults.iter().take(5) {
            println!("  {} at {:#07x}", fault.kind.label(), fault.address);
        }
    }
}
