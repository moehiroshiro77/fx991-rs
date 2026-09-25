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

//! Coprocessor data-transfer handlers.
//!
//! The nX-U8 core has an optional coprocessor register bank (`CRn`/`CERn`/`CXRn`/
//! `CQRn`).  VerF's ROM never touches it and  lists it as an explicit
//! non-goal, so reaching one is a loud, located failure rather than a plausible
//! no-op that would silently corrupt a chain.

use crate::{Cpu, Unimplemented};

pub fn op_cr_r(cpu: &mut Cpu) {
    let opcode = cpu.opcode;
    let pc = cpu.regs.pc;
    let csr = cpu.regs.csr;
    cpu.unimplemented = Some(Unimplemented { opcode, pc, csr });
}

pub fn op_cr_ea(cpu: &mut Cpu) {
    let opcode = cpu.opcode;
    let pc = cpu.regs.pc;
    let csr = cpu.regs.csr;
    cpu.unimplemented = Some(Unimplemented { opcode, pc, csr });
}
