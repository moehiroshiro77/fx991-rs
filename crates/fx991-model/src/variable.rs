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

//! Decode and encode the calculator's 10-byte variable format.
//!
//! Each variable slot is 10 bytes (`mem-spans.txt`):
//!
//! `text
//! byte 0     high nibble = type (0 = floating point), low nibble = digit 1
//! bytes 1-7  digits 2.15, one BCD nibble each  -> 15 significant digits total
//! byte 8     exponent, BCD (0x00, 0x82, 0x99..), value 0.99
//! byte 9     low nibble encodes both signs:
//!              0 = mantissa +, exponent -      1 = mantissa +, exponent +
//!              5 = mantissa -, exponent -      6 = mantissa -, exponent +
//!            (high nibble always 0)
//! `
//!
//! **Everything is BCD, including the exponent.**  That is the one detail worth
//! stating twice: the VerC notes' "负指数用反码表示…82-100=-18" only works if `0x82`
//! means *decimal* 82, and the `0.5` sample (`… 99 00`) only works if `0x99` means
//! *decimal* 99 -- one less than 100, i.e. exponent `-1`.
//!
//! Sources:  §"变量的储存格式", whose two
//! worked examples are reproduced in the tests.

/// A decoded variable slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// The type nibble from byte 0; 0 is floating point.
    pub vtype: u8,
    /// The 15 significant digits.
    pub digits: String,
    /// Power of ten; `digits` is scaled by `10^exponent / 10^14`.
    pub exponent: i32,
    /// Whether the value is negative.
    pub negative: bool,
    /// The value rendered the way the display would show it.
    pub text: String,
}

/// Why a slot could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariableError {
    /// A slot is exactly 10 bytes.
    WrongLength(usize),
    /// A nibble outside 0-9, which BCD does not allow.
    NotBcd(u8),
    /// The low nibble of byte 9 is not one of the four defined sign codes.
    UnknownSign(u8),
    /// The exponent does not fit the BCD byte.
    ExponentOutOfRange(i32),
}

impl std::fmt::Display for VariableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VariableError::WrongLength(len) => {
                write!(f, "a variable slot is 10 bytes, got {len}")
            }
            VariableError::NotBcd(byte) => write!(f, "{byte:#04x} is not BCD"),
            VariableError::UnknownSign(nibble) => {
                write!(f, "unknown sign nibble {nibble:#x}")
            }
            VariableError::ExponentOutOfRange(exp) => {
                write!(f, "exponent {exp} does not fit the BCD byte")
            }
        }
    }
}

impl std::error::Error for VariableError {}

fn bcd(byte: u8) -> Result<u8, VariableError> {
    if byte >> 4 > 9 || byte & 0x0F > 9 {
        return Err(VariableError::NotBcd(byte));
    }
    Ok((byte >> 4) * 10 + (byte & 0x0F))
}

/// Decode a 10-byte slot.
pub fn decode(raw: &[u8]) -> Result<Decoded, VariableError> {
    if raw.len() != 10 {
        return Err(VariableError::WrongLength(raw.len()));
    }

    let vtype = raw[0] >> 4;
    let mut digits = String::with_capacity(15);
    digits.push(char::from(b'0' + (raw[0] & 0x0F)));
    for byte in &raw[1..8] {
        digits.push(char::from(b'0' + (byte >> 4)));
        digits.push(char::from(b'0' + (byte & 0x0F)));
    }

    if digits.bytes().all(|b| b == b'0') {
        // An all-zero slot is what the ROM writes to clear a variable.
        return Ok(Decoded {
            vtype,
            digits,
            exponent: 0,
            negative: false,
            text: "0".to_string(),
        });
    }

    let mut exponent = bcd(raw[8])? as i32;
    let sign = raw[9] & 0x0F;
    if !matches!(sign, 0 | 1 | 5 | 6) {
        return Err(VariableError::UnknownSign(sign));
    }
    let negative = matches!(sign, 5 | 6);
    if matches!(sign, 0 | 5) {
        exponent -= 100; // biased: 99 -> -1, 82 -> -18
    }

    let text = if vtype == 0 {
        format_text(&digits, exponent, negative)
    } else {
        format!("<type {vtype}>")
    };

    Ok(Decoded {
        vtype,
        digits,
        exponent,
        negative,
        text,
    })
}

