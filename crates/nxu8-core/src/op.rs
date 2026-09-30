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

//! The handler identity: one `Op` per handler, and its name.
//!
//! Dispatch runs through [`Op`] rather than through the handler's name, so the
//! hot path is an array index and a jump instead of a chain of string
//! comparisons.  The name survives for everything that *reports* an instruction
//! -- the disassembler, the trace ring, the debugger's views -- because those
//! want the text, not the tag.
//!
//! [`Op::name`] is the single source of that text, so a tag and the name a tool
//! prints can never drift apart.

/// Which handler an opcode decodes to.
///
/// The variant order is the order of the generated table's rows, and
/// [`Op::ALL`] follows it, so the tag a regenerated table carries is stable.
///
/// The variants are not individually documented: the mnemonic each one
/// implements is exactly [`Op::name`], and the handler it runs carries the
/// translation notes.  Spelling them out again here would be a third copy of the
/// same fact.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    Add,
    Add16,
    Addc,
    And,
    Mov16,
    Mov,
    Or,
    Xor,
    Cmp16,
    Sub,
    Subc,
    Daa,
    Das,
    Neg,
    Mul,
    Div,
    IncEa,
    DecEa,
    Sll,
    Sllc,
    Sra,
    Srl,
    Srlc,
    Bitmod,
    Extbw,
    LsEa,
    LsR,
    LsIR,
    LsBp,
    LsFp,
    LsI,
    Push,
    Pushl,
    Pop,
    Popl,
    Addsp,
    Ctrl,
    Lea,
    PswOr,
    PswAnd,
    Cplc,
    Bc,
    Swi,
    Brk,
    B,
    Bl,
    Rt,
    Rti,
    Nop,
    Dsr,
    CrR,
    CrEa,
}

