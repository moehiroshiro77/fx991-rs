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

//! Keyboard matrix, faithful to.
//!
//! The real-hardware path is the one that matters (`real_hardware = 1`):
//!
//! * A button is identified by a 6-bit index `((code >> 1) & 0x38) | (code & 7)`;
//!   `code` is the value from 's `button_map`.
//! * `ko_bit = 1 << ((code >> 4) & 0xF)` selects the output ("column") line,
//!   `ki_bit = 1 << (code & 0xF)` the input ("row") line.
//! * KI (`0xF040`) idles at `0xFF`; a pressed button whose `ko_bit` is driven low
//!   by KO (`0xF046`) **and not masked off by KOMask** (`0xF044`) clears its
//!   `ki_bit`.  KOMask bits therefore *suppress*.
//! * Ghosting is modelled as in `RecalculateGhost`: a
//!   column that is driven but not masked leaks onto other columns reachable
//!   through pressed keys.
//!
//! `input_filter` (`0xF042`) gates the *interrupt* -- via `has_input` -- without
//! affecting KI itself.  That is why a key press alone does not wake a parked ROM.

use fx991_model::KeyTable;

use crate::interrupts::{Interrupts, KEYBOARD_INTERRUPT};

/// `0xF040`
pub const KI: u32 = 0x0_F040;
/// `0xF042`
pub const INPUT_FILTER: u32 = 0x0_F042;
/// `0xF044`, 16-bit but only 10 bits are used
pub const KO_MASK: u32 = 0x0_F044;
/// `0xF046`, 16-bit but only 10 bits are used
pub const KO: u32 = 0x0_F046;
/// `0xF050`, only registered when `real_hardware == 0`
pub const PD_VALUE: u32 = 0x0_F050;

/// `0xF048`, an 8-byte "unknown" block from.
pub const MISC_F048: u32 = 0x0_F048;
/// `0xF220`, a 4-byte "unknown" block from.
pub const MISC_F220: u32 = 0x0_F220;

/// The 6-bit button index for a matrix code.
pub fn button_index(code: u8) -> usize {
    if code == 0xFF {
        return 63;
    }
    (((code >> 1) & 0x38) | (code & 0x07)) as usize
}

/// The two matrix lines a code addresses.
pub fn matrix_bits(code: u8) -> (u8, u8) {
    if code == 0xFF {
        return (0, 0);
    }
    (1 << ((code >> 4) & 0xF), 1 << (code & 0xF))
}

#[derive(Debug, Clone, Copy, Default)]
struct Button {
    present: bool,
    is_power: bool,
    ko_bit: u8,
    ki_bit: u8,
    pressed: bool,
    /// Latched by a right-click, so it stays down without the button being
    /// held.  A stuck key draws in a different colour.
    stuck: bool,
}

/// The keyboard matrix.
pub struct Keyboard {
    buttons: [Button; 64],
    table: KeyTable,

