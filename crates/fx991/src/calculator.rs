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

//! A calculator-shaped front end: press keys, read the display, read the answer.
//!
//! The emulator is faithful but slow to converse with: the ROM sleeps in a STOP
//! state, wakes on a timer, arms its key filter, scans the matrix only inside a
//! key-wait loop, and only then writes a character into the input area.  Driving
//! that by hand means knowing the boot point and how long each step takes.
//!
//! `Calculator` encapsulates exactly that:
//!
//! ```no_run
//! use fx991::Calculator;
//!
//! let mut calc = Calculator::from_rom_path("data/rom_verF.bin").unwrap();
//! calc.press("1+2").unwrap();
//! assert_eq!(calc.input(), "1+2");
//! calc.press("EXE").unwrap();
//! assert_eq!(calc.answer_text(), "3");
//! ```
//!
//! What it does *not* hide: the ROM only accepts a key press while it is inside its
//! key-wait loop with the filter armed, so [`Calculator::press`] advances the machine
//! until the key is taken, or until a budget runs out.

use fx991_model::address;
use fx991_model::variable;

use crate::emu::Emu;

/// The ROM never got into a state where it could accept a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotReady {
    /// What was being attempted.
    pub action: String,
    /// How many ticks were spent trying.
    pub budget: u64,
    /// Where the machine was when it gave up.
    pub at: u32,
}

impl std::fmt::Display for NotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: gave up after {} ticks at {:#07x}",
            self.action, self.budget, self.at
        )
    }
}

impl std::error::Error for NotReady {}

/// Default boot budget: the ROM needs ~456k ticks to reach its key-wait loop, and
/// that number moves a little with RAM contents, so leave plenty of headroom.
pub const DEFAULT_BOOT_BUDGET: u64 = 8_000_000;
/// Default budget for the ROM to accept a key once one is held.
pub const DEFAULT_ACCEPT_BUDGET: u64 = 4_000_000;
/// How long to let the ROM's handler run after it has accepted a key.
pub const DEFAULT_SETTLE: u64 = 120_000;

/// The machine, driven the way a person drives it.
pub struct Calculator {
    /// The underlying emulator, for anything this front end does not cover.
    pub emu: Emu,
}

impl Calculator {
    /// Boot a ROM into a state where it is ready for input.
    pub fn from_rom_path(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let emu = Emu::load_rom(path)?;
        let mut calc = Self { emu };
        calc.boot()?;
        Ok(calc)
    }

    /// Boot from ROM bytes.
    pub fn from_rom(rom: Vec<u8>) -> Result<Self, NotReady> {
        let mut calc = Self {
            emu: Emu::from_rom(rom),
        };
        calc.boot()?;
        Ok(calc)
    }

    /// Advance until the ROM arms its key filter, which is the observable "ready"
    /// edge: a real calculator is ready as soon as it powers on, but this ROM takes
    /// a few hundred thousand instructions to finish initialising.
    pub fn boot(&mut self) -> Result<(), NotReady> {
        // Start from a clean machine, then wait for the filter edge.
        self.emu.reset();
        if self.emu.chipset.ready_for_key() {
            return Ok(());
        }
        for _ in 0..DEFAULT_BOOT_BUDGET {
            self.emu.step();
            if self.emu.chipset.ready_for_key() {
                return Ok(());
            }
        }
        Err(NotReady {
            action: "booting to the key-wait loop".to_string(),
            budget: DEFAULT_BOOT_BUDGET,
            at: self.emu.pc(),
        })
    }

    /// Press and release one key by matrix code, waiting for the ROM to take it.
    pub fn tap_code(&mut self, code: u8) -> Result<(), NotReady> {
        self.tap_code_with(code, DEFAULT_ACCEPT_BUDGET, DEFAULT_SETTLE)
    }

    /// [`Calculator::tap_code`] with explicit budgets.
    pub fn tap_code_with(
        &mut self,
        code: u8,
        accept_budget: u64,
        settle: u64,
    ) -> Result<(), NotReady> {
        let power = self.emu.chipset.keyboard.borrow().is_power_key(code);
        if !self.emu.chipset.press_key(code) {
            return Err(NotReady {
                action: format!("pressing unknown matrix code {code:#04x}"),
                budget: 0,
                at: self.emu.pc(),
            });
        }

        // The power key resets the machine instead of entering the matrix, so
        // there is no key-accept loop to wait for -- the ROM is already
        // re-initialising and will never reach ROM_KEY_ACCEPTED for it.
        if power {
            self.emu.run(settle);
            self.emu.chipset.keyboard.borrow_mut().release(Some(code));
            self.emu.run(settle);
            return Ok(());
        }

        let wanted = crate::emu::Breakpoint::physical(address::ROM_KEY_ACCEPTED);
        let mut taken = false;
        for _ in 0..accept_budget {
            if wanted.matches(self.emu.pc()) {
                taken = true;
                break;
            }
            self.emu.step();
        }

        self.emu.run(settle);
        self.emu.chipset.keyboard.borrow_mut().release(Some(code));
        self.emu.run(settle);

        if taken {
            Ok(())
        } else {
            Err(NotReady {
                action: format!("the ROM did not accept key {code:#04x}"),
                budget: accept_budget,
                at: self.emu.pc(),
            })
        }
    }