impl Op {
    /// Every variant, in declaration order.
    pub const ALL: &'static [Op] = &[
        Op::Add,
        Op::Add16,
        Op::Addc,
        Op::And,
        Op::Mov16,
        Op::Mov,
        Op::Or,
        Op::Xor,
        Op::Cmp16,
        Op::Sub,
        Op::Subc,
        Op::Daa,
        Op::Das,
        Op::Neg,
        Op::Mul,
        Op::Div,
        Op::IncEa,
        Op::DecEa,
        Op::Sll,
        Op::Sllc,
        Op::Sra,
        Op::Srl,
        Op::Srlc,
        Op::Bitmod,
        Op::Extbw,
        Op::LsEa,
        Op::LsR,
        Op::LsIR,
        Op::LsBp,
        Op::LsFp,
        Op::LsI,
        Op::Push,
        Op::Pushl,
        Op::Pop,
        Op::Popl,
        Op::Addsp,
        Op::Ctrl,
        Op::Lea,
        Op::PswOr,
        Op::PswAnd,
        Op::Cplc,
        Op::Bc,
        Op::Swi,
        Op::Brk,
        Op::B,
        Op::Bl,
        Op::Rt,
        Op::Rti,
        Op::Nop,
        Op::Dsr,
        Op::CrR,
        Op::CrEa,
    ];

    /// The handler's name, as the table spells it.
    pub fn name(self) -> &'static str {
        match self {
            Op::Add => "OP_ADD",
            Op::Add16 => "OP_ADD16",
            Op::Addc => "OP_ADDC",
            Op::And => "OP_AND",
            Op::Mov16 => "OP_MOV16",
            Op::Mov => "OP_MOV",
            Op::Or => "OP_OR",
            Op::Xor => "OP_XOR",
            Op::Cmp16 => "OP_CMP16",
            Op::Sub => "OP_SUB",
            Op::Subc => "OP_SUBC",
            Op::Daa => "OP_DAA",
            Op::Das => "OP_DAS",
            Op::Neg => "OP_NEG",
            Op::Mul => "OP_MUL",
            Op::Div => "OP_DIV",
            Op::IncEa => "OP_INC_EA",
            Op::DecEa => "OP_DEC_EA",
            Op::Sll => "OP_SLL",
            Op::Sllc => "OP_SLLC",
            Op::Sra => "OP_SRA",
            Op::Srl => "OP_SRL",
            Op::Srlc => "OP_SRLC",
            Op::Bitmod => "OP_BITMOD",
            Op::Extbw => "OP_EXTBW",
            Op::LsEa => "OP_LS_EA",
            Op::LsR => "OP_LS_R",
            Op::LsIR => "OP_LS_I_R",
            Op::LsBp => "OP_LS_BP",
            Op::LsFp => "OP_LS_FP",
            Op::LsI => "OP_LS_I",
            Op::Push => "OP_PUSH",
            Op::Pushl => "OP_PUSHL",
            Op::Pop => "OP_POP",
            Op::Popl => "OP_POPL",
            Op::Addsp => "OP_ADDSP",
            Op::Ctrl => "OP_CTRL",
            Op::Lea => "OP_LEA",
            Op::PswOr => "OP_PSW_OR",
            Op::PswAnd => "OP_PSW_AND",
            Op::Cplc => "OP_CPLC",
            Op::Bc => "OP_BC",
            Op::Swi => "OP_SWI",
            Op::Brk => "OP_BRK",
            Op::B => "OP_B",
            Op::Bl => "OP_BL",
            Op::Rt => "OP_RT",
            Op::Rti => "OP_RTI",
            Op::Nop => "OP_NOP",
            Op::Dsr => "OP_DSR",
            Op::CrR => "OP_CR_R",
            Op::CrEa => "OP_CR_EA",
        }
    }

    /// The tag for a name, or `None` for an unknown one.
    ///
    /// Used when the table is built and by the tests that check the table
    /// against [`Op::ALL`]; the hot path never calls it.
    pub fn from_name(name: &str) -> Option<Op> {
        Op::ALL.iter().copied().find(|op| op.name() == name)
    }

    /// The dispatcher: run this handler.
    ///
    /// The `match` compiles to a jump table.  That is the point of carrying a
    /// tag instead of the handler's name: a name has to be compared against
    /// every candidate, and a tag indexes straight to the code.
    #[inline]
    pub fn run<B: crate::Memory, S: crate::ops::ctrl::ControlSink>(
        self,
        cpu: &mut crate::Cpu,
        bus: &mut B,
        sink: &mut S,
    ) {
        use crate::ops::{arith, coprocessor, ctrl, loadstore, pushpop, shift};
        match self {
            // --- arithmetic ---------------------------------------------------
            Op::Add => arith::op_add(cpu),
            Op::Add16 => arith::op_add16(cpu),
            Op::Addc => arith::op_addc(cpu),
            Op::And => arith::op_and(cpu),
            Op::Mov16 => arith::op_mov16(cpu),
            Op::Mov => arith::op_mov(cpu),
            Op::Or => arith::op_or(cpu),
            Op::Xor => arith::op_xor(cpu),
            Op::Cmp16 => arith::op_cmp16(cpu),
            Op::Sub => arith::op_sub(cpu),
            Op::Subc => arith::op_subc(cpu),
            Op::Daa => arith::op_daa(cpu),
            Op::Das => arith::op_das(cpu),
            Op::Neg => arith::op_neg(cpu),
            Op::Mul => arith::op_mul(cpu),
            Op::Div => arith::op_div(cpu),
            Op::IncEa => arith::op_inc_ea(cpu, bus),
            Op::DecEa => arith::op_dec_ea(cpu, bus),
            // --- shift / bit ---------------------------------------------------
            Op::Sll => shift::op_sll(cpu),
            Op::Sllc => shift::op_sllc(cpu),
            Op::Sra => shift::op_sra(cpu),
            Op::Srl => shift::op_srl(cpu),
            Op::Srlc => shift::op_srlc(cpu),
            Op::Bitmod => shift::op_bitmod(cpu, bus),
            Op::Extbw => shift::op_extbw(cpu),
            // --- load / store --------------------------------------------------
            Op::LsEa => loadstore::op_ls_ea(cpu, bus),
            Op::LsR => loadstore::op_ls_r(cpu, bus),
            Op::LsIR => loadstore::op_ls_i_r(cpu, bus),
            Op::LsBp => loadstore::op_ls_bp(cpu, bus),
            Op::LsFp => loadstore::op_ls_fp(cpu, bus),
            Op::LsI => loadstore::op_ls_i(cpu, bus),
            // --- push / pop ----------------------------------------------------
            Op::Push => pushpop::op_push(cpu, bus),
            Op::Pushl => pushpop::op_pushl(cpu, bus),
            Op::Pop => pushpop::op_pop(cpu, bus),
            Op::Popl => pushpop::op_popl(cpu, bus),
            // --- control -------------------------------------------------------
            Op::Addsp => ctrl::op_addsp(cpu),
            Op::Ctrl => ctrl::op_ctrl(cpu),
            Op::Lea => ctrl::op_lea(cpu),
            Op::PswOr => ctrl::op_psw_or(cpu),
            Op::PswAnd => ctrl::op_psw_and(cpu),
            Op::Cplc => ctrl::op_cplc(cpu),
            Op::Bc => ctrl::op_bc(cpu),
            Op::Swi => ctrl::op_swi(cpu, sink),
            Op::Brk => ctrl::op_brk(cpu, sink),
            Op::B => ctrl::op_b(cpu),
            Op::Bl => ctrl::op_bl(cpu),
            Op::Rt => ctrl::op_rt(cpu),
            Op::Rti => ctrl::op_rti(cpu),
            Op::Nop => ctrl::op_nop(cpu),
            Op::Dsr => ctrl::op_dsr(cpu),
            // --- coprocessor (unimplemented on purpose) ------------------------
            Op::CrR => coprocessor::op_cr_r(cpu),
            Op::CrEa => coprocessor::op_cr_ea(cpu),
        }
    }
}
