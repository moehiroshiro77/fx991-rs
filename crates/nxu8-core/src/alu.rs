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

//! Flag arithmetic, following the manual's per-instruction flag tables.
//!
//! The subtle parts, copied rather than "cleaned up":
//!
//! * `impl_flags_out` starts at [`PSW_Z`] and [`zs_check`] only *clears* Z when
//!   the value is non-zero.  Writing zero and not
//!   touching the flags are therefore distinguishable.
//! * `add8` recomputes C/OV/HC unconditionally; OV is `carry_8 ^ carry_7`.
//! * `OP_SUB` is "clear C, set Z, then SUBC"; `SUBC` complements the operand, adds,
//!   and complements back.
//! * The merge back into PSW only touches bits in `impl_flags_changed`.

use crate::regs::{PSW_C, PSW_HC, PSW_OV, PSW_S, PSW_Z};
use crate::Cpu;

/// `Add8` with the carry-in taken from `cpu.flags_in`.
#[inline]
pub fn add8(cpu: &mut Cpu) {
    let op0 = (cpu.operands[0].value & 0xFF) as u8;
    let op1 = (cpu.operands[1].value & 0xFF) as u8;
    let c_in = u8::from(cpu.flags_in & PSW_C != 0);

    let carry_8 = ((op0 as u16) + (op1 as u16) + (c_in as u16)) >> 8;
    let carry_7 = (((op0 & 0x7F) as u16) + ((op1 & 0x7F) as u16) + (c_in as u16)) >> 7;
    let carry_4 = (((op0 & 0x0F) as u16) + ((op1 & 0x0F) as u16) + (c_in as u16)) >> 4;

    cpu.flags_changed |= PSW_C | PSW_OV | PSW_HC;
    cpu.flags_out = (cpu.flags_out & !PSW_C) | if carry_8 != 0 { PSW_C } else { 0 };
    cpu.flags_out = (cpu.flags_out & !PSW_OV) | if carry_8 ^ carry_7 != 0 { PSW_OV } else { 0 };
    cpu.flags_out = (cpu.flags_out & !PSW_HC) | if carry_4 != 0 { PSW_HC } else { 0 };

    cpu.operands[0].value = op0.wrapping_add(op1).wrapping_add(c_in) as u64;
}

/// `ZSCheck`: clears Z for a non-zero result, sets S from bit 7.
#[inline]
pub fn zs_check(cpu: &mut Cpu) {
    cpu.flags_changed |= PSW_Z | PSW_S;
    if cpu.operands[0].value & 0xFF != 0 {
        cpu.flags_out &= !PSW_Z;
    }
    cpu.flags_out = (cpu.flags_out & !PSW_S)
        | if cpu.operands[0].value & 0x80 != 0 {
            PSW_S
        } else {
            0
        };
}

/// `ShiftLeft8`.
///
/// Computes `(uint16)value << shift_by | shift_buffer >> (8 - shift_by)`
/// with **no special case for `shift_by == 0`**: then the buffer term is a shift
/// by 8, which is zero, and the carry-out is taken from bit 8 of the value shift
/// alone.  Replicated literally -- a "shift by 0 means carry-in from the buffer"
/// reading would be wrong.
#[inline]
pub fn shift_left8(cpu: &mut Cpu) {
    cpu.operands[0].value &= 0xFF;
    let shift_by = (cpu.operands[1].value & 7) as u32;
    let low = cpu.operands[0].value as u32;
    let buffer = cpu.shift_buffer as u32;
    // Computed in u32 so the 16-bit truncation is explicit.
    let result = ((low << shift_by) | (buffer >> (8 - shift_by))) & 0xFFFF;
    cpu.flags_changed |= PSW_C;
    if result & 0x100 != 0 {
        cpu.flags_out |= PSW_C;
    }
    cpu.operands[0].value = (result & 0xFF) as u64;
}