    /// `0xF046` contents, low 10 bits.
    pub keyboard_out: u16,
    /// `0xF044` contents, low 10 bits.
    pub keyboard_out_mask: u16,
    /// `0xF040` contents.
    pub keyboard_in: u8,
    /// `0xF042` contents.
    pub input_filter: u8,
    ghost: [u8; 8],
    /// Whether any pressed key is visible to the filter, i.e. can raise the IRQ.
    pub has_input: bool,
    /// Set when a key goes down or up, so a renderer can repaint the highlight.
    /// The pressed state is not visible in the screen buffer, so a renderer has
    /// no other way to learn that a button's appearance changed.
    pub dirty: bool,
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Keyboard {
    /// Build the matrix from the measured key table.
    pub fn new() -> Self {
        let table = KeyTable::new();
        let mut buttons = [Button::default(); 64];
        for key in table.keys() {
            let index = button_index(key.code);
            if buttons[index].present {
                continue;
            }
            let (ko_bit, ki_bit) = matrix_bits(key.code);
            buttons[index] = Button {
                present: true,
                is_power: key.code == 0xFF,
                ko_bit,
                ki_bit,
                pressed: false,
                stuck: false,
            };
        }

        let mut keyboard = Self {
            buttons,
            table,
            keyboard_out: 0,
            keyboard_out_mask: 0,
            keyboard_in: 0xFF,
            input_filter: 0,
            ghost: [0; 8],
            has_input: false,
            dirty: true,
        };
        keyboard.recalculate_ghost();
        keyboard
    }

    /// The key table, for name lookups.
    pub fn table(&self) -> &KeyTable {
        &self.table
    }

    /// Resolve a name to its matrix code.
    pub fn code_for_name(&self, name: &str) -> Option<u8> {
        self.table.by_name(name).map(|key| key.code)
    }

    /// `Keyboard::Reset`.
    ///
    /// Clears the matrix *drive* state -- the KO/KI output registers and the
    /// ghost bookkeeping -- but deliberately **leaves `pressed` and `stuck`
    /// alone**.  A reset is not a finger: a key that is physically held stays
    /// held, and the ROM re-reads it when it scans again.  That is exactly what
    /// the self-test depends on: `SHIFT` + `7` held across an `ON` press is how
    /// the machine is meant to be entered.
    ///
    /// Clearing the buttons here would make the reset look like a release, and
    /// the self-test would never trigger from the UI.
    pub fn reset(&mut self) {
        self.keyboard_out = 0;
        self.keyboard_out_mask = 0;
        self.input_filter = 0;
        self.has_input = false;
        self.dirty = true;
        self.recalculate_ghost();
    }

    /// Take the dirty flag, clearing it.  `true` means a button's pressed state
    /// changed since the last call, so its highlight needs repainting.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    /// True when the code is the power key, which resets instead of pressing.
    pub fn is_power_key(&self, code: u8) -> bool {
        self.buttons[button_index(code)].is_power
    }

    /// Press a key by matrix code.  Returns false for a code the model lacks.
    ///
    /// The power key is special: pressing it does not enter the matrix at all,
    /// it resets the machine.  Because the reset needs
    /// the whole `Chipset`, the caller does it -- see
    /// [`Keyboard::is_power_key`] and `Chipset:press_key`.
    pub fn press(&mut self, code: u8) -> bool {
        self.press_with(code, false)
    }

    /// [`Keyboard::press`] with the "stick" flag.
    ///
    /// `stick` is what a right-click does: it *toggles* a latch rather than
    /// holding the key down, so the button stays pressed after the mouse is
    /// released.  That is how you press `SHIFT` and
    /// then click something else.
    pub fn press_with(&mut self, code: u8, stick: bool) -> bool {
        let index = button_index(code);
        if !self.buttons[index].present {
            return false;
        }
        let button = &mut self.buttons[index];
        let was_pressed = button.pressed;
        if stick {
            button.stuck = !button.stuck;
            button.pressed = button.stuck;
        } else {
            button.pressed = true;
        }
        self.dirty |= button.pressed != was_pressed;
        self.recalculate_ghost();
        true
    }

    /// Whether the button is latched down by a right-click.
    pub fn is_stuck(&self, code: u8) -> bool {
        let index = button_index(code);
        self.buttons[index].present && self.buttons[index].stuck
    }

    /// Every button that is currently down, as `(code, stuck)`.
    ///
    /// A renderer needs this to draw the press highlights; the matrix state is
    /// not visible anywhere else.
    pub fn pressed_buttons(&self) -> Vec<(u8, bool)> {
        self.table
            .keys()
            .filter_map(|key| {
                let index = button_index(key.code);
                let button = &self.buttons[index];
                if button.present && button.pressed {
                    Some((key.code, button.stuck))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Press a key by name.
    pub fn press_name(&mut self, name: &str) -> Option<u8> {
        let code = self.code_for_name(name)?;
        self.press(code).then_some(code)
    }

    /// Release one key, or every key when `code` is `None`.
    ///
    /// A stuck (right-click latched) key is *not* released by this: only its own
    /// right-click clears it (where a left-button
    /// release calls `ReleaseAll` but a stuck key stays down).
    pub fn release(&mut self, code: Option<u8>) {
        match code {
            None => {
                for button in &mut self.buttons {
                    if button.stuck {
                        continue;
                    }
                    self.dirty |= button.pressed;
                    button.pressed = false;
                }
            }
            Some(code) => {
                let index = button_index(code);
                if self.buttons[index].present && !self.buttons[index].stuck {
                    self.dirty |= self.buttons[index].pressed;
                    self.buttons[index].pressed = false;
                }
            }
        }
        self.recalculate_ghost();
    }

    /// True when the button is currently held.
    pub fn is_pressed(&self, code: u8) -> bool {
        let index = button_index(code);
        self.buttons[index].present && self.buttons[index].pressed
    }

    /// `RecalculateGhost`.
    ///
    /// The indices here are matrix coordinates (column 0.8, row 0.8, and the button
    /// index `cx * 8 + rx` derived from them), so the loops index rather than iterate.
    #[allow(clippy::needless_range_loop)]
    pub fn recalculate_ghost(&mut self) {
        let mut connections = [0u8; 8];

        self.has_input = false;
        for button in &self.buttons {
            if button.present
                && !button.is_power
                && button.pressed
                && button.ki_bit & self.input_filter != 0
            {
                self.has_input = true;
            }
        }

        for cx in 0..8 {
            for rx in 0..8 {
                let button = self.buttons[cx * 8 + rx];
                if button.present && !button.is_power && button.pressed {
                    for ax in 0..8 {
                        let sibling = self.buttons[ax * 8 + rx];
                        if sibling.present && !sibling.is_power && sibling.pressed {
                            connections[cx] |= 1 << ax;
                        }
                    }
                }
            }
        }

        let mut seen = [false; 8];
        for cx in 0..8 {
            if seen[cx] {
                continue;
            }
            let mut to_visit = 1u8 << cx;
            let mut ghost_mask = 1u8 << cx;
            seen[cx] = true;
            while to_visit != 0 {
                let mut new_to_visit = 0u8;
                for vx in 0..8 {
                    if to_visit & (1 << vx) != 0 {
                        for sx in 0..8 {
                            if connections[vx] & (1 << sx) != 0 && !seen[sx] {
                                new_to_visit |= 1 << sx;
                                ghost_mask |= 1 << sx;
                                seen[sx] = true;
                            }
                        }
                    }
                }
                to_visit = new_to_visit;
            }
            for gx in 0..8 {
                if ghost_mask & (1 << gx) != 0 {
                    self.ghost[gx] = ghost_mask;
                }
            }
        }

        self.recalculate_ki();
    }

    /// `RecalculateKI`.
    ///
    /// Bit positions are the KO/KI line numbers, so the loop indexes by bit.
    #[allow(clippy::needless_range_loop)]
    pub fn recalculate_ki(&mut self) {
        let mut ghosted = 0u8;
        for ix in 0..7 {
            if self.keyboard_out & !self.keyboard_out_mask & (1 << ix) != 0 {
                ghosted |= self.ghost[ix];
            }
        }

        let mut keyboard_in = 0xFFu8;
        for button in &self.buttons {
            if button.present && !button.is_power && button.pressed && button.ko_bit & ghosted != 0
            {
                keyboard_in &= !button.ki_bit;
            }
        }
        self.keyboard_in = keyboard_in;
    }

    /// Raise the keyboard interrupt when the filter lets a key be seen.
    pub fn tick(&mut self, interrupts: &mut Interrupts) {
        if self.has_input {
            interrupts.try_raise(KEYBOARD_INTERRUPT);
        }
    }

    /// Read an SFR this peripheral owns.
    pub fn read(&mut self, address: u32) -> Option<u8> {
        match address {
            KI => Some(self.keyboard_in),
            INPUT_FILTER => Some(self.input_filter),
            _ if address == KO_MASK || address == KO_MASK + 1 => {
                let shift = ((address - KO_MASK) & 1) * 8;
                Some(((self.keyboard_out_mask & 0x3FF) >> shift) as u8)
            }
            _ if address == KO || address == KO + 1 => {
                let shift = ((address - KO) & 1) * 8;
                Some(((self.keyboard_out & 0x3FF) >> shift) as u8)
            }
            _ => None,
        }
    }

    /// Write an SFR this peripheral owns.  Writing the *low* byte of KO or KOMask
    /// triggers the KI recompute (56`).
    pub fn write(&mut self, address: u32, value: u8) -> bool {
        match address {
            KI => true, // read-only, but owned: writes are ignored, not faults
            INPUT_FILTER => {
                self.input_filter = value;
                true
            }
            _ if address == KO_MASK || address == KO_MASK + 1 => {
                let shift = ((address - KO_MASK) & 1) * 8;
                self.keyboard_out_mask &= !(0xFFu16 << shift);
                self.keyboard_out_mask |= (value as u16) << shift;
                self.keyboard_out_mask &= 0x3FF;
                if shift == 0 {
                    self.recalculate_ki();
                }
                true
            }
            _ if address == KO || address == KO + 1 => {
                let shift = ((address - KO) & 1) * 8;
                self.keyboard_out &= !(0xFFu16 << shift);
                self.keyboard_out |= (value as u16) << shift;
                self.keyboard_out &= 0x3FF;
                if shift == 0 {
                    self.recalculate_ki();
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILTER_ON: u8 = 0xFF;

    fn keyboard() -> Keyboard {
        let mut kb = Keyboard::new();
        kb.input_filter = FILTER_ON;
        kb
    }

    #[test]
    fn ki_idles_high() {
        let kb = Keyboard::new();
        assert_eq!(kb.keyboard_in, 0xFF);
        assert!(!kb.has_input);
    }

    #[test]
    fn a_press_with_no_column_driven_leaves_ki_at_idle() {
        // KI only responds once the ROM selects the key's column through KO.
        let mut kb = keyboard();
        assert!(kb.press(0x00));
        assert!(kb.has_input, "the interrupt would fire");
        assert_eq!(kb.keyboard_in, 0xFF, "but no row is pulled");
    }

    #[test]
    fn a_press_with_the_column_driven_pulls_ki_low() {
        // `KO & ~KOMask` selects a column, so KOMask bits suppress.
        let mut kb = keyboard();
        kb.keyboard_out = 0x01;
        kb.keyboard_out_mask = 0x00;
        kb.recalculate_ghost();
        assert!(kb.press(0x00));
        assert_eq!(kb.keyboard_in, !1u8);
    }

    #[test]
    fn komask_can_suppress_a_driven_column() {
        let mut kb = keyboard();
        kb.keyboard_out = 0x01;
        kb.keyboard_out_mask = 0x01;
        kb.recalculate_ghost();
        assert!(kb.press(0x00));
        assert_eq!(kb.keyboard_in, 0xFF);
    }

    #[test]
    fn the_input_filter_gates_the_interrupt_not_the_row() {
        let mut kb = Keyboard::new();
        kb.keyboard_out = 0x01;
        kb.keyboard_out_mask = 0x00;
        kb.recalculate_ghost();
        kb.input_filter = 0;
        assert!(kb.press(0x00));
        assert!(!kb.has_input);
        assert_eq!(kb.keyboard_in, !1u8, "row still pulled");
        kb.input_filter = FILTER_ON;
        kb.recalculate_ghost();
        assert!(kb.has_input);
    }

    #[test]
    fn release_restores_ki() {
        let mut kb = keyboard();
        kb.keyboard_out = 0x01;
        kb.recalculate_ghost();
        kb.press(0x00);
        kb.release(Some(0x00));
        assert_eq!(kb.keyboard_in, 0xFF);
        assert!(!kb.has_input);
    }

    #[test]
    fn reset_clears_the_drive_state_but_not_the_buttons() {
        // `Keyboard::Reset` clears the KO/KI output
        // registers and the ghost bookkeeping, but deliberately leaves the
        // buttons' `pressed` flags alone: a reset is not a finger.  The
        // self-test depends on this -- it is entered by holding SHIFT + 7
        // across an ON press, and ON *is* a reset.
        let mut kb = keyboard();
        kb.keyboard_out = 0x01;
        kb.recalculate_ghost();
        kb.press(0x07); // SHIFT
        kb.press(0x02); // 7
        assert_eq!(
            kb.keyboard_in,
            !(0x80 | 0x04) as u8,
            "expected 0x7B while held"
        );

        kb.reset();

        assert!(kb.is_pressed(0x07), "the reset released SHIFT");
        assert!(kb.is_pressed(0x02), "the reset released 7");
        assert_eq!(kb.keyboard_out, 0, "KO should be cleared");
        assert_eq!(kb.keyboard_out_mask, 0);
        assert_eq!(kb.input_filter, 0);

        // The ROM re-scans and sees the keys again.
        kb.keyboard_out = 0x01;
        kb.recalculate_ghost();
        assert_eq!(kb.keyboard_in, !(0x80 | 0x04) as u8);
    }

    #[test]
    fn two_keys_in_the_same_column_both_show() {
        let mut kb = keyboard();
        kb.keyboard_out = 0x01;
        kb.recalculate_ghost();
        kb.press(0x00); // '1': ko 0x01, ki 0x01
        kb.press(0x01); // '4': ko 0x01, ki 0x02
        assert_eq!(kb.keyboard_in, !0x03u8);
    }

    #[test]
    fn writing_ko_recomputes_ki() {
        let mut kb = keyboard();
        kb.press(0x00);
        assert_eq!(kb.keyboard_in, 0xFF, "nothing drives column 0 yet");
        assert!(kb.write(KO, 0x01));
        assert_eq!(kb.keyboard_in, !1u8);
    }

    #[test]
    fn ki_is_read_only_but_owned() {
        let mut kb = keyboard();
        let before = kb.keyboard_in;
        assert!(kb.write(KI, 0x00), "owned, so not a fault");
        assert_eq!(kb.keyboard_in, before);
    }

    #[test]
    fn a_code_the_model_lacks_cannot_be_pressed() {
        let mut kb = Keyboard::new();
        // 0x50 maps to a button index no measured code claims.
        assert!(kb.table().by_code(0x50).is_none());
        assert!(!kb.press(0x50));
    }

    #[test]
    fn distinct_codes_can_share_a_button_index_and_the_first_one_wins() {
        // 0x77 and 0xFF both fold to button index 63.  The power key registers
        // first, so 0x77 is simply not a button the model knows.
        let kb = Keyboard::new();
        assert_eq!(button_index(0x77), button_index(0xFF));
        assert!(kb.table().by_code(0x77).is_none());
        assert!(kb.is_power_key(0xFF));
    }

    #[test]
    fn the_power_key_is_flagged_and_has_no_matrix_bits() {
        let kb = Keyboard::new();
        assert!(kb.is_power_key(0xFF));
        assert_eq!(matrix_bits(0xFF), (0, 0));
        assert_eq!(button_index(0xFF), 63);
    }
}
