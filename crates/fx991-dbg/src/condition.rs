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

//! Breakpoint conditions: a small expression language, parsed and evaluated.
//!
//! The syntax is deliberately close to what a debugger user already types, so
//! `bp 0F968 if [filter] == 0xFF` and `bp 201FA14 if r0 == 0x40 && hits > 2` both
//! work:
//!
//! ```text
//! expr    := or
//! or      := and ('||' and)*
//! and     := cmp ('&&' cmp)*
//! cmp     := unary (('==' | '!=' | '<' | '<=' | '>' | '>=') unary)?
//! unary   := '!' unary | primary
//! primary := '(' expr ')' | '[' expr ']' | number | name
//! ```
//!
//! An operand used as a condition is true when it is non-zero, so `if r0` reads as
//! "when R0 is set" and `if [0xD180]` as "when the input area's first byte is not
//! zero".
//!
//! # Registers
//!
//! `r0`..`r15` are bytes, `er0`..`er14` (even only), `xr0`/`xr4`/`xr8`/`xr12` and
//! `qr0`/`qr8` are the wider aliases of the same bank, and `sp`, `pc`, `csr`,
//! `psw`, `ea`, `lr`, `lcsr` and `dsr` are the control registers.  The PSW bits
//! have their own names: `c`, `z`, `s`, `ov`, `mie`, `hc`, and `elevel`.
//!
//! `pc` is the address of the instruction *about to run*, because conditions are
//! evaluated before the instruction does.  That is what a breakpoint means; note
//! it differs from what a stepper reports afterwards, which is the next address.
//!
//! # Names
//!
//! The symbols in `fx991_model`'s address module are available by short name
//! (`input`, `answer`, `filter`, `key_accepted`, `idle`, `reset_entry`, ...), so a
//! condition can say `[filter] == 0xFF` instead of `[0xF042] == 0xFF`.  See
//! [`SYMBOLS`] for the full list.
//!
//! # Why this is not a general scripting language
//!
//! Conditions are evaluated on *every* arrival at a breakpoint, so this is a
//! tree-walking interpreter over a parsed form with no allocation on the evaluate
//! path, and no string handling at all.  Parsing happens once, when the
//! breakpoint is set.

use fx991_bus::Bus;
use nxu8_core::regs::{PSW_C, PSW_HC, PSW_MIE, PSW_OV, PSW_S, PSW_Z};
use nxu8_core::Regs;

/// The symbols a condition may name, as `(name, address)`.
///
/// Kept short and lowercase on purpose: these are meant to be typed.
pub const SYMBOLS: &[(&str, u32)] = &[
    // The RAM areas worth naming, because they are what a condition reads.
    ("input", fx991_model::address::INPUT_BUFFER),
    ("replay", fx991_model::address::REPLAY_BUFFER),
    ("random", fx991_model::address::RANDOM_SEED),
    ("counter", fx991_model::address::COUNTER),
    ("vars", fx991_model::address::VARIABLES),
    ("m", fx991_model::address::VARIABLES),
    ("answer", fx991_model::address::VARIABLE_ANS),
    ("ans", fx991_model::address::VARIABLE_ANS),
    (
        "preans",
        fx991_model::address::VARIABLES + 11 * fx991_model::address::VARIABLE_SIZE,
    ),
    ("row", fx991_model::address::SCREEN_ROW),
    ("mirror_a", fx991_model::address::SCREEN_MIRROR_A),
    ("mirror_b", fx991_model::address::SCREEN_MIRROR_B),
    ("input_mode", fx991_model::address::INPUT_MODE),
    ("cursor", fx991_model::address::CURSOR),
    // SFRs worth naming, because they are what a wait loop polls.
    ("filter", fx991_model::address::KEY_INPUT_FILTER),
    ("ki", fx991_chipset::keyboard::KI),
    ("ko", fx991_chipset::keyboard::KO),
    ("komask", fx991_chipset::keyboard::KO_MASK),
    ("int_mask", 0x0_F010),
    ("int_pending", 0x0_F014),
    ("port", 0x0_F050),
    ("lcd_range", fx991_chipset::screen::CONTROL_RANGE),
    ("lcd_mode", fx991_chipset::screen::CONTROL_MODE),
    ("lcd_contrast", fx991_chipset::screen::CONTROL_CONTRAST),
    ("lcd_buffer", fx991_chipset::screen::BUFFER_BASE),
    ("timer_interval", fx991_chipset::timer::INTERVAL),
    ("timer_counter", fx991_chipset::timer::COUNTER),
    ("timer_control", fx991_chipset::timer::CONTROL),
    // ROM landmarks.
    ("key_accepted", fx991_model::address::ROM_KEY_ACCEPTED),
    ("idle", fx991_model::address::ROM_IDLE),
    ("reset_entry", fx991_model::address::ROM_RESET_ENTRY),
];