/// `ShiftRight8`, same literal transcription.
#[inline]
pub fn shift_right8(cpu: &mut Cpu) {
    cpu.operands[0].value &= 0xFF;
    let shift_by = (cpu.operands[1].value & 7) as u32;
    let low = cpu.operands[0].value as u32;
    let buffer = cpu.shift_buffer as u32;
    let result = ((low << (8 - shift_by)) | (buffer << (16 - shift_by))) & 0xFFFF;
    cpu.flags_changed |= PSW_C;
    if result & 0x80 != 0 {
        cpu.flags_out |= PSW_C;
    }
    cpu.operands[0].value = ((result >> 8) & 0xFF) as u64;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cpu;

    fn cpu_with(op0: u64, op1: u64, flags_in: u8) -> Cpu {
        let mut cpu = Cpu::new();
        cpu.operands[0].value = op0;
        cpu.operands[1].value = op1;
        cpu.flags_in = flags_in;
        cpu.flags_out = PSW_Z;
        cpu.flags_changed = 0;
        cpu
    }

    #[test]
    fn add_sets_carry_and_zero_on_ff_plus_one() {
        let mut cpu = cpu_with(0xFF, 1, 0);
        add8(&mut cpu);
        assert_eq!(cpu.operands[0].value, 0);
        assert_ne!(cpu.flags_out & PSW_C, 0);
        assert_ne!(cpu.flags_changed & PSW_C, 0);
    }

    #[test]
    fn add_reports_signed_overflow_without_carry() {
        let mut cpu = cpu_with(0x7F, 1, 0);
        add8(&mut cpu);
        assert_eq!(cpu.operands[0].value, 0x80);
        assert_ne!(cpu.flags_out & PSW_OV, 0);
        assert_eq!(cpu.flags_out & PSW_C, 0, "no unsigned carry");
    }

    #[test]
    fn add_reports_half_carry_out_of_the_low_nibble() {
        let mut cpu = cpu_with(0x0F, 1, 0);
        add8(&mut cpu);
        assert_ne!(cpu.flags_out & PSW_HC, 0);
    }

    #[test]
    fn add8_uses_the_carry_in() {
        let mut cpu = cpu_with(0xFF, 0, PSW_C);
        add8(&mut cpu);
        assert_eq!(cpu.operands[0].value, 0);
        assert_ne!(cpu.flags_out & PSW_C, 0);
    }

    #[test]
    fn zs_check_only_clears_z_for_a_non_zero_value() {
        let mut cpu = cpu_with(0, 0, 0);
        zs_check(&mut cpu);
        assert_ne!(cpu.flags_out & PSW_Z, 0, "Z stays set for zero");

        let mut cpu = cpu_with(1, 0, 0);
        zs_check(&mut cpu);
        assert_eq!(cpu.flags_out & PSW_Z, 0, "Z cleared for non-zero");
    }

    #[test]
    fn zs_check_sets_the_sign_bit_from_bit_seven() {
        let mut cpu = cpu_with(0x80, 0, 0);
        zs_check(&mut cpu);
        assert_ne!(cpu.flags_out & PSW_S, 0);

        let mut cpu = cpu_with(0x7F, 0, 0);
        zs_check(&mut cpu);
        assert_eq!(cpu.flags_out & PSW_S, 0);
    }

    #[test]
    fn shift_left_by_zero_leaves_the_value_and_clears_carry() {
        // `>> (8 - 0)` is `>> 8`, i.e. zero: the buffer does *not* shift in, and
        // bit 8 of the unshifted value is clear.
        let mut cpu = cpu_with(0x80, 0, 0);
        cpu.shift_buffer = 0xFF;
        shift_left8(&mut cpu);
        assert_eq!(cpu.operands[0].value, 0x80);
        assert_eq!(cpu.flags_out & PSW_C, 0, "no carry out of bit 8");
    }

    #[test]
    fn shift_left_by_one_shifts_the_value_and_reports_carry() {
        let mut cpu = cpu_with(0x80, 1, 0);
        cpu.shift_buffer = 0;
        shift_left8(&mut cpu);
        assert_eq!(cpu.operands[0].value, 0);
        assert_ne!(cpu.flags_out & PSW_C, 0);
    }

    #[test]
    fn shift_left_by_one_shifts_in_bit_seven_of_the_buffer_as_bit_zero() {
        // `SLLC Rn` takes the *previous* byte register: `buffer >> 7` puts its
        // MSB into the destination's LSB.
        let mut cpu = cpu_with(0x00, 1, 0);
        cpu.shift_buffer = 0x01;
        shift_left8(&mut cpu);
        assert_eq!(cpu.operands[0].value, 0x00, "buffer>>7 of 0x01 is zero");

        let mut cpu = cpu_with(0x00, 1, 0);
        cpu.shift_buffer = 0x80;
        shift_left8(&mut cpu);
        assert_eq!(cpu.operands[0].value, 0x01);
    }

    #[test]
    fn shift_right_by_zero_is_a_no_op_with_no_carry() {
        // `value << 8` lands in the high byte and `>> 8` brings it back, so the
        // value survives; the low byte of the accumulator stays 0, so C is clear.
        let mut cpu = cpu_with(0x80, 0, 0);
        cpu.shift_buffer = 0xFF;
        shift_right8(&mut cpu);
        assert_eq!(cpu.operands[0].value, 0x80);
        assert_eq!(cpu.flags_out & PSW_C, 0, "bit 7 of the accumulator is 0");
    }

    #[test]
    fn shift_right_by_one_moves_bit_seven_of_the_value_into_carry() {
        let mut cpu = cpu_with(0x01, 1, 0);
        cpu.shift_buffer = 0x80;
        shift_right8(&mut cpu);
        assert_eq!(cpu.operands[0].value, 0x00, "the byte is emptied");
        assert_ne!(cpu.flags_out & PSW_C, 0, "C comes from bit 7 of the value");
    }

    #[test]
    fn shift_right_by_one_shifts_in_bit_zero_of_the_buffer_as_the_new_msb() {
        // This is what `SRLC Rn` means: the *next* byte register's LSB becomes
        // the destination's MSB.  The buffer term is `buffer << 15` truncated to
        // 16 bits, so only buffer bit 0 survives, landing at bit 15 -> bit 7.
        let mut cpu = cpu_with(0x00, 1, 0);
        cpu.shift_buffer = 0x01;
        shift_right8(&mut cpu);
        assert_eq!(cpu.operands[0].value, 0x80);
        assert_eq!(cpu.flags_out & PSW_C, 0, "bit 7 of the accumulator was 0");
    }
}
