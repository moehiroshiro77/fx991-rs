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

//! Where the data files live.
//!
//! Every integration test needs the ROM, and hard-coding a relative path makes the
//! suite depend on the working directory.  These helpers walk up from
//! `CARGO_MANIFEST_DIR` instead, so a test works whether it is run from the
//! workspace root or from inside a crate directory.

use std::path::{Path, PathBuf};

/// The workspace root: the first directory at or above this crate that has a
/// `data/` directory.
///
/// Returns `None` rather than panicking when there is none, which is the normal
/// case for a fresh clone: the ROM and the face texture are not distributed with
/// this repository, so `data/` may not exist at all.  Everything below is written
/// to degrade to "this data is missing" instead of failing the build.
pub fn workspace_root() -> Option<PathBuf> {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if dir.join("data").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// A path under the workspace's `data/`, whether or not it exists.
///
/// Falls back to a path relative to the crate when there is no `data/` anywhere,
/// so callers still get something printable in an error message.
pub fn data_file(relative: &str) -> PathBuf {
    match workspace_root() {
        Some(root) => root.join("data").join(relative),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join(relative),
    }
}

/// The VerF ROM image.
pub fn rom_path() -> PathBuf {
    data_file("rom_verF.bin")
}

/// The decoded face texture (`data/skin.rgba`).
pub fn skin_path() -> PathBuf {
    data_file("skin.rgba")
}

/// The verified disassembly listing.
pub fn disassembly_path() -> PathBuf {
    data_file("_disas_verF.txt")
}

/// The ROM bytes, or a message saying why they could not be read.
///
/// Returned as a `Result` rather than panicking so a checkout without the ROM can
/// still run the crate-local unit tests.
pub fn rom_bytes() -> Result<Vec<u8>, String> {
    let path = rom_path();
    std::fs::read(&path).map_err(|err| format!("could not read {}: {err}", path.display()))
}

/// The md5 documented in, for a test to assert against.
pub const ROM_MD5: &str = "47bbf88fb3a9432b311b423f9b766e8f";

/// The ROM's size in bytes.
pub const ROM_LEN: usize = 262144;

/// True when the ROM is present, so a test can skip rather than fail.
pub fn have_rom() -> bool {
    Path::new(&rom_path()).is_file()
}

/// The verified disassembly, parsed into `(address, bytes, text)` triples.
pub struct ListingEntry {
    /// Physical address.
    pub address: u32,
    /// Raw instruction bytes.
    pub bytes: Vec<u8>,
    /// Disassembled text.
    pub text: String,
}

/// Load the verified listing (`data/_disas_verF.txt`).
///
/// The listing is the project's single source of truth for "what instruction is at
/// this address" (100% byte-consistent with the ROM), so tests that
/// need to know an encoding read it rather than decoding bytes themselves.
pub fn listing() -> Result<Vec<ListingEntry>, String> {
    let path = disassembly_path();
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;

    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.len() < 8 {
            continue;
        }
        let (address_part, rest) = line.split_at(6);
        let Ok(address) = u32::from_str_radix(address_part, 16) else {
            continue;
        };
        let mut bytes = Vec::new();
        let mut instr = String::new();
        for (index, token) in rest.split_whitespace().enumerate() {
            if index < 4 && token.len() == 2 && token.chars().all(|c| c.is_ascii_hexdigit()) {
                if let Ok(byte) = u8::from_str_radix(token, 16) {
                    bytes.push(byte);
                    continue;
                }
            }
            if !instr.is_empty() {
                instr.push(' ');
            }
            instr.push_str(token);
        }
        if bytes.is_empty() {
            continue;
        }
        out.push(ListingEntry {
            address,
            bytes,
            text: instr,
        });
    }
    Ok(out)
}
