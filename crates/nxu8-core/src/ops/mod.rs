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

//! Instruction handlers, grouped by family.
//!
//! Each handler is named after the mnemonic family it implements and the module
//! doc above it carries the translation notes, so the functions themselves are
//! deliberately left without per-function docs -- except where a handler has a
//! surprise worth flagging (the dummy stack byte, the CTRL selector packing, the
//! flag-merge mask), and those say so inline.
//!
//!  names a handler per opcode row and each functional group lives in its
//! own translation unit (,..).  Keeping the
//! same split makes the two implementations diffable group by group rather than
//! as one large function.

#![allow(missing_docs)]

pub mod arith;
pub mod coprocessor;
pub mod ctrl;
pub mod loadstore;
pub mod pushpop;
pub mod shift;

use crate::{Cpu, Memory};

/// Every handler name the opcode table may name.
///
/// Used by the decode tests to prove that a mismatched table is caught, and by
/// [`run`] to route a dispatch entry to its implementation.
pub const ALL_HANDLERS: &[&str] = &[
    // arith
    "OP_ADD",
    "OP_ADD16",
    "OP_ADDC",
    "OP_AND",
    "OP_MOV16",
    "OP_MOV",
    "OP_OR",
    "OP_XOR",
    "OP_CMP16",
    "OP_SUB",
    "OP_SUBC",
    "OP_DAA",
    "OP_DAS",
    "OP_NEG",
    "OP_MUL",
    "OP_DIV",
    "OP_INC_EA",
    "OP_DEC_EA",
    // shift / bit
    "OP_SLL",
    "OP_SLLC",
    "OP_SRA",
    "OP_SRL",
    "OP_SRLC",
    "OP_BITMOD",
    "OP_EXTBW",
    // load / store
    "OP_LS_EA",
    "OP_LS_R",
    "OP_LS_I_R",
    "OP_LS_BP",
    "OP_LS_FP",
    "OP_LS_I",
    // push / pop
    "OP_PUSH",
    "OP_PUSHL",
    "OP_POP",
    "OP_POPL",
    // control
    "OP_ADDSP",
    "OP_CTRL",
    "OP_LEA",
    "OP_PSW_OR",
    "OP_PSW_AND",
    "OP_CPLC",
    "OP_BC",
    "OP_SWI",
    "OP_BRK",
    "OP_B",
    "OP_BL",
    "OP_RT",
    "OP_RTI",
    "OP_NOP",
    "OP_DSR",
    // coprocessor (unimplemented on purpose)
    "OP_CR_R",
    "OP_CR_EA",
];

/// Run the named handler.  Returns `false` if the name is unknown.
///
/// `SWI`/`BRK` are routed to the caller's [`ControlSink`](ctrl::ControlSink) so this crate needs to
/// know nothing about any particular chipset.
pub fn run<B: Memory, S: ctrl::ControlSink>(
    cpu: &mut Cpu,
    bus: &mut B,
    sink: &mut S,
    handler: &str,
) -> bool {
    match handler {
        // --- arithmetic ---------------------------------------------------
        "OP_ADD" => arith::op_add(cpu),
        "OP_ADD16" => arith::op_add16(cpu),
        "OP_ADDC" => arith::op_addc(cpu),
        "OP_AND" => arith::op_and(cpu),
        "OP_MOV16" => arith::op_mov16(cpu),
        "OP_MOV" => arith::op_mov(cpu),
        "OP_OR" => arith::op_or(cpu),
        "OP_XOR" => arith::op_xor(cpu),
        "OP_CMP16" => arith::op_cmp16(cpu),
        "OP_SUB" => arith::op_sub(cpu),
        "OP_SUBC" => arith::op_subc(cpu),
        "OP_DAA" => arith::op_daa(cpu),
        "OP_DAS" => arith::op_das(cpu),
        "OP_NEG" => arith::op_neg(cpu),
        "OP_MUL" => arith::op_mul(cpu),
        "OP_DIV" => arith::op_div(cpu),
        "OP_INC_EA" => arith::op_inc_ea(cpu, bus),
        "OP_DEC_EA" => arith::op_dec_ea(cpu, bus),
        // --- shift / bit ---------------------------------------------------
        "OP_SLL" => shift::op_sll(cpu),
        "OP_SLLC" => shift::op_sllc(cpu),
        "OP_SRA" => shift::op_sra(cpu),
        "OP_SRL" => shift::op_srl(cpu),
        "OP_SRLC" => shift::op_srlc(cpu),
        "OP_BITMOD" => shift::op_bitmod(cpu, bus),
        "OP_EXTBW" => shift::op_extbw(cpu),
        // --- load / store --------------------------------------------------
        "OP_LS_EA" => loadstore::op_ls_ea(cpu, bus),
        "OP_LS_R" => loadstore::op_ls_r(cpu, bus),
        "OP_LS_I_R" => loadstore::op_ls_i_r(cpu, bus),
        "OP_LS_BP" => loadstore::op_ls_bp(cpu, bus),
        "OP_LS_FP" => loadstore::op_ls_fp(cpu, bus),
        "OP_LS_I" => loadstore::op_ls_i(cpu, bus),
        // --- push / pop ----------------------------------------------------
        "OP_PUSH" => pushpop::op_push(cpu, bus),
        "OP_PUSHL" => pushpop::op_pushl(cpu, bus),
        "OP_POP" => pushpop::op_pop(cpu, bus),
        "OP_POPL" => pushpop::op_popl(cpu, bus),
        // --- control -------------------------------------------------------
        "OP_ADDSP" => ctrl::op_addsp(cpu),
        "OP_CTRL" => ctrl::op_ctrl(cpu),
        "OP_LEA" => ctrl::op_lea(cpu),
        "OP_PSW_OR" => ctrl::op_psw_or(cpu),
        "OP_PSW_AND" => ctrl::op_psw_and(cpu),
        "OP_CPLC" => ctrl::op_cplc(cpu),
        "OP_BC" => ctrl::op_bc(cpu),
        "OP_SWI" => ctrl::op_swi(cpu, sink),
        "OP_BRK" => ctrl::op_brk(cpu, sink),
        "OP_B" => ctrl::op_b(cpu),
        "OP_BL" => ctrl::op_bl(cpu),
        "OP_RT" => ctrl::op_rt(cpu),
        "OP_RTI" => ctrl::op_rti(cpu),
        "OP_NOP" => ctrl::op_nop(cpu),
        "OP_DSR" => ctrl::op_dsr(cpu),
        // --- coprocessor ---------------------------------------------------
        "OP_CR_R" => coprocessor::op_cr_r(cpu),
        "OP_CR_EA" => coprocessor::op_cr_ea(cpu),
        _ => return false,
    }
    true
}
