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

//! Miscellaneous SFRs: DSR, standby control, and the "unknown" holes.
//!
//!  binds a fixed list of single-byte SFRs plus
//! two wider blobs at `0xF048` (8 bytes) and `0xF220` (4 bytes).  The single-byte
//! list is the `addr[]` array in.
//!
//! `StandbyControl` is folded in here because it
//! only needs two shadow bytes: `0xF008` arms the STOP acceptor (`0x5_` then
//! `0xA_`) and `0xF009` bit 0 halts, bit 1 stops.

use crate::interrupts::Interrupts;

/// `0xF000` -- the data segment register, stored in the CPU.
pub const DSR: u32 = 0x0_F000;
/// `0xF008` -- standby accept
pub const STPACP: u32 = 0x0_F008;
/// `0xF009` -- bits 0/1 request halt/stop
pub const SBYCON: u32 = 0x0_F009;

/// The single-byte "unknown" SFRs from  (`HW_CLASSWIZ`).
pub const SINGLE_BYTE_SFRS: [u32; 13] = [
    0xF00A, 0xF018, 0xF033, 0xF034, 0xF041, 0xF035, 0xF036, 0xF039, 0xF012, 0xF03D, 0xF224, 0xF028,
    0xF310,
];

/// What a write to `SBYCON` asked for, so the chipset can act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandbyRequest {
    /// `0xF008` armed the STOP acceptor.
    Armed,
    /// bit 0: halt until an interrupt.
    Halt,
    /// bit 1 with the acceptor armed: stop until an interrupt.
    Stop,
}

/// DSR, standby control, and the unknown SFR block.
pub struct Misc {
    bytes: [u8; SINGLE_BYTE_SFRS.len()],
    wide_f048: [u8; 8],
    wide_f220: [u8; 4],
    /// Last value written to `0xF008`.
    pub stpacp_last: u8,
    /// Whether `0xF008` has seen the `0x5_`-then-`0xA_` handshake.
    pub stop_acceptor_enabled: bool,
}

/// The DSR/standby/miscellaneous block's state, for snapshot and restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiscState {
    /// The single-byte "unknown" SFRs.
    pub bytes: Vec<u8>,
    /// The `0xF048..0xF050` block.
    pub wide_f048: [u8; 8],
    /// The `0xF220..0xF224` block.
    pub wide_f220: [u8; 4],
    /// Last value written to `0xF008`.
    pub stpacp_last: u8,
    /// Whether the `0x5_`-then-`0xA_` handshake has been seen.
    pub stop_acceptor_enabled: bool,
}

impl Default for Misc {
    fn default() -> Self {
        Self::new()
    }
}

impl Misc {
    /// Everything needed to put this block back where it was.
    pub fn snapshot(&self) -> MiscState {
        MiscState {
            bytes: self.bytes.to_vec(),
            wide_f048: self.wide_f048,
            wide_f220: self.wide_f220,
            stpacp_last: self.stpacp_last,
            stop_acceptor_enabled: self.stop_acceptor_enabled,
        }
    }

    /// Restore a [`MiscState`].
    pub fn restore(&mut self, state: &MiscState) {
        let length = state.bytes.len().min(self.bytes.len());
        self.bytes[..length].copy_from_slice(&state.bytes[..length]);
        self.wide_f048 = state.wide_f048;
        self.wide_f220 = state.wide_f220;
        self.stpacp_last = state.stpacp_last;
        self.stop_acceptor_enabled = state.stop_acceptor_enabled;
    }

    /// Everything zeroed.
    pub fn new() -> Self {
        Self {
            bytes: [0; SINGLE_BYTE_SFRS.len()],
            wide_f048: [0; 8],
            wide_f220: [0; 4],
            stpacp_last: 0,
            stop_acceptor_enabled: false,
        }
    }

    /// `Peripheral::Reset`.
    pub fn reset(&mut self) {
        self.bytes = [0; SINGLE_BYTE_SFRS.len()];
        self.wide_f048 = [0; 8];
        self.wide_f220 = [0; 4];
        self.stpacp_last = 0;
        self.stop_acceptor_enabled = false;
    }

    fn index_of(address: u32) -> Option<usize> {
        SINGLE_BYTE_SFRS.iter().position(|entry| *entry == address)
    }

    /// Read an SFR this peripheral owns.
    pub fn read(&mut self, address: u32) -> Option<u8> {
        if let Some(index) = Self::index_of(address) {
            return Some(self.bytes[index]);
        }
        if (0xF048..0xF050).contains(&address) {
            return Some(self.wide_f048[(address - 0xF048) as usize]);
        }
        if (0xF220..0xF224).contains(&address) {
            return Some(self.wide_f220[(address - 0xF220) as usize]);
        }
        None
    }