/// A register a condition may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg {
    /// One of the sixteen byte registers.
    Byte(u8),
    /// A 16-bit `ERn`, indexed by its byte offset (even).
    Word(u8),
    /// A 32-bit `XRn`, indexed by its byte offset (multiple of four).
    Xr(u8),
    /// A 64-bit `QRn`, indexed by its byte offset (0 or 8).
    Qr(u8),
    /// The stack pointer.
    Sp,
    /// The program counter.
    Pc,
    /// The code segment register.
    Csr,
    /// The program status word.
    Psw,
    /// The effective address.
    Ea,
    /// The exception link register.
    Lr,
    /// The exception CSR.
    Lcsr,
    /// The data segment register.
    Dsr,
    /// A single PSW bit.
    Flag(Flag),
}

/// A PSW bit a condition may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    /// Carry.
    Carry,
    /// Zero.
    Zero,
    /// Sign.
    Sign,
    /// Overflow.
    Overflow,
    /// Master interrupt enable.
    Mie,
    /// Half carry.
    HalfCarry,
    /// The exception level, as a two-bit number.
    Elevel,
}

/// The comparison operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

impl Cmp {
    fn test(self, left: u64, right: u64) -> bool {
        match self {
            Cmp::Eq => left == right,
            Cmp::Ne => left != right,
            Cmp::Lt => left < right,
            Cmp::Le => left <= right,
            Cmp::Gt => left > right,
            Cmp::Ge => left >= right,
        }
    }
}

/// A parsed condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// A literal number.
    Number(u64),
    /// A register, or a PSW bit.
    Register(Reg),
    /// A symbol's address, i.e. the symbol itself rather than what it points at.
    Address(u32),
    /// A read of the data space at the inner expression's value.
    Load(Box<Expr>),
    /// This breakpoint's hit count so far, before the current hit.
    Hits,
    /// A comparison.
    Compare(Box<Expr>, Cmp, Box<Expr>),
    /// `&&`
    And(Box<Expr>, Box<Expr>),
    /// `||`
    Or(Box<Expr>, Box<Expr>),
    /// `!`
    Not(Box<Expr>),
}

/// Why a condition could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// What went wrong, in the user's terms.
    pub message: String,
    /// Byte offset into the source where it went wrong.
    pub position: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at offset {})", self.message, self.position)
    }
}

impl std::error::Error for ParseError {}

/// What a condition is evaluated against.
///
/// Borrowed rather than owned so evaluation allocates nothing: this runs on every
/// arrival at a conditional breakpoint.
pub struct Context<'a> {
    /// The register file the instruction is about to act on.
    pub regs: &'a Regs,
    /// The data space, for `[..]` references.
    ///
    /// Reads go through [`Bus::peek_quiet`], so a condition cannot trip a
    /// watchpoint or add to the fault log by being tested.
    pub bus: &'a mut Bus,
    /// The hit count of the breakpoint being tested.
    pub hits: u64,
}

