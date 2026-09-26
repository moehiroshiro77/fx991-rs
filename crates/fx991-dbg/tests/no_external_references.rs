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

//! Every file in this repository must be understandable from a standalone clone,
//! plus the files the user supplies at run time.
//!
//! # The rule
//!
//! Nothing here may name a path outside the repository.  A reference a reader cannot
//! open is worse than no reference: it cannot be checked, cannot be dated, and reads
//! as authoritative while the file it points at may be gone.
//!
//! # What is allowed
//!
//! * `data/...` -- the files the user supplies.  They are git-ignored, and naming
//!   them is the whole point of the README.
//! * `target/...` -- this repository's own build output.
//! * External **URLs** and licence attribution.  Crediting another project by name
//!   and link is a reference, not a dangling path.
//! * Any path that exists in this repository.
//!
//! # How to satisfy it
//!
//! Say what the thing **is**, not where it lives: "a separate tool", "the
//! calculator's own key map", "measured from the emulator".  If the file is one the
//! user supplies, add it to [`ALLOWED`] and document it in `README.md`.
//!
//! # Why this is a test
//!
//! The failure is invisible: a comment naming a file that does not exist here
//! compiles, passes every other check, and looks authoritative.  Only a test fails
//! when it happens.

use std::path::{Path, PathBuf};

/// Directory prefixes that may be named and are absent from a clone.
///
/// Supplied by the user at run time and git-ignored; the README says how to obtain
/// each one.  Checked by `every_allowed_prefix_is_documented` so the exemption
/// cannot quietly widen.
const ALLOWED: &[&str] = &["data/", "target/"];

/// Substrings that identify a path outside this repository, whatever it is called.
///
/// Explicit rather than clever: a pattern that guessed would either miss something
/// or fire on prose.  The general check below is what catches an unknown shape.
const FORBIDDEN: &[&str] = &[
    "../",
    "casiostudy",
    "model.lua",
    "lua_name",
    "docs/ui-plan",
    "tools/emini",
    "tools/u8dis",
    "tools/gen_rust",
    "data/web",
    "emini-status",
    "HANDOVER.md",
    "ui-plan",
    "mem-spans",
    "interface.png",
    "CPU.cpp",
    "Keyboard.cpp",
    "Screen.cpp",
];

/// The repository root, from this crate's manifest directory.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/<name> has a grandparent")
        .to_path_buf()
}