    /// Press a key by name.
    pub fn tap(&mut self, name: &str) -> Result<(), NotReady> {
        let code = self
            .emu
            .chipset
            .keyboard
            .borrow()
            .code_for_name(name)
            .ok_or_else(|| NotReady {
                action: format!("no button named {name:?}"),
                budget: 0,
                at: self.emu.pc(),
            })?;
        self.tap_code(code)
    }

    /// Press a key name, or a whole string such as `"1+2EXE"`.
    ///
    /// Strings are tokenised against the measured key table, so `1+2` presses
    /// `1`, `+`, `2`.  The trailing `EXE` is what makes the calculator evaluate.
    pub fn press(&mut self, keys: &str) -> Result<(), NotReady> {
        let names: Vec<String> = {
            let keyboard = self.emu.chipset.keyboard.borrow();
            let table = keyboard.table();
            let split = table.split_names(keys).map_err(|err| NotReady {
                action: err.to_string(),
                budget: 0,
                at: self.emu.pc(),
            })?;
            split.into_iter().map(|name| name.to_string()).collect()
        };
        for name in names {
            self.tap(&name)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ state
    /// The linear-input area as text, up to the first zero byte.
    ///
    /// The ROM stores calculator character codes, not ASCII: `+` is `0xA6`, `-` is
    /// `0xA7`.  Bytes below 0x80 happen to match ASCII, and the operator bytes are
    /// mapped back to key names through the measured table, so `1+2` reads back as
    /// `"1+2"` rather than `"1<A6>2"`.  Unknown high bytes stay visible as `<XX>`.
    pub fn input(&mut self) -> String {
        let bytes = self.emu.peek_block(address::INPUT_BUFFER, 32);
        // Snapshot the token map so no borrow is held while formatting.
        let tokens: Vec<(u8, &'static str)> = {
            let keyboard = self.emu.chipset.keyboard.borrow();
            keyboard
                .table()
                .keys()
                .filter_map(|key| key.token.map(|token| (token, key.name)))
                .collect()
        };
        let mut out = String::new();
        for byte in bytes {
            if byte == 0 {
                break;
            }
            if (0x20..0x7F).contains(&byte) {
                out.push(byte as char);
            } else if let Some((_, name)) = tokens.iter().find(|(token, _)| *token == byte) {
                out.push_str(name);
            } else {
                out.push_str(&format!("<{byte:02X}>"));
            }
        }
        out
    }

    /// The raw input-area bytes.
    pub fn input_bytes(&mut self, length: usize) -> Vec<u8> {
        self.emu.peek_block(address::INPUT_BUFFER, length)
    }

    /// The `Ans` variable as raw bytes.
    pub fn answer_bytes(&mut self) -> Vec<u8> {
        self.emu.peek_block(address::VARIABLE_ANS, 10)
    }

    /// `Ans` decoded to decimal text.
    pub fn answer_text(&mut self) -> String {
        let raw = self.answer_bytes();
        variable::decode(&raw)
            .map(|decoded| decoded.text)
            .unwrap_or_else(|err| format!("<{err}>"))
    }

    /// Any named variable as text.
    pub fn variable(&mut self, name: &str) -> Option<String> {
        let index = address::VARIABLE_ORDER
            .iter()
            .position(|entry| *entry == name)?;
        let base = address::VARIABLES + index as u32 * address::VARIABLE_SIZE;
        let raw = self.emu.peek_block(base, 10);
        variable::decode(&raw).ok().map(|decoded| decoded.text)
    }

    /// Every variable, in slot order.
    pub fn variables(&mut self) -> Vec<(&'static str, String)> {
        address::VARIABLE_ORDER
            .iter()
            .filter_map(|name| self.variable(name).map(|text| (*name, text)))
            .collect()
    }

    /// The dot-matrix rows as strings of `#` and `.`.
    pub fn display(&self) -> Vec<String> {
        self.emu.chipset.screen.borrow().render_ascii()
    }

    /// How many pixels are lit.
    pub fn lit_pixels(&self) -> usize {
        self.emu.chipset.screen.borrow().lit_pixels()
    }

    /// Render the display to a PNG byte stream.
    pub fn screen_png(&self, scale: usize) -> Vec<u8> {
        self.emu.chipset.screen.borrow().to_png(scale)
    }

    /// Evaluated expression helper: type `expression`, press `EXE`, read `Ans`.
    ///
    /// The expression must use key names, so `"1+2*3"` works but `"sin(1)"` needs
    /// the `sin` key spelled out.
    pub fn eval(&mut self, expression: &str) -> Result<String, NotReady> {
        self.press(expression)?;
        self.tap("EXE")?;
        Ok(self.answer_text())
    }
}