impl Expr {
    /// Parse a condition.
    ///
    /// An empty condition is not valid here; callers that treat "no condition" as
    /// "always" should store `None` instead (see [`crate::breakpoint::Breakpoint`]).
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let mut parser = Parser {
            tokens: tokenize(text)?,
            position: 0,
            source_len: text.len(),
        };
        let expr = parser.parse_or()?;
        if let Some(token) = parser.peek() {
            return Err(ParseError {
                message: format!("unexpected {token:?} after the expression"),
                position: parser.offset(),
            });
        }
        Ok(expr)
    }

    /// Evaluate against a machine state.  Anything non-zero is true.
    pub fn eval(&self, context: &mut Context<'_>) -> bool {
        self.value(context) != 0
    }

    /// Evaluate to a number.
    pub fn value(&self, context: &mut Context<'_>) -> u64 {
        match self {
            Expr::Number(value) => *value,
            Expr::Address(address) => *address as u64,
            Expr::Hits => context.hits,
            Expr::Register(reg) => read_register(context.regs, *reg),
            Expr::Load(inner) => {
                let address = inner.value(context) as u32;
                context.bus.peek_quiet(address) as u64
            }
            Expr::Not(inner) => (inner.value(context) == 0) as u64,
            Expr::And(left, right) => {
                // Short-circuit, so `[x] != 0 && [y] != 0` does not read `y` when
                // `x` is zero.  Worth having: a condition should not depend on
                // reads it did not need.
                (left.value(context) != 0 && right.value(context) != 0) as u64
            }
            Expr::Or(left, right) => (left.value(context) != 0 || right.value(context) != 0) as u64,
            Expr::Compare(left, op, right) => {
                op.test(left.value(context), right.value(context)) as u64
            }
        }
    }

    /// The registers this condition reads, for a debugger to display.
    pub fn registers(&self) -> Vec<Reg> {
        let mut out = Vec::new();
        self.collect_registers(&mut out);
        out
    }

    fn collect_registers(&self, out: &mut Vec<Reg>) {
        match self {
            Expr::Register(reg) => out.push(*reg),
            Expr::Load(inner) => inner.collect_registers(out),
            Expr::Not(inner) => inner.collect_registers(out),
            Expr::And(left, right) | Expr::Or(left, right) => {
                left.collect_registers(out);
                right.collect_registers(out);
            }
            Expr::Compare(left, _, right) => {
                left.collect_registers(out);
                right.collect_registers(out);
            }
            Expr::Number(_) | Expr::Address(_) | Expr::Hits => {}
        }
    }
}

fn read_register(regs: &Regs, reg: Reg) -> u64 {
    match reg {
        Reg::Byte(index) => regs.r[(index & 0xF) as usize] as u64,
        Reg::Word(index) => regs.er((index & 0xE) as usize) as u64,
        Reg::Xr(index) => regs.xr((index & 0xC) as usize) as u64,
        Reg::Qr(index) => regs.reg((index & 0x8) as usize, 8),
        Reg::Sp => regs.sp as u64,
        Reg::Pc => regs.physical_pc() as u64,
        Reg::Csr => regs.csr as u64,
        Reg::Psw => regs.psw() as u64,
        Reg::Ea => regs.ea as u64,
        Reg::Lr => regs.lr() as u64,
        Reg::Lcsr => regs.lcsr() as u64,
        Reg::Dsr => regs.dsr as u64,
        Reg::Flag(flag) => {
            let psw = regs.psw();
            match flag {
                Flag::Carry => (psw & PSW_C != 0) as u64,
                Flag::Zero => (psw & PSW_Z != 0) as u64,
                Flag::Sign => (psw & PSW_S != 0) as u64,
                Flag::Overflow => (psw & PSW_OV != 0) as u64,
                Flag::Mie => (psw & PSW_MIE != 0) as u64,
                Flag::HalfCarry => (psw & PSW_HC != 0) as u64,
                Flag::Elevel => regs.elevel() as u64,
            }
        }
    }
}