/// Render the value the way the calculator's display would.
///
/// Plain decimal while the point lands inside or just before the 15 digits, and
/// scientific notation past that -- the split the VerC notes' two examples show.
pub fn format_text(digits: &str, exponent: i32, negative: bool) -> String {
    let point = exponent + 1; // digits before the decimal point
    let sign = if negative { "-" } else { "" };
    if (1..=15).contains(&point) {
        let whole = &digits[..point as usize];
        let frac = digits[point as usize..].trim_end_matches('0');
        return if frac.is_empty() {
            format!("{sign}{whole}")
        } else {
            format!("{sign}{whole}.{frac}")
        };
    }
    if (-15..=0).contains(&point) {
        let padded = format!("{}{}", "0".repeat((-point) as usize), digits);
        let frac = padded[..(15 + point) as usize].trim_end_matches('0');
        return if frac.is_empty() {
            format!("{sign}0")
        } else {
            format!("{sign}0.{frac}")
        };
    }
    let lead = &digits[..1];
    let rest = digits[1..].trim_end_matches('0');
    if rest.is_empty() {
        format!("{sign}{lead}e{exponent}")
    } else {
        format!("{sign}{lead}.{rest}e{exponent}")
    }
}

/// Encode a number into the 10-byte format.
///
/// `decode(&encode(x)).text` reproduces the calculator's *display*, not `x`
/// verbatim -- the format normalises to 15 significant digits.  Compare numbers,
/// not strings, or compare against the text the calculator itself would show.
///
/// The exponent relation follows from [`decode`]: with `digits` the 15 BCD digits
/// and the decimal point after digit `exponent + 1`, the value is
/// `int(digits) * 10^(exponent - 14)`; writing the value as
/// `int(significant) * 10^k` gives `exponent = k + len(significant) - 1`.
pub fn encode(value: f64) -> Result<[u8; 10], VariableError> {
    if !value.is_finite() {
        return Err(VariableError::ExponentOutOfRange(0));
    }

    let negative = value < 0.0;
    let magnitude = value.abs();
    if magnitude == 0.0 {
        return Ok([0u8; 10]);
    }

    // Render with enough significant digits, then read the decimal back out of the
    // string so the digits are exact rather than accumulated through arithmetic.
    let rendered = format!("{magnitude:.14e}");
    let (mantissa, exponent) = rendered
        .split_once('e')
        .expect("format! with `e` always emits an exponent");
    let exponent: i32 = exponent.parse().expect("exponent is an integer");

    let mut significant: String = mantissa.chars().filter(|c| *c != '.').collect();
    while significant.len() > 1 && significant.ends_with('0') {
        significant.pop();
    }
    if significant.len() > 15 {
        significant.truncate(15);
    }
    while significant.len() < 15 {
        significant.push('0');
    }

    if !(-99..=99).contains(&exponent) {
        return Err(VariableError::ExponentOutOfRange(exponent));
    }

    // Two nibbles per byte from byte 1 onwards, high nibble first.
    let mut raw = [0u8; 10];
    let digits: Vec<u8> = significant.bytes().map(|b| b - b'0').collect();
    raw[0] = digits[0];
    for (offset, digit) in digits[1..15].iter().enumerate() {
        let byte = 1 + offset / 2;
        if offset % 2 == 0 {
            raw[byte] = digit << 4;
        } else {
            raw[byte] |= digit;
        }
    }

    let biased_i32 = if exponent < 0 {
        exponent + 100
    } else {
        exponent
    };
    let biased =
        u8::try_from(biased_i32).map_err(|_| VariableError::ExponentOutOfRange(exponent))?;
    raw[8] = ((biased / 10) << 4) | (biased % 10);
    raw[9] = match (negative, exponent >= 0) {
        (false, true) => 1,
        (false, false) => 0,
        (true, true) => 6,
        (true, false) => 5,
    };
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(hex: &str) -> Vec<u8> {
        let cleaned: String = hex.split_whitespace().collect();
        (0..cleaned.len() / 2)
            .map(|i| u8::from_str_radix(&cleaned[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn the_five_integers_the_rom_actually_produced() {
        // Measured by typing the expression and reading ram:0xD324.
        for (hex, want) in [
            ("03 00 00 00 00 00 00 00 00 01", "3"),
            ("01 20 00 00 00 00 00 00 01 01", "12"),
            ("01 80 00 00 00 00 00 00 01 01", "18"),
            ("01 01 00 00 00 00 00 00 02 01", "101"),
            ("01 00 00 00 00 00 00 00 03 01", "1000"),
        ] {
            assert_eq!(decode(&slot(hex)).unwrap().text, want, "{hex}");
        }
    }

    #[test]
    fn the_tutorial_examples_decode() {
        //  §"变量的储存格式"
        assert_eq!(
            decode(&slot("06 89 47 57 00 00 00 00 00 01")).unwrap().text,
            "6.894757"
        );
        assert_eq!(
            decode(&slot("07 46 42 68 41 12 85 46 82 05")).unwrap().text,
            "-7.46426841128546e-18"
        );
    }

    #[test]
    fn a_negative_biased_exponent_is_bcd_not_binary() {
        // 0x99 is *decimal* 99, i.e. exponent -1 -- this is the case a naive
        // binary reading of the byte gets wrong.
        let decoded = decode(&slot("05 00 00 00 00 00 00 00 99 00")).unwrap();
        assert_eq!(decoded.exponent, -1);
        assert_eq!(decoded.text, "0.5");
    }

    #[test]
    fn an_all_zero_slot_decodes_as_zero() {
        assert_eq!(decode(&[0u8; 10]).unwrap().text, "0");
    }

    #[test]
    fn a_slot_must_be_ten_bytes() {
        assert_eq!(decode(&[0u8; 9]), Err(VariableError::WrongLength(9)));
    }

    #[test]
    fn a_non_bcd_exponent_is_rejected() {
        // 0xFA has an A nibble.
        let mut raw = slot("03 00 00 00 00 00 00 00 00 01");
        raw[8] = 0xFA;
        assert!(matches!(decode(&raw), Err(VariableError::NotBcd(0xFA))));
    }

    #[test]
    fn an_undefined_sign_nibble_is_rejected() {
        let mut raw = slot("03 00 00 00 00 00 00 00 00 01");
        raw[9] = 0x03;
        assert!(matches!(decode(&raw), Err(VariableError::UnknownSign(3))));
    }

    #[test]
    fn encoding_reproduces_the_bytes_the_rom_measured() {
        for (value, hex) in [
            (3.0, "03 00 00 00 00 00 00 00 00 01"),
            (12.0, "01 20 00 00 00 00 00 00 01 01"),
            (1000.0, "01 00 00 00 00 00 00 00 03 01"),
            (0.5, "05 00 00 00 00 00 00 00 99 00"),
        ] {
            assert_eq!(encode(value).unwrap().to_vec(), slot(hex), "{value}");
        }
    }

    #[test]
    fn encode_and_decode_round_trip() {
        for value in [
            0.0, 1.0, 3.0, 12.0, 101.0, 1000.0, 0.5, 6.894757, -7.5, -0.5,
        ] {
            let raw = encode(value).unwrap();
            let decoded = decode(&raw).unwrap();
            let parsed: f64 = decoded.text.parse().unwrap_or_else(|_| {
                // Scientific notation with an exponent the parser accepts.
                decoded.text.clone().parse().unwrap()
            });
            assert!(
                (parsed - value).abs() < 1e-9,
                "{value} round-tripped to {}",
                decoded.text
            );
        }
    }
}