/// Every tracked-looking source and document file, skipping build output and VCS data.
fn files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if matches!(name.as_str(), "target" | ".git" | "data" | "kbtmp") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            let relevant = matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("rs" | "md" | "toml" | "yml" | "dbg" | "py" | "sh")
            );
            // The guard itself has to spell the forbidden strings, so it is not a
            // subject of its own check.
            if relevant && name != "no_external_references.rs" {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn nothing_in_this_repository_names_a_path_outside_it() {
    let root = root();
    let mut problems = Vec::new();

    for path in files(&root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string()
            .replace('\\', "/");

        for (number, line) in text.lines().enumerate() {
            // A line that is only a URL is attribution, not a path.
            if line.contains("http://") || line.contains("https://") {
                continue;
            }
            for needle in FORBIDDEN {
                if line.contains(needle) {
                    problems.push(format!(
                        "{relative}:{}: names {needle:?}\n      {}",
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        problems.is_empty(),
        "these lines name something that is not in this repository.\n\
         A standalone clone cannot resolve it, so a reader cannot check the claim it \
         supports.\n\
         Rewrite the reference to say what the thing *is* (\"the reference \
         implementation\", \"a separate tool\") instead of where it lives.\n\
         If the path is legitimately supplied by the user, add it to ALLOWED and \
         document it in the README.\n\n{}",
        problems.join("\n")
    );
}

/// A path written in backticks that carries a file extension.
///
/// This is the general form of the rule, and it is the check that matters: the
/// deny-list above catches known shapes, while this catches an unknown one whatever
/// it is called.
fn quoted_paths(line: &str) -> Vec<String> {
    const EXTENSIONS: &[&str] = &[
        "rs", "md", "py", "toml", "yml", "txt", "dbg", "bin", "rgba", "json", "png", "lua", "sh",
        "cpp", "hpp", "yaml",
    ];
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let Some(close) = rest[open + 1..].find('`') else {
            break;
        };
        let quoted = &rest[open + 1..open + 1 + close];
        rest = &rest[open + 1 + close + 1..];

        // Anything with a known source or data extension, with or without a
        // directory: a bare `keytable.py` is as unresolvable here as a full path,
        // and that is the shape of the references this rule exists for.
        let has_extension = quoted
            .rsplit_once('.')
            .is_some_and(|(_, ext)| EXTENSIONS.contains(&ext));
        // A quoted *command line* is not a path, even when it ends in `.bin`.
        if !has_extension || quoted.contains(' ') {
            continue;
        }
        // A glob is a pattern, not a path, and a URL is a reference.
        if quoted.contains(['*', '{']) || quoted.contains("://") {
            continue;
        }
        out.push(quoted.to_string());
    }
    out
}

#[test]
fn every_path_the_repository_quotes_resolves_inside_it() {
    let root = root();
    let mut problems = Vec::new();

    for path in files(&root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string()
            .replace('\\', "/");

        for (number, line) in text.lines().enumerate() {
            for quoted in quoted_paths(line) {
                if ALLOWED.iter().any(|allowed| quoted.starts_with(allowed)) {
                    continue;
                }
                // A path may be written relative to the repository, to `crates/`,
                // to a top-level directory, or to the crate that quotes it.
                let mut candidates = vec![
                    root.join(&quoted),
                    root.join("crates").join(&quoted),
                    root.join("docs").join(&quoted),
                    root.join("scripts").join(&quoted),
                ];
                // The crate that contains this file, so `tests/golden.rs` written
                // inside `fx991-ui/src/` resolves.
                if let Ok(within) = path.strip_prefix(root.join("crates")) {
                    if let Some(crate_name) = within.components().next() {
                        candidates.push(root.join("crates").join(crate_name).join(&quoted));
                    }
                }
                if candidates.iter().any(|candidate| candidate.exists()) {
                    continue;
                }
                // A bare filename resolves by name anywhere in the repository: prose
                // says `dispatch_digest.rs` for a module, not for a path.
                if !quoted.contains('/')
                    && files(&root)
                        .iter()
                        .any(|known| known.file_name().is_some_and(|n| n == quoted.as_str()))
                {
                    continue;
                }
                problems.push(format!(
                    "{relative}:{}: quotes {quoted:?}, which does not exist here
      {}",
                    number + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "these quoted paths do not resolve inside this repository.
         A standalone clone cannot open them, so a reader cannot check the claim they          support.  Rewrite the text to describe what the thing is rather than where it          lives, or delete the reference.
         Do **not** silence this by adding to ALLOWED unless the file is one the user          supplies at run time and README.md documents it.

{}",
        problems.join("
")
    );
}

#[test]
fn every_allowed_prefix_is_documented() {
    let root = root();
    let readme = std::fs::read_to_string(root.join("README.md")).expect("README.md");
    for allowed in ALLOWED {
        assert!(
            readme.contains(allowed.trim_end_matches('/')),
            "ALLOWED exempts {allowed:?}, but README.md never mentions it"
        );
    }
}

#[test]
fn every_data_path_the_repository_names_is_one_the_readme_explains() {
    // The other half of the rule: `data/...` is allowed, so it has to stay honest.
    // Anything named there must appear in the README, or a reader is told to look
    // for a file the documentation never mentions.
    let root = root();
    let readme = std::fs::read_to_string(root.join("README.md")).expect("README.md");

    for path in files(&root) {
        if path.file_name().and_then(|n| n.to_str()) == Some("README.md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let Some(start) = line.find("data/") else {
                continue;
            };
            let rest = &line[start..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-')))
                .unwrap_or(rest.len());
            let named = &rest[..end];
            assert!(
                readme.contains(named),
                "{} names {named:?}, which README.md never mentions",
                path.strip_prefix(&root).unwrap_or(&path).display()
            );
        }
    }
}