/// Look up a register by name, or `None` when the name is not one.
pub fn parse_register(name: &str) -> Option<Reg> {
    if let Some(index) = index_suffix(name, "qr") {
        return (index % 8 == 0).then_some(Reg::Qr(index));
    }
    if let Some(index) = index_suffix(name, "xr") {
        return (index % 4 == 0).then_some(Reg::Xr(index));
    }
    if let Some(index) = index_suffix(name, "er") {
        return (index % 2 == 0).then_some(Reg::Word(index));
    }
    if let Some(index) = index_suffix(name, "r") {
        return (index < 16).then_some(Reg::Byte(index));
    }
    Some(match name {
        "sp" => Reg::Sp,
        "pc" => Reg::Pc,
        "csr" => Reg::Csr,
        "psw" => Reg::Psw,
        "ea" => Reg::Ea,
        "lr" => Reg::Lr,
        "lcsr" => Reg::Lcsr,
        "dsr" => Reg::Dsr,
        "c" => Reg::Flag(Flag::Carry),
        "z" => Reg::Flag(Flag::Zero),
        "s" => Reg::Flag(Flag::Sign),
        "ov" => Reg::Flag(Flag::Overflow),
        "mie" => Reg::Flag(Flag::Mie),
        "hc" => Reg::Flag(Flag::HalfCarry),
        "elevel" => Reg::Flag(Flag::Elevel),
        _ => return None,
    })
}

/// `er10` -> `Some(10)`, `x` -> `None`.
fn index_suffix(name: &str, prefix: &str) -> Option<u8> {
    let rest = name.strip_prefix(prefix)?;
    if rest.is_empty() || !rest.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

/// Look up a symbol's address by name.
pub fn parse_symbol(name: &str) -> Option<u32> {
    SYMBOLS
        .iter()
        .find(|(symbol, _)| *symbol == name)
        .map(|(_, address)| *address)
}

// ------------------------------------------------------------------- tokenizer

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Number(u64),
    Name(String),
    Open,
    Close,
    BracketOpen,
    BracketClose,
    Not,
    And,
    Or,
    Compare(Cmp),
}

fn tokenize(source: &str) -> Result<Vec<Token>, ParseError> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        match byte {
            b'(' => {
                tokens.push(Token::Open);
                index += 1;
            }
            b')' => {
                tokens.push(Token::Close);
                index += 1;
            }
            b'[' => {
                tokens.push(Token::BracketOpen);
                index += 1;
            }
            b']' => {
                tokens.push(Token::BracketClose);
                index += 1;
            }
            b'!' => {
                if bytes.get(index + 1) == Some(&b'=') {
                    tokens.push(Token::Compare(Cmp::Ne));
                    index += 2;
                } else {
                    tokens.push(Token::Not);
                    index += 1;
                }
            }
            b'=' => {
                if bytes.get(index + 1) == Some(&b'=') {
                    tokens.push(Token::Compare(Cmp::Eq));
                    index += 2;
                } else {
                    return Err(ParseError {
                        message: "use `==` to compare, not `=`".to_string(),
                        position: index,
                    });
                }
            }
            b'&' => {
                if bytes.get(index + 1) == Some(&b'&') {
                    tokens.push(Token::And);
                    index += 2;
                } else {
                    return Err(ParseError {
                        message: "use `&&` for and, not `&`".to_string(),
                        position: index,
                    });
                }
            }
            b'|' => {
                if bytes.get(index + 1) == Some(&b'|') {
                    tokens.push(Token::Or);
                    index += 2;
                } else {
                    return Err(ParseError {
                        message: "use `||` for or, not `|`".to_string(),
                        position: index,
                    });
                }
            }
            b'<' | b'>' => {
                let comparison = match (byte, bytes.get(index + 1)) {
                    (b'<', Some(b'=')) => Cmp::Le,
                    (b'>', Some(b'=')) => Cmp::Ge,
                    (b'<', _) => Cmp::Lt,
                    _ => Cmp::Gt,
                };
                let width = if matches!(bytes.get(index + 1), Some(b'=')) {
                    2
                } else {
                    1
                };
                tokens.push(Token::Compare(comparison));
                index += width;
            }
            _ if byte.is_ascii_digit() => {
                let (number, width) =
                    crate::number::parse_prefix(&source[index..]).ok_or_else(|| ParseError {
                        message: "could not read that number".to_string(),
                        position: index,
                    })?;
                tokens.push(Token::Number(number));
                index += width;
            }
            // `$10` is the other hex prefix the command line accepts, so it has to
            // work here too.  It cannot be folded into the arm above because that
            // one is reached only on a leading digit.
            b'$' => {
                let (number, width) =
                    crate::number::parse_prefix(&source[index..]).ok_or_else(|| ParseError {
                        message: "`$` must be followed by hex digits".to_string(),
                        position: index,
                    })?;
                tokens.push(Token::Number(number));
                index += width;
            }
            _ if byte.is_ascii_alphabetic() || byte == b'_' => {
                let start = index;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                }
                tokens.push(Token::Name(source[start..index].to_ascii_lowercase()));
            }
            other => {
                return Err(ParseError {
                    message: format!("unexpected character {:?}", other as char),
                    position: index,
                });
            }
        }
    }
    Ok(tokens)
}

