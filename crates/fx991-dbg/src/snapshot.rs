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

//! Named save points, and a diff between two of them.
//!
//! A snapshot is a whole [`fx991_chipset::ChipsetSnapshot`] plus the tick counter,
//! so restoring one and running forward reproduces exactly what happened the first
//! time -- the integration test `a_full_snapshot_round_trips_and_replays_the_rom`
//! pins that on the real ROM.  That is what makes "go back and look again" possible
//! instead of "start over".
//!
//! # Patches are inside the snapshot
//!
//! A planted code overlay is machine state, so it comes back with a restore.  That
//! is the right behaviour -- a snapshot is "where the machine was" -- but it means
//! [`crate::patch::Patches`] must re-derive its view of the world afterwards rather
//! than keeping its own copy.  It does; see that module.

use fx991::EmuSnapshot;

/// One saved state.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// What the user called it.
    pub name: String,
    /// The machine.
    pub state: EmuSnapshot,
}

/// A debugger's collection of save points.
///
/// Re-taking a name replaces the old state rather than adding a second entry: a
/// script that loops "snapshot here, try something, restore" would otherwise grow
/// without bound and `restore here` would be ambiguous.
#[derive(Debug, Default, Clone)]
pub struct Snapshots {
    entries: Vec<Snapshot>,
}

impl Snapshots {
    /// An empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Save `state` under `name`, replacing any previous state with that name.
    pub fn save(&mut self, name: impl Into<String>, state: EmuSnapshot) {
        let name = name.into();
        if let Some(existing) = self.entries.iter_mut().find(|entry| entry.name == name) {
            existing.state = state;
            return;
        }
        self.entries.push(Snapshot { name, state });
    }

    /// The state saved under `name`.
    pub fn get(&self, name: &str) -> Option<&EmuSnapshot> {
        self.entries
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| &entry.state)
    }

    /// Whether a name is in use.
    pub fn contains(&self, name: &str) -> bool {
        self.entries.iter().any(|entry| entry.name == name)
    }

    /// Remove a name.  Returns whether there was one.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.name != name);
        self.entries.len() != before
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// How many save points are held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing is saved.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every save point, in the order they were first taken.
    pub fn entries(&self) -> &[Snapshot] {
        &self.entries
    }
}

/// A difference between two states.
///
/// Only the parts a person would ask about: the registers, the RAM, and a one-line
/// summary of everything else.  A full field-by-field dump of two
/// `ChipsetSnapshot`s is unreadable and almost never what was wanted.
#[derive(Debug, Clone, Default)]
pub struct Diff {
    /// Registers that differ, as `(name, before, after)`.
    pub registers: Vec<(String, u64, u64)>,
    /// RAM bytes that differ, as `(address, before, after)`.
    pub ram: Vec<(u32, u8, u8)>,
    /// The PC before and after.
    pub pc: (u32, u32),
    /// The SP before and after.
    pub sp: (u16, u16),
    /// The PSW before and after.
    pub psw: (u8, u8),
    /// The tick counter before and after.
    pub ticks: (u64, u64),
    /// Peripherals whose state differs, by name.
    pub peripherals: Vec<&'static str>,
    /// The number of bytes that differ in the screen buffer.
    pub screen_bytes: usize,
}

impl Diff {
    /// True when nothing at all differs.
    pub fn is_empty(&self) -> bool {
        self.registers.is_empty()
            && self.ram.is_empty()
            && self.pc.0 == self.pc.1
            && self.sp.0 == self.sp.1
            && self.psw.0 == self.psw.1
            && self.peripherals.is_empty()
            && self.screen_bytes == 0
    }

