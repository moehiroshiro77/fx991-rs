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

//! The measured key table, and the lookups built on top of it.
//!
//! Three things have to line up for a key press to mean something:
//!
//! 1. **The matrix code** -- the value in the emulator's own key map.  This is what
//!    the KO/KI scan uses and the only thing the hardware knows.
//! 2. **The button** (`+`/`SHIFT`/`ALPHA`/`sin`/..).  The emulator's key map leaves
//!    18 of the 48 buttons unnamed.
//! 3. **The token the ROM writes into the input area** (`ram:0xD180`).  This is the
//!    calculator's own character code and is independent of the matrix code.
//!
//! The `token` values in [`keys:KEYS`] were **measured**: press the code, wait for
//! the ROM to accept it, then read `0xD180`.
//! reproduces the measurement, and every code with a token returned the same byte on
//! two separate boots.
//!
//! **One of 's names is wrong and it matters.**  It calls code `0x30`
//! `=`; the ROM says code `0x30` types token `0xA6`, which the character table
//! decodes as `+`.  Evaluating is `EXE` (code `0x60`), which inserts no character.

use std::collections::HashMap;

use crate::keys::{KEYMAP_NAMES, KEYS, UNNAMED_BLOCK_CODES};

/// One button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    /// The matrix code from.
    pub code: u8,
    /// Human name; from the ROM's own token where one was measured.
    pub name: &'static str,
    /// The byte the ROM writes into `ram:0xD180`, when the key inserts anything.
    pub token: Option<u8>,
}

impl Key {
    /// Build a key.  `const` so the generated table can be a literal.
    pub const fn new(code: u8, name: &'static str, token: Option<u8>) -> Self {
        Self { code, name, token }
    }

    /// True when the ROM stores a character for this key.
    pub fn inserts_character(&self) -> bool {
        self.token.is_some()
    }
}

/// A lookup over [`KEYS`], with 's names layered underneath.
#[derive(Clone)]
pub struct KeyTable {
    by_code: HashMap<u8, Key>,
    by_name: HashMap<String, u8>,
}

impl Default for KeyTable {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyTable {
    /// Build the table.
    pub fn new() -> Self {
        let mut by_code = HashMap::new();
        for key in KEYS {
            by_code.entry(key.code).or_insert(*key);
        }

        let mut by_name: HashMap<String, u8> = HashMap::new();
        for key in KEYS {
            by_name.entry(key.name.to_string()).or_insert(key.code);
        }
        // 's names fill gaps but never override a measured name, so
        // `press("=")` still resolves -- to 0x30, which is `+`.
        for (name, code) in KEYMAP_NAMES {
            if by_code.contains_key(code) {
                by_name.entry((*name).to_string()).or_insert(*code);
            }
        }
        by_name.entry("Enter".to_string()).or_insert(0x60);
        by_name.entry("POWER".to_string()).or_insert(0xFF);
        // `1/x` and `pow` would resolve to `(-)` and `fraction`, which are not the
        // keys those names describe: the reciprocal is 0x24 and the square is 0x25.
        by_name.entry("reciprocal".to_string()).or_insert(0x24);
        by_name.entry("square".to_string()).or_insert(0x25);
        by_name.entry("power".to_string()).or_insert(0x35);

        Self { by_code, by_name }
    }

    /// Look a button up by matrix code.
    pub fn by_code(&self, code: u8) -> Option<&Key> {
        self.by_code.get(&code)
    }

    /// Look a button up by name.
    pub fn by_name(&self, name: &str) -> Option<&Key> {
        self.by_name
            .get(name)
            .and_then(|code| self.by_code.get(code))
    }

    /// Every name the table knows, including the aliases layered over [`KEYS`].
    ///
    /// A caller that has to split a string into key names needs this to know how
    /// long a name can be; taking the bound from the table keeps it right when a
    /// longer name is added.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    /// The button's name, or `None` for a code the model file does not mention.
    pub fn name_of(&self, code: u8) -> Option<&'static str> {
        self.by_code.get(&code).map(|key| key.name)
    }