// ---------------------------------------------------------------------- parser

struct Parser {
    tokens: Vec<Token>,
    position: usize,
    source_len: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.position).cloned();
        if token.is_some() {
            self.position += 1;
        }
        token
    }

    /// A byte offset to blame, approximated by token position since the tokenizer
    /// does not keep offsets.  Good enough to point at the offending clause.
    fn offset(&self) -> usize {
        if self.tokens.is_empty() {
            return self.source_len;
        }
        self.source_len * self.position / self.tokens.len()
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.next();
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_comparison()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.next();
            let right = self.parse_comparison()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expr, ParseError> {
        let left = self.parse_unary()?;
        if let Some(Token::Compare(op)) = self.peek().cloned() {
            self.next();
            let right = self.parse_unary()?;
            return Ok(Expr::Compare(Box::new(left), op, Box::new(right)));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        if matches!(self.peek(), Some(Token::Not)) {
            self.next();
            let inner = self.parse_unary()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let position = self.offset();
        match self.next() {
            Some(Token::Number(value)) => Ok(Expr::Number(value)),
            Some(Token::Open) => {
                let inner = self.parse_or()?;
                match self.next() {
                    Some(Token::Close) => Ok(inner),
                    _ => Err(ParseError {
                        message: "missing `)`".to_string(),
                        position,
                    }),
                }
            }
            Some(Token::BracketOpen) => {
                let inner = self.parse_or()?;
                match self.next() {
                    Some(Token::BracketClose) => Ok(Expr::Load(Box::new(inner))),
                    _ => Err(ParseError {
                        message: "missing `]`".to_string(),
                        position,
                    }),
                }
            }
            Some(Token::Name(name)) => {
                if name == "hits" || name == "hitcount" {
                    return Ok(Expr::Hits);
                }
                if let Some(reg) = parse_register(&name) {
                    return Ok(Expr::Register(reg));
                }
                if let Some(address) = parse_symbol(&name) {
                    return Ok(Expr::Address(address));
                }
                // A bare address, the way the project's notes write them: `D180`
                // starts with a letter, so the tokenizer reads it as a name before
                // any number rule can see it.  Registers and symbols are matched
                // above, so this only catches what neither claims -- and the shared
                // grammar only treats a run as hex when it is long enough and holds
                // a hex letter, which no symbol here does.
                if let Some(address) = crate::number::parse_u32(&name) {
                    return Ok(Expr::Address(address));
                }
                Err(ParseError {
                    message: format!(
                        "unknown name {name:?}; try a register like `r0`, a symbol like \
                         `filter`, `hits`, or `0x`-prefixed hex"
                    ),
                    position,
                })
            }
            Some(token) => Err(ParseError {
                message: format!("expected a value, found {token:?}"),
                position,
            }),
            None => Err(ParseError {
                message: "the condition is empty".to_string(),
                position,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fx991_bus::{Bus, Rom};

    /// A bus with known bytes in the two places a condition reads from.
    ///
    /// `0xD180` is RAM and `0xF042` an SFR, so neither comes from the ROM image;
    /// the values are written through the data path, which is the only way they
    /// can be there.
    fn bus() -> Bus {
        use nxu8_core::Memory;

        let mut bus = Bus::new(Rom::new(vec![0u8; 0x2_0000]));
        bus.write_data(0xD180, 0x41);
        bus.write_data(0xD181, 0x00);
        // 0xF042 is the keyboard filter, which the chipset owns; register a stub so
        // a condition can read a known value from it.
        bus.register(Box::new(KnownFilter));
        bus
    }

    /// A stand-in for the keyboard's input filter at `0xF042`.
    struct KnownFilter;

    impl fx991_bus::SfrDevice for KnownFilter {
        fn read(&mut self, address: u32) -> Option<u8> {
            (address == 0xF042).then_some(0xFF)
        }
        fn write(&mut self, _address: u32, _value: u8) -> bool {
            false
        }
        fn name(&self) -> &'static str {
            "test-filter"
        }
    }

    /// Evaluate against a register file with a couple of known values.
    fn evaluate(text: &str, hits: u64, setup: impl FnOnce(&mut Regs)) -> bool {
        let expr = Expr::parse(text).expect("parses");
        let mut regs = Regs::new();
        setup(&mut regs);
        let mut bus = bus();
        let mut context = Context {
            regs: &regs,
            bus: &mut bus,
            hits,
        };
        expr.eval(&mut context)
    }

    /// The value an expression naming an address resolves to, however it is written.
    fn number_value(text: &str) -> u64 {
        match Expr::parse(text).expect("parses") {
            Expr::Number(value) => value,
            Expr::Address(address) => address as u64,
            other => panic!("{text} parsed as {other:?}, which is not a plain value"),
        }
    }

    #[test]
    fn a_bare_address_parses_the_way_the_command_line_reads_it() {
        // `D180` begins with a letter, so the tokenizer calls it a name; the parser
        // then has to recognise it as an address, or the same notation that works in
        // `m D180` would be an error inside `if [D180] == 1`.
        //
        // Asserted by value rather than by variant: a leading digit goes through the
        // number rule and a leading letter through the name rule, so the two forms
        // produce different `Expr` variants for the same place.  `value()` treats
        // them alike, which is what a condition depends on.
        for text in [
            "D180", "d180", "0xD180", "0XD180", "$D180", "D180h", "D180H",
        ] {
            assert_eq!(number_value(text), 0xD180, "{text}");
        }
        for text in ["121A8", "0x121A8", "121A8h"] {
            assert_eq!(number_value(text), 0x1_21A8, "{text}");
        }
        // A plain decimal is still a plain decimal.
        assert_eq!(number_value("16"), 16);
    }

    #[test]
    fn a_symbol_or_register_name_is_never_read_as_an_address() {
        // The address fallback runs last, so every name the tables define still
        // resolves to itself.  This is the property that keeps the fallback safe.
        for (name, _) in SYMBOLS {
            assert!(
                crate::number::parse_u32(name).is_none(),
                "symbol {name:?} would be shadowed by the address fallback"
            );
        }
        // A symbol still resolves to its own address, not to something the number
        // grammar made up.
        assert_eq!(Expr::parse("filter").unwrap(), Expr::Address(0xF042));
        assert_eq!(Expr::parse("idle").unwrap(), Expr::Address(0x9216));
        // And a register is still a register.
        assert_eq!(Expr::parse("er0").unwrap(), Expr::Register(Reg::Word(0)));
    }

    #[test]
    fn numbers_parse_in_every_notation_the_plans_use() {
        for (text, expected) in [
            ("0", 0u64),
            ("12", 12),
            ("0x10", 16),
            ("0X10", 16),
            ("10h", 16),
            ("0b1010", 10),
            ("0xFF", 255),
        ] {
            let expr = Expr::parse(text).unwrap();
            assert_eq!(expr, Expr::Number(expected), "{text}");
        }
    }

    #[test]
    fn registers_use_the_width_their_name_implies() {
        assert_eq!(parse_register("r0"), Some(Reg::Byte(0)));
        assert_eq!(parse_register("r15"), Some(Reg::Byte(15)));
        assert_eq!(parse_register("er10"), Some(Reg::Word(10)));
        assert_eq!(parse_register("xr12"), Some(Reg::Xr(12)));
        assert_eq!(parse_register("qr8"), Some(Reg::Qr(8)));
        assert_eq!(parse_register("sp"), Some(Reg::Sp));
        assert_eq!(parse_register("unknown"), None);

        // A width that cannot hold that index is rejected rather than masked, so a
        // typo fails loudly instead of reading a different register.
        assert_eq!(parse_register("r16"), None);
        assert_eq!(parse_register("er11"), None, "ER names are even");
        assert_eq!(
            parse_register("xr2"),
            None,
            "XR names are multiples of four"
        );
        assert_eq!(parse_register("qr4"), None, "QR names are 0 or 8");
    }

    #[test]
    fn a_register_aliases_the_same_bytes_at_every_width() {
        // `set_er(0, 0xBEEF)` puts EF in R0 and BE in R1; the wider names must
        // observe that rather than keeping their own copy.
        assert!(evaluate("er0 == 0xBEEF", 0, |regs| regs.set_er(0, 0xBEEF)));
        assert!(evaluate("r0 == 0xEF", 0, |regs| regs.set_er(0, 0xBEEF)));
        assert!(evaluate("r1 == 0xBE", 0, |regs| regs.set_er(0, 0xBEEF)));
    }

    #[test]
    fn a_memory_reference_reads_the_data_space() {
        // 0xD180 holds 0x41 in the fixture.
        assert!(evaluate("[0xD180] == 0x41", 0, |_| {}));
        assert!(!evaluate("[0xD180] == 0x42", 0, |_| {}));
        assert!(evaluate("[0xD181] == 0", 0, |_| {}));
    }

    #[test]
    fn a_symbol_names_an_address_and_can_be_dereferenced() {
        assert!(evaluate("[filter] == 0xFF", 0, |_| {}));
        assert!(evaluate("filter == 0xF042", 0, |_| {}));
        assert!(evaluate("[input] == 0x41", 0, |_| {}));
        // An unknown name is an error, not a silent zero.
        assert!(Expr::parse("nonsense").is_err());
    }

    #[test]
    fn comparison_operators_behave() {
        assert!(evaluate("1 == 1", 0, |_| {}));
        assert!(evaluate("1 != 2", 0, |_| {}));
        assert!(evaluate("1 < 2", 0, |_| {}));
        assert!(evaluate("2 <= 2", 0, |_| {}));
        assert!(evaluate("3 > 2", 0, |_| {}));
        assert!(!evaluate("3 >= 4", 0, |_| {}));
    }

    #[test]
    fn boolean_operators_and_grouping_work() {
        assert!(evaluate("1 && 1", 0, |_| {}));
        assert!(!evaluate("1 && 0", 0, |_| {}));
        assert!(evaluate("0 || 1", 0, |_| {}));
        assert!(!evaluate("!1", 0, |_| {}));
        assert!(evaluate("!(0)", 0, |_| {}));
        assert!(evaluate("(1 || 0) && 1", 0, |_| {}));
        // And binds tighter than or.
        assert!(evaluate("0 && 0 || 1", 0, |_| {}));
    }

    #[test]
    fn a_bare_operand_is_true_when_non_zero() {
        assert!(evaluate("1", 0, |_| {}));
        assert!(!evaluate("0", 0, |_| {}));
        assert!(evaluate("r0", 0, |regs| regs.r[0] = 1));
        assert!(!evaluate("r0", 0, |regs| regs.r[0] = 0));
        assert!(evaluate("[input]", 0, |_| {}), "0x41 is not zero");
        assert!(!evaluate("[0xD181]", 0, |_| {}), "0 is false");
    }

    #[test]
    fn hits_is_the_breakpoints_own_counter() {
        assert!(evaluate("hits == 0", 0, |_| {}));
        assert!(evaluate("hits == 3", 3, |_| {}));
        assert!(evaluate("hits > 2", 3, |_| {}));
        // x64dbg calls this hitcount; accept both spellings.
        assert!(evaluate("hitcount == 3", 3, |_| {}));
    }

    #[test]
    fn psw_bits_are_addressable_by_name() {
        assert!(evaluate("z", 0, |regs| regs.set_psw(PSW_Z)));
        assert!(evaluate("c == 0", 0, |regs| regs.set_psw(PSW_Z)));
        assert!(evaluate("mie", 0, |regs| regs.set_psw(PSW_MIE)));
        assert!(evaluate("ov", 0, |regs| regs.set_psw(PSW_OV)));
        assert!(evaluate("s", 0, |regs| regs.set_psw(PSW_S)));
        assert!(evaluate("hc", 0, |regs| regs.set_psw(PSW_HC)));
        assert!(evaluate("elevel == 0", 0, |_| {}));
        assert!(evaluate("psw == 0x40", 0, |regs| regs.set_psw(PSW_Z)));
    }

    #[test]
    fn a_parse_error_says_what_is_wrong_and_roughly_where() {
        let error = Expr::parse("r0 = 1").unwrap_err();
        assert!(error.message.contains("=="), "{error}");

        let error = Expr::parse("1 & 1").unwrap_err();
        assert!(error.message.contains("&&"), "{error}");

        let error = Expr::parse("[0xD180").unwrap_err();
        assert!(error.message.contains(']'), "{error}");

        let error = Expr::parse("(1").unwrap_err();
        assert!(error.message.contains(')'), "{error}");

        let error = Expr::parse("").unwrap_err();
        assert!(error.message.contains("empty"), "{error}");

        let error = Expr::parse("1 2").unwrap_err();
        assert!(error.message.contains("unexpected"), "{error}");

        assert!(error.position <= "r0 = 1".len());
    }

    #[test]
    fn evaluating_a_condition_does_not_disturb_the_machine() {
        // A condition reading memory must not look like the guest reading it, or
        // testing a breakpoint would add faults and trip watchpoints.
        let mut bus = bus();
        bus.watch(fx991_bus::Watch::data(0xF042));
        let regs = Regs::new();
        let expr = Expr::parse("[filter] == 0xFF").unwrap();

        let mut context = Context {
            regs: &regs,
            bus: &mut bus,
            hits: 0,
        };
        assert!(expr.eval(&mut context));
        assert_eq!(bus.fault_count, 0, "no fault was recorded");
        assert!(bus.take_watch_hits().is_empty(), "no watch fired");

        // Reading an unmapped address in a condition is also silent.
        let expr = Expr::parse("[0x60000] == 0").unwrap();
        let mut context = Context {
            regs: &regs,
            bus: &mut bus,
            hits: 0,
        };
        assert!(expr.eval(&mut context));
        assert_eq!(bus.fault_count, 0);
    }

    #[test]
    fn and_short_circuits_so_an_unneeded_read_does_not_happen() {
        // The right-hand side reads an unmapped address; if it were evaluated the
        // quiet read would still be silent, so the observable proof is that the
        // expression is false without the read mattering.
        let expr = Expr::parse("[0xD181] != 0 && [0x60000] == 0").unwrap();
        let mut regs = Regs::new();
        regs.r[0] = 0;
        let mut bus = bus();
        let mut context = Context {
            regs: &regs,
            bus: &mut bus,
            hits: 0,
        };
        // 0xD181 is 0, so the and fails and the second clause is not reached.
        assert!(!expr.eval(&mut context));
    }

    #[test]
    fn registers_reports_what_the_condition_depends_on() {
        let expr = Expr::parse("r0 == 1 && er4 != 0 || [sp] == 2").unwrap();
        let registers = expr.registers();
        assert!(registers.contains(&Reg::Byte(0)));
        assert!(registers.contains(&Reg::Word(4)));
        assert!(registers.contains(&Reg::Sp));

        // A constant condition depends on nothing.
        assert!(Expr::parse("1 == 1").unwrap().registers().is_empty());
    }

    #[test]
    fn the_symbol_table_has_no_duplicate_names() {
        for (index, (name, _)) in SYMBOLS.iter().enumerate() {
            assert!(
                !SYMBOLS[index + 1..].iter().any(|(other, _)| other == name),
                "{name} is listed twice"
            );
        }
    }
}