    /// Write an SFR this peripheral owns.
    ///
    /// `0xF008`/`0xF009` return `Some(request)` through [`Misc::write`]'s caller
    /// only when they need the chipset to act.
    pub fn write(&mut self, address: u32, value: u8) -> Option<StandbyRequest> {
        if let Some(index) = Self::index_of(address) {
            self.bytes[index] = value;
            return None;
        }
        if (0xF048..0xF050).contains(&address) {
            self.wide_f048[(address - 0xF048) as usize] = value;
            return None;
        }
        if (0xF220..0xF224).contains(&address) {
            self.wide_f220[(address - 0xF220) as usize] = value;
            return None;
        }
        match address {
            STPACP => {
                //
                if value & 0xF0 == 0xA0 && self.stpacp_last & 0xF0 == 0x50 {
                    self.stop_acceptor_enabled = true;
                }
                self.stpacp_last = value;
                Some(StandbyRequest::Armed)
            }
            SBYCON => {
                if value & 0x01 != 0 {
                    return Some(StandbyRequest::Halt);
                }
                if value & 0x02 != 0 && self.stop_acceptor_enabled {
                    self.stop_acceptor_enabled = false;
                    return Some(StandbyRequest::Stop);
                }
                None
            }
            _ => None,
        }
    }

    /// True when this peripheral owns the address at all.
    pub fn owns(&self, address: u32) -> bool {
        Self::index_of(address).is_some()
            || (0xF048..0xF050).contains(&address)
            || (0xF220..0xF224).contains(&address)
            || address == STPACP
            || address == SBYCON
    }

    /// Apply a standby request to the interrupt controller's run mode.
    pub fn apply(request: StandbyRequest, interrupts: &mut Interrupts) {
        match request {
            StandbyRequest::Armed => {}
            StandbyRequest::Halt => interrupts.halt(),
            StandbyRequest::Stop => interrupts.stop(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unknown_block_round_trips() {
        let mut misc = Misc::new();
        for (i, address) in SINGLE_BYTE_SFRS.iter().enumerate() {
            assert!(misc.write(*address, i as u8 + 1).is_none());
            assert_eq!(misc.read(*address), Some(i as u8 + 1));
        }
    }

    #[test]
    fn the_wide_blocks_round_trip() {
        let mut misc = Misc::new();
        misc.write(0xF048, 0x11);
        misc.write(0xF04F, 0x22);
        assert_eq!(misc.read(0xF048), Some(0x11));
        assert_eq!(misc.read(0xF04F), Some(0x22));
        misc.write(0xF220, 0x33);
        assert_eq!(misc.read(0xF223), Some(0x00));
        assert_eq!(misc.read(0xF220), Some(0x33));
    }

    #[test]
    fn stop_requires_the_handshake_before_sbycon_can_stop() {
        let mut misc = Misc::new();
        // Without the handshake, bit 1 does nothing.
        assert_eq!(misc.write(SBYCON, 0x02), None);

        assert_eq!(misc.write(STPACP, 0x50), Some(StandbyRequest::Armed));
        assert_eq!(misc.write(STPACP, 0xA0), Some(StandbyRequest::Armed));
        assert!(misc.stop_acceptor_enabled);
        assert_eq!(misc.write(SBYCON, 0x02), Some(StandbyRequest::Stop));
        assert!(!misc.stop_acceptor_enabled, "the acceptor is consumed");
    }

    #[test]
    fn bit_zero_halts_regardless_of_the_acceptor() {
        let mut misc = Misc::new();
        assert_eq!(misc.write(SBYCON, 0x01), Some(StandbyRequest::Halt));
    }

    #[test]
    fn a_halt_request_sets_the_run_mode() {
        let mut interrupts = Interrupts::new();
        Misc::apply(StandbyRequest::Halt, &mut interrupts);
        assert_eq!(interrupts.run_mode, crate::interrupts::RunMode::Halt);
        Misc::apply(StandbyRequest::Stop, &mut interrupts);
        assert_eq!(interrupts.run_mode, crate::interrupts::RunMode::Stop);
    }

    #[test]
    fn an_unowned_address_is_reported_as_such() {
        let misc = Misc::new();
        assert!(!misc.owns(0xF040));
        assert!(misc.owns(0xF033));
        assert!(misc.owns(STPACP));
    }
}
