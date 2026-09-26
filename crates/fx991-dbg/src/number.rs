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

//! One number grammar, shared by every language in the debugger.
//!
//! The command line, the patch assembler and the condition language all read
//! numbers, and a user reasonably expects one notation to work in all three.  It
//! is written once here so the three cannot drift apart.
//!
//! # What is accepted
//!
//! | form | meaning | example |
//! |---|---|---|
//! | `0x`/`0X`/`$` prefix | hex | `0xD180`, `$10` |
//! | `0b`/`0B` prefix | binary | `0b1010` |
//! | trailing `h`/`H` | hex | `D180h`, `10h` |
//! | bare, with a hex letter | hex | `D180`, `121A8` |
//! | bare digits | decimal | `16`, `22110` |
//!
//! # The five-digit ambiguity, and why it is not guessed
//!
//! `22110` is a valid decimal *and* a valid hex address, and the two are
//! different places (`0x565E` versus `0x22110`).  Guessing would be the worst
//! outcome: a breakpoint would silently land somewhere plausible and the user
//! would believe a wrong result.  So a bare all-digit run is **decimal**, and the
//! address is written `22110h` or `0x22110` to mean hex.  A hex *letter* is
//! unambiguous, so `121A8` and `D180` work unqualified.
//!
//! This is why binary needs its `0b`: `1010` alone is decimal ten, and reading it
//! as binary would be the same kind of silent misreading the rule above exists to
//! prevent.

/// Read a number from the front of `text`, and how many bytes it used.
///
/// This is a *prefix* parser: the tokenizer in the condition language hands it the
/// rest of the source and needs to know where the number ended.  Trailing text is
/// not an error -- use [`parse_u32`] when the whole string must be a number.
pub fn parse_prefix(text: &str) -> Option<(u64, usize)> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }

    // `0x`/`0X`/`$` hex and `0b`/`0B` binary.
    if bytes.len() >= 2 && bytes[0] == b'0' {
        let radix = match bytes[1] {
            b'x' | b'X' => Some(16),
            b'b' | b'B' => Some(2),
            _ => None,
        };
        if let Some(radix) = radix {
            let digits = digits_of(&text[2..], radix);
            if digits == 0 {
                return None;
            }
            return u64::from_str_radix(&text[2..2 + digits], radix)
                .ok()
                .map(|value| (value, 2 + digits));
        }
    }
    if let Some(rest) = text.strip_prefix('$') {
        let digits = digits_of(rest, 16);
        if digits == 0 {
            return None;
        }
        return u64::from_str_radix(&rest[..digits], 16)
            .ok()
            .map(|value| (value, 1 + digits));
    }

    // A trailing `h` marks hex, the way the plans write addresses.
    let hex_digits = digits_of(text, 16);
    if hex_digits > 0 && text[hex_digits..].starts_with(['h', 'H']) {
        return u64::from_str_radix(&text[..hex_digits], 16)
            .ok()
            .map(|value| (value, hex_digits + 1));
    }

    // A run containing a hex letter cannot be decimal, so it is an address: this
    // is what lets `121A8` and `D180` be written the way the notes write them.
    if hex_digits >= 3 && text[..hex_digits].bytes().any(is_hex_letter) {
        return u64::from_str_radix(&text[..hex_digits], 16)
            .ok()
            .map(|value| (value, hex_digits));
    }

    let digits = digits_of(text, 10);
    if digits == 0 {
        return None;
    }
    text[..digits]
        .parse::<u64>()
        .ok()
        .map(|value| (value, digits))
}

/// Read a number that must make up the whole of `text` (ignoring surrounding
/// whitespace), narrowed to `u32`.
pub fn parse_u32(text: &str) -> Option<u32> {
    let text = text.trim();
    let (value, width) = parse_prefix(text)?;
    (width == text.len()).then_some(value as u32)
}

/// How many leading characters of `source` are valid digits in `radix`.
fn digits_of(source: &str, radix: u32) -> usize {
    source
        .bytes()
        .take_while(|byte| (*byte as char).is_digit(radix))
        .count()
}

fn is_hex_letter(byte: u8) -> bool {
    matches!(byte, b'a'..=b'f' | b'A'..=b'F')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prefixed_forms_are_read_in_their_own_radix() {
        assert_eq!(parse_u32("0x10"), Some(16));
        assert_eq!(parse_u32("0X10"), Some(16));
        assert_eq!(parse_u32("$10"), Some(16));
        assert_eq!(parse_u32("0b1010"), Some(10), "binary, not 0xB1010");
        assert_eq!(parse_u32("0B1010"), Some(10));
        assert_eq!(parse_u32("10h"), Some(16));
        assert_eq!(parse_u32("10H"), Some(16));
        assert_eq!(parse_u32("0F968h"), Some(0xF_968));
    }

    #[test]
    fn a_bare_run_with_a_hex_letter_is_an_address() {
        assert_eq!(parse_u32("D180"), Some(0xD180));
        assert_eq!(parse_u32("F042"), Some(0xF042));
        assert_eq!(parse_u32("121A8"), Some(0x1_21A8));
        // Two digits is too short to be an address, so it stays decimal.
        assert_eq!(parse_u32("1a"), None, "not long enough to be an address");
    }

    #[test]
    fn a_bare_all_digit_run_is_decimal() {
        assert_eq!(parse_u32("16"), Some(16));
        // Both readings are valid and they are different places, so the
        // documented rule is decimal; hex needs a marker.
        assert_eq!(parse_u32("22110"), Some(22110));
        assert_eq!(parse_u32("22110h"), Some(0x2_2110));
        assert_eq!(parse_u32("0x22110"), Some(0x2_2110));
    }

    #[test]
    fn the_whole_string_has_to_be_the_number() {
        assert_eq!(parse_u32(""), None);
        assert_eq!(parse_u32("zzz"), None);
        assert_eq!(parse_u32("16x"), None);
        assert_eq!(parse_u32(" 16 "), Some(16), "surrounding space is fine");
        assert_eq!(parse_u32("0x"), None, "a prefix with no digits");
        assert_eq!(parse_u32("0b"), None);
    }

    #[test]
    fn the_prefix_parser_reports_where_the_number_ended() {
        assert_eq!(parse_prefix("0x10"), Some((16, 4)));
        assert_eq!(parse_prefix("0b1010"), Some((10, 6)));
        assert_eq!(parse_prefix("10h"), Some((16, 3)));
        assert_eq!(parse_prefix("121A8"), Some((0x1_21A8, 5)));
        // Trailing text is left for the caller: this is how the condition
        // tokenizer walks `r0 == 10 and c`.
        assert_eq!(parse_prefix("10 and c"), Some((10, 2)));
        assert_eq!(parse_prefix("D180] == 1"), Some((0xD180, 4)));
    }

    #[test]
    fn a_bare_run_is_only_hex_when_it_holds_a_letter() {
        // Three or more characters with a hex letter in them: an address.
        assert_eq!(parse_prefix("121A8"), Some((0x1_21A8, 5)));
        assert_eq!(parse_prefix("D180] == 1"), Some((0xD180, 4)));
        // All digits: decimal, so `12188` is not `0x12188`.
        assert_eq!(parse_prefix("12188"), Some((12188, 5)));
        // A run too short to be an address stays decimal, letter or not.
        assert_eq!(parse_prefix("1a"), Some((1, 1)));
    }
}