    /// Every button, in table order.
    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        KEYS.iter()
    }

    /// The three unnamed blocks in  order (row-major, 6 per row).
    pub fn unnamed_block_codes(&self) -> &'static [[u8; 6]] {
        UNNAMED_BLOCK_CODES
    }

    /// Translate a key name into the token it would type, if any.
    pub fn token_for(&self, name: &str) -> Option<u8> {
        self.by_name(name).and_then(|key| key.token)
    }

    /// Turn a string of key names into the token bytes they would produce.
    ///
    /// `"1+2EXE"` -> `[0x31, 0xA6, 0x32]`: `EXE` evaluates rather than inserting,
    /// so it contributes no token.  Unknown names are an error rather than a
    /// silently dropped character.
    pub fn tokens_for(&self, text: &str) -> Result<Vec<u8>, KeyError> {
        let mut out = Vec::new();
        for name in self.split_names(text)? {
            let key = self
                .by_name
                .get(name)
                .and_then(|code| self.by_code.get(code))
                .ok_or_else(|| KeyError::UnknownName(name.to_string()))?;
            if let Some(token) = key.token {
                out.push(token);
            }
        }
        Ok(out)
    }

    /// Split `"1+2"` into key names, preferring the longest known name.
    pub fn split_names<'a>(&self, text: &'a str) -> Result<Vec<&'a str>, KeyError> {
        let mut names: Vec<&str> = self.by_name.keys().map(String::as_str).collect();
        names.sort_by_key(|name| std::cmp::Reverse(name.len()));

        let mut out = Vec::new();
        let mut rest = text;
        'outer: while !rest.is_empty() {
            for name in &names {
                if !name.is_empty() && rest.starts_with(name) {
                    out.push(&rest[..name.len()]);
                    rest = &rest[name.len()..];
                    continue 'outer;
                }
            }
            return Err(KeyError::Unparsable(rest.to_string()));
        }
        Ok(out)
    }
}

/// Why a key name could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// No button has that name.
    UnknownName(String),
    /// The remaining text does not start with any known key name.
    Unparsable(String),
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyError::UnknownName(name) => write!(f, "unknown key {name:?}"),
            KeyError::Unparsable(rest) => write!(f, "cannot read the key sequence at {rest:?}"),
        }
    }
}

impl std::error::Error for KeyError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_measured_code_resolves() {
        let table = KeyTable::new();
        for key in KEYS {
            assert_eq!(table.by_code(key.code).map(|k| k.code), Some(key.code));
        }
    }

    #[test]
    fn keymap_names_still_resolve() {
        let table = KeyTable::new();
        for (name, code) in KEYMAP_NAMES {
            assert_eq!(
                table.by_name(name).map(|key| key.code),
                Some(*code),
                "{name}"
            );
        }
    }

    #[test]
    fn the_measured_names_beat_the_misnamed_ones() {
        let table = KeyTable::new();
        //  calls 0x30 "="; the token says it is "+".
        assert_eq!(table.by_name("+").unwrap().code, 0x30);
        assert_eq!(table.by_name("=").unwrap().code, 0x30, "alias kept working");
        assert_eq!(table.by_code(0x30).unwrap().name, "+");
        assert_eq!(table.by_code(0x60).unwrap().name, "EXE");
        assert!(!table.by_code(0x60).unwrap().inserts_character());
    }

    #[test]
    fn the_operators_are_the_measured_tokens() {
        let table = KeyTable::new();
        assert_eq!(table.token_for("+"), Some(0xA6));
        assert_eq!(table.token_for("-"), Some(0xA7));
        assert_eq!(table.token_for("*"), Some(0xA8));
        assert_eq!(table.token_for("/"), Some(0xA9));
        assert_eq!(table.token_for("1"), Some(0x31));
        assert_eq!(table.token_for("."), Some(0x2E));
    }

    #[test]
    fn tokens_for_skips_keys_that_insert_nothing() {
        let table = KeyTable::new();
        assert_eq!(table.tokens_for("1+2").unwrap(), vec![0x31, 0xA6, 0x32]);
        assert_eq!(table.tokens_for("1+2EXE").unwrap(), vec![0x31, 0xA6, 0x32]);
        assert_eq!(table.tokens_for("EXE").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn an_unknown_name_is_an_error() {
        let table = KeyTable::new();
        assert!(matches!(
            table.tokens_for("NoSuchKey"),
            Err(KeyError::Unparsable(_))
        ));
        assert!(table.by_name("NoSuchKey").is_none());
    }

    #[test]
    fn a_code_the_model_file_does_not_mention_has_no_name() {
        let table = KeyTable::new();
        assert!(table.by_code(0x77).is_none());
    }
}