    /// Compare two states.
    pub fn between(before: &EmuSnapshot, after: &EmuSnapshot) -> Self {
        let mut diff = Diff {
            pc: (
                before.chipset.cpu.regs.physical_pc(),
                after.chipset.cpu.regs.physical_pc(),
            ),
            sp: (before.chipset.cpu.regs.sp, after.chipset.cpu.regs.sp),
            psw: (before.chipset.cpu.regs.psw(), after.chipset.cpu.regs.psw()),
            ticks: (before.ticks, after.ticks),
            ..Diff::default()
        };

        // Byte registers, then the wider aliases, since a diff is read by a person
        // and `ER0` changing while `R0`/`R1` change is one fact, not three.
        for index in 0..16 {
            let left = before.chipset.cpu.regs.r[index];
            let right = after.chipset.cpu.regs.r[index];
            if left != right {
                diff.registers
                    .push((format!("R{index}"), left as u64, right as u64));
            }
        }
        for index in (0..16).step_by(2) {
            let left = before.chipset.cpu.regs.er(index);
            let right = after.chipset.cpu.regs.er(index);
            if left != right {
                diff.registers
                    .push((format!("ER{index}"), left as u64, right as u64));
            }
        }
        for (name, left, right) in [
            (
                "SP",
                before.chipset.cpu.regs.sp as u64,
                after.chipset.cpu.regs.sp as u64,
            ),
            (
                "EA",
                before.chipset.cpu.regs.ea as u64,
                after.chipset.cpu.regs.ea as u64,
            ),
            (
                "LR",
                before.chipset.cpu.regs.lr() as u64,
                after.chipset.cpu.regs.lr() as u64,
            ),
        ] {
            if left != right {
                diff.registers.push((name.to_string(), left, right));
            }
        }

        let before_ram = &before.chipset.bus.ram;
        let after_ram = &after.chipset.bus.ram;
        for (index, (left, right)) in before_ram.iter().zip(after_ram.iter()).enumerate() {
            if left != right {
                diff.ram.push((0x0_D000 + index as u32, *left, *right));
            }
        }

        // The peripherals, as a summary.  The screen buffer is counted rather than
        // listed: it is 2 KiB and a person wants "the display changed", not 2000
        // byte pairs.
        let before = &before.chipset;
        let after = &after.chipset;
        if before.interrupts.mask != after.interrupts.mask
            || before.interrupts.pending != after.interrupts.pending
            || before.interrupts.run_mode != after.interrupts.run_mode
        {
            diff.peripherals.push("interrupts");
        }
        if before.screen.mode != after.screen.mode
            || before.screen.contrast != after.screen.contrast
            || before.screen.range != after.screen.range
        {
            diff.peripherals.push("screen control");
        }
        diff.screen_bytes = before
            .screen
            .buffer
            .iter()
            .zip(after.screen.buffer.iter())
            .filter(|(left, right)| left != right)
            .count();
        if before.timer != after.timer {
            diff.peripherals.push("timer");
        }
        if before.keyboard != after.keyboard {
            diff.peripherals.push("keyboard");
        }
        if before.misc != after.misc {
            diff.peripherals.push("misc");
        }
        if before.dsr != after.dsr {
            diff.peripherals.push("dsr");
        }
        if before.standby != after.standby {
            diff.peripherals.push("standby");
        }
        if before.bus.fault_count != after.bus.fault_count {
            diff.peripherals.push("faults");
        }
        if before.bus.overlays != after.bus.overlays {
            diff.peripherals.push("patches");
        }

        diff
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fx991::Emu;

    /// A machine with no ROM: enough to snapshot, because a snapshot is state and
    /// not behaviour.
    fn emu() -> Emu {
        Emu::from_rom(vec![0u8; 0x4_0000])
    }

    #[test]
    fn a_name_holds_one_state_and_re_saving_replaces_it() {
        let mut snapshots = Snapshots::new();
        let mut machine = emu();
        snapshots.save("here", machine.snapshot());
        machine.run(10);
        snapshots.save("here", machine.snapshot());

        assert_eq!(snapshots.len(), 1, "re-saving a name replaces");
        assert_eq!(snapshots.get("here").unwrap().ticks, 10);
    }

    #[test]
    fn names_can_be_removed_and_listed() {
        let mut snapshots = Snapshots::new();
        let machine = emu();
        snapshots.save("a", machine.snapshot());
        snapshots.save("b", machine.snapshot());
        assert_eq!(snapshots.entries().len(), 2);
        assert!(snapshots.contains("a"));

        assert!(snapshots.remove("a"));
        assert!(
            !snapshots.remove("a"),
            "removing twice reports nothing removed"
        );
        assert!(!snapshots.contains("a"));
        assert!(snapshots.contains("b"));

        snapshots.clear();
        assert!(snapshots.is_empty());
    }

    #[test]
    fn a_missing_name_is_none_not_a_panic() {
        let snapshots = Snapshots::new();
        assert!(snapshots.get("nothing").is_none());
    }

    #[test]
    fn an_identical_state_diffs_as_empty() {
        let machine = emu();
        let state = machine.snapshot();
        assert!(Diff::between(&state, &state).is_empty());
    }

    #[test]
    fn a_diff_names_the_registers_and_ram_that_moved() {
        let mut machine = emu();
        let before = machine.snapshot();
        machine.set_reg(0, 0x42);
        machine.poke(0x0_D180, 0x99);
        let after = machine.snapshot();

        let diff = Diff::between(&before, &after);
        assert!(!diff.is_empty());
        assert!(
            diff.registers
                .iter()
                .any(|(name, _, to)| name == "R0" && *to == 0x42),
            "R0 is reported: {:?}",
            diff.registers
        );
        assert!(
            diff.ram.contains(&(0x0_D180, 0x00, 0x99)),
            "the RAM byte is reported: {:?}",
            diff.ram
        );
        assert_eq!(diff.ticks, (0, 0));
    }

    #[test]
    fn a_diff_reports_the_screen_as_a_count_and_a_summary() {
        let mut machine = emu();
        let before = machine.snapshot();
        machine.poke(fx991_chipset::screen::CONTROL_MODE, 5);
        machine.poke(fx991_chipset::screen::BUFFER_BASE, 0xFF);
        let after = machine.snapshot();

        let diff = Diff::between(&before, &after);
        assert!(
            diff.peripherals.contains(&"screen control"),
            "{:?}",
            diff.peripherals
        );
        assert_eq!(diff.screen_bytes, 1, "one buffer byte changed");
    }

    #[test]
    fn a_diff_reports_a_patch_and_a_fault_count() {
        let mut machine = emu();
        let before = machine.snapshot();
        machine.load_code(0x1_0000, &[0x8E, 0xF2]);
        machine.peek(0x6_0000); // one fault
        let after = machine.snapshot();

        let diff = Diff::between(&before, &after);
        assert!(diff.peripherals.contains(&"patches"));
        assert!(diff.peripherals.contains(&"faults"));
    }
}
