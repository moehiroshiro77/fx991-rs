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

//! The 24-bit data space: ROM windows, battery-backed RAM, and SFRs.
//!
//! The layout follows the hardware: three ROM windows, battery-backed RAM from
//! `0xD000`, and the SFR block above `0xF000`.
//! Three behaviours are worth stating up front because they are easy to get wrong:
//!
//! 1. **Regions are keyed by `address >> 16` of their base.**  The RAM region is
//!    set up at base `0xD000` and the SFRs at `0xF000`+, so **both land in segment
//!    0**, not in segments 0xD/0xF.  Segment 0 is therefore the complete 16-bit
//!    map: ROM `0x0000-0xCFFF`, RAM `0xD000-0xEFFF`, SFR `0xF000-0xFFFF`.  Segments
//!    1/2/3 hold ROM banks at `0x10000`/`0x20000`/`0x30000`, and segment 5 is a
//!    window onto ROM `0x00000` used for reading ROM *data*.
//! 2. Code fetches in segment 0 are served straight from the ROM image, ignoring
//!    the fact that the segment-0 ROM region only spans `0x00000-0x0CFFF`.
//!    Code fetches into `0x0D000-0x0FFFF` therefore return real ROM bytes
//!    instead of faulting.
//! 3. Unmapped reads return 0 and unmapped writes are dropped, but both are
//!    *recorded* -- "read as 0" and "genuinely 0" are very different when you are
//!    debugging a ROP chain.
//!
//! # Where the peripherals live
//!
//! SFR access goes through [`SfrDevice`], so this crate owns the *address map* and
//! whoever owns the peripherals implements the *behaviour*.  That split is what
//! keeps the chipset out of the address decoder.

#![warn(missing_docs)]

use nxu8_core::Memory;

/// The ROM image, plus the handful of facts about it the map needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rom {
    /// Raw ROM contents.
    pub data: Vec<u8>,
}

impl Rom {
    /// Wrap a ROM image.
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    /// Size in bytes.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// True when the image is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Read a byte, or `None` past the end of the image.
    #[inline]
    pub fn byte(&self, address: u32) -> Option<u8> {
        self.data.get(address as usize).copied()
    }
}

/// One of the ROM windows from  for HW_CLASSWIZ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RomWindow {
    /// Start of the window in the data space.
    pub base: u32,
    /// Length of the window.
    pub size: u32,
    /// Offset into the ROM image that the window maps.
    pub rom_base: u32,
}

/// The segment layout for fx-991CN X.
pub const CLASSWIZ_ROM_WINDOWS: [RomWindow; 5] = [
    RomWindow {
        base: 0x0_0000,
        size: 0x0_D000,
        rom_base: 0x0_0000,
    },
    RomWindow {
        base: 0x1_0000,
        size: 0x1_0000,
        rom_base: 0x1_0000,
    },
    RomWindow {
        base: 0x2_0000,
        size: 0x1_0000,
        rom_base: 0x2_0000,
    },
    RomWindow {
        base: 0x3_0000,
        size: 0x1_0000,
        rom_base: 0x3_0000,
    },
    // A second window onto the bottom of the ROM, for reading ROM *data*.
    RomWindow {
        base: 0x5_0000,
        size: 0x1_0000,
        rom_base: 0x0_0000,
    },
];

/// Battery-backed RAM: `0xD000.0xEFFF`, 0x2000 bytes.
pub const RAM_BASE: u32 = 0x0_D000;
/// Length of the battery-backed RAM region.
pub const RAM_SIZE: u32 = 0x2000;

/// Start of the special-function register area.
pub const SFR_BASE: u32 = 0x0_F000;
/// End of the special-function register area, inclusive.
pub const SFR_END: u32 = 0x0_FFFF;

/// A device that owns part of the SFR space.
///
/// Implementors are queried in registration order and must return `None` for any
/// address they do not own, so a partially overlapping device is a bug rather than
/// a silent shadow.
pub trait SfrDevice {
    /// Read one SFR byte, or `None` if this device does not own `address`.
    fn read(&mut self, address: u32) -> Option<u8>;

    /// Write one SFR byte.  Return `false` if this device does not own `address`.
    fn write(&mut self, address: u32, value: u8) -> bool;

    /// Name for diagnostics; shown in memory-error reports.
    fn name(&self) -> &'static str;
}

/// What kind of access faulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// A code fetch.
    Code,
    /// A data read.
    Read,
    /// A data write.
    Write,
    /// An SFR read inside the SFR range but owned by nobody.
    SfrRead,
    /// An SFR write inside the SFR range but owned by nobody.
    SfrWrite,
}

impl Access {
    /// Short label for logs.
    pub fn label(self) -> &'static str {
        match self {
            Access::Code => "code",
            Access::Read => "read",
            Access::Write => "write",
            Access::SfrRead => "sfr-read",
            Access::SfrWrite => "sfr-write",
        }
    }
}

/// A recorded access to an unmapped address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessFault {
    /// What kind of access it was.
    pub kind: Access,
    /// The address that faulted.
    pub address: u32,
}

/// The number of distinct faults kept before the log stops growing.
///
/// The ROM touches one unmapped SFR tens of
/// thousands of times, so an unbounded log would be a memory leak in a long run.
pub const MAX_LOGGED_FAULTS: usize = 4096;

/// A memory access to watch for.
///
/// Watches are checked on every data access, so this is deliberately a flat
/// record rather than a trait object: the debugger needs addresses and flags, and
/// the emulator needs the check to be cheap when nothing is armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watch {
    /// The address being watched.
    pub address: u32,
    /// Stop when the address is read.
    pub read: bool,
    /// Stop when the address is written.
    pub write: bool,
    /// Stop when the address is fetched as code.
    pub execute: bool,
}

impl Watch {
    /// Watch both data directions, which is what "watch this variable" means.
    pub fn data(address: u32) -> Self {
        Self {
            address,
            read: true,
            write: true,
            execute: false,
        }
    }

    /// Whether this watch cares about an access of the given kind.
    pub fn wants(&self, kind: Access) -> bool {
        match kind {
            Access::Code => self.execute,
            Access::Read | Access::SfrRead => self.read,
            Access::Write | Access::SfrWrite => self.write,
        }
    }
}

/// One access that matched a [`Watch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchHit {
    /// What kind of access it was.
    pub kind: Access,
    /// The address accessed.
    pub address: u32,
    /// The byte read or written.  Meaningless for a code fetch.
    pub value: u8,
}

/// How many watch hits are buffered before the oldest are dropped.
///
/// A watch on a hot address (an `[EA]` loop counter, say) would otherwise grow
/// without bound during one `run`.
pub const MAX_BUFFERED_WATCH_HITS: usize = 256;

/// Which part of the address space an address falls in.
///
/// Worth having as a value rather than a chain of range checks at every call
/// site, because the layout is counter-intuitive: RAM and the SFRs live in
/// **segment 0**, not segments 0xD/0xF, so `0xD_D000` is not a RAM alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// A ROM window, with the offset into the image it maps.
    Rom {
        /// Base of the window in the data space.
        base: u32,
        /// Offset into the ROM image the window starts at.
        rom_base: u32,
    },
    /// Battery-backed RAM.
    Ram,
    /// An SFR.
    Sfr,
    /// Nothing claims this address.
    Unmapped,
}

/// Everything about a [`Bus`] that a snapshot needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusSnapshot {
    /// The battery-backed RAM.
    pub ram: Vec<u8>,
    /// Planted code blocks, by base address.
    pub overlays: Vec<(u32, Vec<u8>)>,
    /// Fault counter at the time of the snapshot.
    pub fault_count: u64,
    /// Most recent fault at the time of the snapshot.
    pub last_fault: Option<AccessFault>,
    /// The fault log at the time of the snapshot.
    pub fault_log: Vec<AccessFault>,
}

/// The data space.
pub struct Bus {
    rom: Rom,
    ram: Vec<u8>,
    devices: Vec<Box<dyn SfrDevice>>,

    /// Overlays consulted before the ROM image, so a test can plant a program
    /// somewhere without mutating the image.
    overlays: Vec<(u32, Vec<u8>)>,

    /// Addresses the caller asked to be told about.
    watches: Vec<Watch>,
    /// Accesses that matched a watch, since the last drain.
    watch_hits: Vec<WatchHit>,
    /// Every match, including ones past the buffer limit.
    watch_hit_count: u64,

    /// While set, accesses are neither recorded as faults nor reported to
    /// watches.  A debugger reading memory must not look like the guest reading
    /// it, and must not perturb the fault counters tests assert on.
    quiet: bool,

    /// Total faults seen, including ones past the log limit.
    pub fault_count: u64,
    /// The most recent fault.
    pub last_fault: Option<AccessFault>,
    /// The first [`MAX_LOGGED_FAULTS`] distinct faults.
    pub fault_log: Vec<AccessFault>,
}

impl Bus {
    /// Build a bus with a zeroed RAM and no devices registered.
    pub fn new(rom: Rom) -> Self {
        Self {
            rom,
            ram: vec![0; RAM_SIZE as usize],
            devices: Vec::new(),
            overlays: Vec::new(),
            watches: Vec::new(),
            watch_hits: Vec::new(),
            watch_hit_count: 0,
            quiet: false,
            fault_count: 0,
            last_fault: None,
            fault_log: Vec::new(),
        }
    }

    /// Register an SFR device.  Later devices are queried after earlier ones.
    pub fn register(&mut self, device: Box<dyn SfrDevice>) {
        self.devices.push(device);
    }

    /// The ROM image.
    pub fn rom(&self) -> &Rom {
        &self.rom
    }

    /// The battery-backed RAM.
    pub fn ram(&self) -> &[u8] {
        &self.ram
    }

    /// Mutable access to the RAM, for snapshot/restore.
    pub fn ram_mut(&mut self) -> &mut [u8] {
        &mut self.ram
    }

    // ------------------------------------------------------------------ regions

    /// Which part of the address space `address` falls in.
    pub fn region(&self, address: u32) -> Region {
        if let Some(window) = self.window_for(address) {
            return Region::Rom {
                base: window.base,
                rom_base: window.rom_base,
            };
        }
        if (RAM_BASE..RAM_BASE + RAM_SIZE).contains(&address) {
            return Region::Ram;
        }
        if (SFR_BASE..=SFR_END).contains(&address) {
            return Region::Sfr;
        }
        Region::Unmapped
    }

    // ------------------------------------------------------------------ watches

    /// Watch an address, replacing any watch already there.
    pub fn watch(&mut self, watch: Watch) {
        self.watches
            .retain(|existing| existing.address != watch.address);
        self.watches.push(watch);
    }

    /// Stop watching an address.  Returns whether anything was removed.
    pub fn unwatch(&mut self, address: u32) -> bool {
        let before = self.watches.len();
        self.watches.retain(|watch| watch.address != address);
        self.watches.len() != before
    }

    /// Every armed watch, in the order they were added.
    pub fn watches(&self) -> &[Watch] {
        &self.watches
    }

    /// Drop every watch.
    pub fn clear_watches(&mut self) {
        self.watches.clear();
        self.watch_hits.clear();
    }

    /// Take the hits recorded since the last call.
    pub fn take_watch_hits(&mut self) -> Vec<WatchHit> {
        std::mem::take(&mut self.watch_hits)
    }

    /// The hits recorded since the last drain, without removing them.
    pub fn watch_hits(&self) -> &[WatchHit] {
        &self.watch_hits
    }

    /// How many accesses have matched a watch in total.
    ///
    /// Unlike the buffer this is not reset by draining, so a caller can tell
    /// "many hits, most dropped" from "no hits".
    pub fn watch_hit_count(&self) -> u64 {
        self.watch_hit_count
    }

    /// Record an access against the armed watches.
    fn note_access(&mut self, kind: Access, address: u32, value: u8) {
        if self.watches.is_empty() {
            return;
        }
        if !self.watches.iter().any(|watch| watch.wants(kind)) {
            return;
        }
        self.watch_hit_count += 1;
        if self.watch_hits.len() < MAX_BUFFERED_WATCH_HITS {
            self.watch_hits.push(WatchHit {
                kind,
                address,
                value,
            });
        }
    }

    // -------------------------------------------------------------- quiet reads

    /// Read a data byte without recording a fault or reporting a watch.
    ///
    /// This is what a debugger's memory view, disassembler, and condition
    /// evaluator use: the guest did not make this access, so it must not show up
    /// in the fault log or trip a watchpoint on its own.
    pub fn peek_quiet(&mut self, address: u32) -> u8 {
        self.quiet = true;
        let value = self.read_data_plain(address & 0x00FF_FFFF);
        self.quiet = false;
        value
    }

    /// [`Bus::peek_quiet`] over a range.
    pub fn peek_quiet_block(&mut self, address: u32, length: usize) -> Vec<u8> {
        (0..length as u32)
            .map(|offset| self.peek_quiet(address + offset))
            .collect()
    }

    /// Fetch a code halfword without recording a fault or reporting a watch.
    pub fn read_code_quiet(&mut self, address: u32) -> u16 {
        self.quiet = true;
        let value = self.read_code_plain(address);
        self.quiet = false;
        value
    }

    fn record(&mut self, kind: Access, address: u32) {
        if self.quiet {
            return;
        }
        self.fault_count += 1;
        let fault = AccessFault { kind, address };
        self.last_fault = Some(fault);
        if self.fault_log.len() < MAX_LOGGED_FAULTS && !self.fault_log.contains(&fault) {
            self.fault_log.push(fault);
        }
    }

    /// Plant a byte block that code fetches and data reads see instead of ROM.
    ///
    /// Used by tests and by the M5 experiments to build a program where the ROM
    /// never looks.  Note that an overlay shadows **both** the code fetch and the
    /// data read at the address, and that a block of odd length will pair its
    /// last byte with `0` on a fetch.
    pub fn load_code(&mut self, address: u32, blob: &[u8]) {
        self.overlays.retain(|(base, _)| *base != address);
        self.overlays.push((address, blob.to_vec()));
    }

    /// Every planted code block, by base address.
    pub fn overlays(&self) -> &[(u32, Vec<u8>)] {
        &self.overlays
    }

    /// Remove a planted block.  Returns whether there was one.
    ///
    /// Patch management needs this: without it a patch could only be replaced,
    /// never removed.
    pub fn clear_code(&mut self, address: u32) -> bool {
        let before = self.overlays.len();
        self.overlays.retain(|(base, _)| *base != address);
        self.overlays.len() != before
    }

    /// Everything a snapshot needs to restore this bus.
    pub fn snapshot(&self) -> BusSnapshot {
        BusSnapshot {
            ram: self.ram.clone(),
            overlays: self.overlays.clone(),
            fault_count: self.fault_count,
            last_fault: self.last_fault,
            fault_log: self.fault_log.clone(),
        }
    }

    /// Put the bus back the way it was when `snapshot` was taken.
    ///
    /// Devices are not part of a snapshot: the registered `SfrDevice`s are
    /// routing shells that point at peripherals owned elsewhere, so their state
    /// travels with the chipset's own snapshot rather than with the bus.
    pub fn restore(&mut self, snapshot: &BusSnapshot) {
        let length = snapshot.ram.len().min(self.ram.len());
        self.ram[..length].copy_from_slice(&snapshot.ram[..length]);
        self.overlays.clone_from(&snapshot.overlays);
        self.fault_count = snapshot.fault_count;
        self.last_fault = snapshot.last_fault;
        self.fault_log.clone_from(&snapshot.fault_log);
    }

    fn overlay_byte(&self, address: u32) -> Option<u8> {
        for (base, blob) in &self.overlays {
            if address >= *base && address < base + blob.len() as u32 {
                return Some(blob[(address - base) as usize]);
            }
        }
        None
    }

    /// Which ROM window, if any, covers `address`.
    fn window_for(&self, address: u32) -> Option<&RomWindow> {
        CLASSWIZ_ROM_WINDOWS
            .iter()
            .find(|window| address >= window.base && address < window.base + window.size)
    }

    /// The data read, without any watcher bookkeeping.
    fn read_data_plain(&mut self, address: u32) -> u8 {
        if let Some(byte) = self.overlay_byte(address) {
            return byte;
        }
        if let Some(window) = self.window_for(address) {
            let offset = window.rom_base + (address - window.base);
            return self.rom.byte(offset).unwrap_or_else(|| {
                self.record(Access::Read, address);
                0
            });
        }
        if (RAM_BASE..RAM_BASE + RAM_SIZE).contains(&address) {
            return self.ram[(address - RAM_BASE) as usize];
        }
        if (SFR_BASE..=SFR_END).contains(&address) {
            for device in &mut self.devices {
                if let Some(value) = device.read(address) {
                    return value;
                }
            }
            self.record(Access::SfrRead, address);
            return 0;
        }
        self.record(Access::Read, address);
        0
    }

    fn read_data_inner(&mut self, address: u32) -> u8 {
        let value = self.read_data_plain(address);
        if !self.quiet {
            let kind = self.classify_data_read(address);
            self.note_access(kind, address, value);
        }
        value
    }

    /// Which kind of fault a read of `address` would be recorded as.
    ///
    /// Only consulted when a watch is armed, so the ordinary path does not pay
    /// for it.
    fn classify_data_read(&self, address: u32) -> Access {
        if self.window_for(address).is_some()
            || self.overlay_byte(address).is_some()
            || (RAM_BASE..RAM_BASE + RAM_SIZE).contains(&address)
        {
            Access::Read
        } else if (SFR_BASE..=SFR_END).contains(&address) {
            Access::SfrRead
        } else {
            Access::Read
        }
    }

    fn classify_data_write(&self, address: u32) -> Access {
        if (SFR_BASE..=SFR_END).contains(&address) {
            Access::SfrWrite
        } else {
            Access::Write
        }
    }

    fn write_data_plain(&mut self, address: u32, value: u8) {
        if (RAM_BASE..RAM_BASE + RAM_SIZE).contains(&address) {
            self.ram[(address - RAM_BASE) as usize] = value;
            return;
        }
        if (SFR_BASE..=SFR_END).contains(&address) {
            for device in &mut self.devices {
                if device.write(address, value) {
                    return;
                }
            }
            self.record(Access::SfrWrite, address);
            return;
        }
        if self.window_for(address).is_some() || self.overlay_byte(address).is_some() {
            // ROM: dropped unless `strict_memory` is set.
            return;
        }
        self.record(Access::Write, address);
    }

    fn write_data_inner(&mut self, address: u32, value: u8) {
        if !self.quiet {
            let kind = self.classify_data_write(address);
            self.note_access(kind, address, value);
        }
        self.write_data_plain(address, value);
    }

    /// The fault kinds seen so far, for a test to assert against.
    pub fn fault_kinds(&self) -> Vec<AccessFault> {
        self.fault_log.clone()
    }

    /// The code fetch, without any watcher bookkeeping.
    fn read_code_plain(&mut self, address: u32) -> u16 {
        if let Some(byte) = self.overlay_byte(address) {
            let high = self.overlay_byte(address + 1).unwrap_or(0);
            return u16::from_le_bytes([byte, high]);
        }
        if address < 0x1_0000 {
            // `MMU::ReadCode`: segment 0 reads the image directly, bypassing the
            // 0xD000 boundary of the segment-0 ROM *region*.
            let low = self.rom.byte(address).unwrap_or_else(|| {
                self.record(Access::Code, address);
                0
            });
            let high = self.rom.byte(address + 1).unwrap_or(0);
            return u16::from_le_bytes([low, high]);
        }
        match self.window_for(address) {
            Some(window) => {
                let offset = window.rom_base + (address - window.base);
                let low = self.rom.byte(offset).unwrap_or(0);
                let high = self.rom.byte(offset + 1).unwrap_or(0);
                u16::from_le_bytes([low, high])
            }
            None => {
                self.record(Access::Code, address);
                0
            }
        }
    }
}

impl Memory for Bus {
    fn read_code(&mut self, address: u32) -> u16 {
        let value = self.read_code_plain(address);
        // Code fetches are watched only when something asked for them; the
        // ordinary path pays one length check.
        if !self.quiet && self.watches.iter().any(|watch| watch.execute) {
            self.note_access(Access::Code, address, value as u8);
        }
        value
    }

    fn read_data(&mut self, address: u32) -> u8 {
        self.read_data_inner(address & 0x00FF_FFFF)
    }

    fn write_data(&mut self, address: u32, value: u8) {
        self.write_data_inner(address & 0x00FF_FFFF, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom32k() -> Rom {
        let mut data = vec![0u8; 0x4_0000];
        // Give a few addresses distinguishable bytes.
        data[0x0000] = 0x00;
        data[0x0001] = 0xF0;
        data[0x0002] = 0x6A;
        data[0x0003] = 0x94;
        data[0x2_21B8] = 0xCE;
        data[0x2_21B9] = 0xF8;
        data[0x0_D100] = 0x11;
        Rom::new(data)
    }

    #[test]
    fn segment_zero_maps_rom_then_ram_then_sfr() {
        let mut bus = Bus::new(rom32k());
        assert_eq!(bus.read_data(0x0_0000), 0x00);

        bus.write_data(RAM_BASE + 0x180, 0x41);
        assert_eq!(bus.read_data(RAM_BASE + 0x180), 0x41);
        assert_eq!(bus.ram()[0x180], 0x41);
    }

    #[test]
    fn rom_data_windows_include_the_fifth_segment() {
        let mut bus = Bus::new(rom32k());
        // Segment 5 is a window onto ROM 0.
        assert_eq!(bus.read_data(0x5_0002), 0x6A);
        assert_eq!(bus.read_data(0x2_21B8), 0xCE);
        assert_eq!(bus.read_data(0x3_0000), bus.rom().data[0x3_0000]);
    }

    #[test]
    fn code_fetch_in_segment_zero_reaches_past_the_rom_region() {
        // `MMU::ReadCode` reads the image directly, so 0xD100 returns the ROM byte
        // even though the segment-0 ROM *region* stops at 0xCFFF.
        let mut bus = Bus::new(rom32k());
        assert_eq!(bus.read_code(0x0_D100), 0x0011);
        assert_eq!(bus.fault_count, 0);
    }

    #[test]
    fn a_long_immediate_reads_two_halfwords_from_the_image() {
        let mut bus = Bus::new(rom32k());
        // 0x0000 is `B 00h:0946Ah`: the opcode word then the address word.
        assert_eq!(bus.read_code(0x0_0000), 0xF000);
        assert_eq!(bus.read_code(0x0_0002), 0x946A);
    }

    #[test]
    fn rom_writes_are_dropped_silently() {
        let mut bus = Bus::new(rom32k());
        let before = bus.read_data(0x1_0000);
        bus.write_data(0x1_0000, 0xFF);
        assert_eq!(bus.read_data(0x1_0000), before);
        assert_eq!(bus.fault_count, 0);
    }

    #[test]
    fn unmapped_writes_and_reads_are_recorded() {
        let mut bus = Bus::new(rom32k());
        assert_eq!(bus.read_data(0x6_0000), 0);
        assert_eq!(
            bus.last_fault,
            Some(AccessFault {
                kind: Access::Read,
                address: 0x6_0000
            })
        );
        bus.write_data(0x6_0000, 1);
        assert_eq!(
            bus.last_fault,
            Some(AccessFault {
                kind: Access::Write,
                address: 0x6_0000
            })
        );
        assert_eq!(bus.fault_count, 2);
    }

    #[test]
    fn an_sfr_read_with_no_device_is_recorded_as_an_sfr_fault() {
        let mut bus = Bus::new(rom32k());
        assert_eq!(bus.read_data(0xF_050), 0);
        assert_eq!(
            bus.last_fault,
            Some(AccessFault {
                kind: Access::SfrRead,
                address: 0xF_050
            })
        );
    }

    struct FakeSfr;
    impl SfrDevice for FakeSfr {
        fn read(&mut self, address: u32) -> Option<u8> {
            (address == 0xF_040).then_some(0xFE)
        }
        fn write(&mut self, address: u32, _value: u8) -> bool {
            address == 0xF_040
        }
        fn name(&self) -> &'static str {
            "fake"
        }
    }

    #[test]
    fn a_registered_device_takes_precedence_over_the_unmapped_path() {
        let mut bus = Bus::new(rom32k());
        bus.register(Box::new(FakeSfr));
        assert_eq!(bus.read_data(0xF_040), 0xFE);
        assert_eq!(bus.fault_count, 0);
        bus.write_data(0xF_041, 0x00);
        assert_eq!(bus.fault_count, 1, "the device only owns 0xF040");
    }

    #[test]
    fn overlays_shadow_rom_for_code_and_data() {
        let mut bus = Bus::new(rom32k());
        bus.load_code(0x1_0000, &[0x8E, 0xF2]);
        assert_eq!(bus.read_code(0x1_0000), 0xF28E);
        assert_eq!(bus.read_data(0x1_0000), 0x8E);
        // The real image is untouched.
        assert_eq!(bus.rom().data[0x1_0000], 0x00);
    }

    #[test]
    fn the_fault_log_is_bounded_but_the_counter_is_not() {
        let mut bus = Bus::new(rom32k());
        for i in 0..(MAX_LOGGED_FAULTS as u32 + 100) {
            bus.read_data(0x6_0000 + i);
        }
        assert_eq!(bus.fault_count, MAX_LOGGED_FAULTS as u64 + 100);
        assert_eq!(bus.fault_log.len(), MAX_LOGGED_FAULTS);
    }

    // ------------------------------------------------------------------ regions

    #[test]
    fn regions_follow_the_segment_zero_map() {
        let bus = Bus::new(rom32k());
        assert_eq!(
            bus.region(0x0_0100),
            Region::Rom {
                base: 0x0_0000,
                rom_base: 0x0_0000
            }
        );
        assert_eq!(
            bus.region(0x1_0100),
            Region::Rom {
                base: 0x1_0000,
                rom_base: 0x1_0000
            }
        );
        // Segment 5 is the second window onto the bottom of the image.
        assert_eq!(
            bus.region(0x5_0100),
            Region::Rom {
                base: 0x5_0000,
                rom_base: 0x0_0000
            }
        );
        // RAM and the SFRs are in segment 0, not 0xD/0xF.
        assert_eq!(bus.region(0x0_D180), Region::Ram);
        assert_eq!(bus.region(0x0_F042), Region::Sfr);
        assert_eq!(bus.region(0xD_D180), Region::Unmapped);
        assert_eq!(bus.region(0x6_0000), Region::Unmapped);
    }

    // ------------------------------------------------------------------- quiet

    #[test]
    fn a_quiet_read_records_no_fault() {
        let mut bus = Bus::new(rom32k());
        assert_eq!(bus.peek_quiet(0x6_0000), 0);
        assert_eq!(bus.fault_count, 0, "a debugger's read is not a guest fault");
        assert!(bus.last_fault.is_none());

        // The ordinary read still records, so the difference is real.
        bus.read_data(0x6_0000);
        assert_eq!(bus.fault_count, 1);
    }

    #[test]
    fn a_quiet_read_does_not_trip_a_watch() {
        use nxu8_core::Memory;

        let mut bus = Bus::new(rom32k());
        bus.watch(Watch::data(0x0_D180));
        bus.write_data(0x0_D180, 0x41);
        bus.take_watch_hits();

        assert_eq!(bus.peek_quiet(0x0_D180), 0x41);
        assert!(
            bus.take_watch_hits().is_empty(),
            "the debugger's own read must not look like the guest's"
        );

        bus.read_data(0x0_D180);
        assert_eq!(
            bus.take_watch_hits().len(),
            1,
            "the guest's read does trip it"
        );
    }

    #[test]
    fn a_quiet_code_fetch_records_no_fault() {
        let mut bus = Bus::new(rom32k());
        bus.read_code_quiet(0x0_0000);
        assert_eq!(bus.fault_count, 0);
        // 0x00946A is past the 32 KiB test image, so the plain path would record.
        bus.read_code_quiet(0x0_946A);
        assert_eq!(bus.fault_count, 0);
    }

    // ------------------------------------------------------------------ watches

    #[test]
    fn a_watch_reports_the_value_and_direction_of_the_access() {
        let mut bus = Bus::new(rom32k());
        bus.watch(Watch::data(0x0_D180));

        use nxu8_core::Memory;
        bus.write_data(0x0_D180, 0x5A);
        let hits = bus.take_watch_hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0],
            WatchHit {
                kind: Access::Write,
                address: 0x0_D180,
                value: 0x5A
            }
        );

        assert_eq!(bus.read_data(0x0_D180), 0x5A);
        let hits = bus.take_watch_hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, Access::Read);
        assert_eq!(hits[0].value, 0x5A);
    }

    #[test]
    fn watch_directions_are_independent() {
        let mut bus = Bus::new(rom32k());
        bus.watch(Watch {
            address: 0x0_D180,
            read: false,
            write: true,
            execute: false,
        });

        use nxu8_core::Memory;
        bus.write_data(0x0_D180, 1);
        assert_eq!(bus.take_watch_hits().len(), 1);
        bus.read_data(0x0_D180);
        assert!(
            bus.take_watch_hits().is_empty(),
            "a write-only watch ignores reads"
        );
    }

    #[test]
    fn an_execute_watch_sees_a_code_fetch() {
        let mut bus = Bus::new(rom32k());
        bus.watch(Watch {
            address: 0x0_0000,
            read: false,
            write: false,
            execute: true,
        });

        use nxu8_core::Memory;
        bus.read_code(0x0_0000);
        let hits = bus.take_watch_hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, Access::Code);
    }

    #[test]
    fn a_watch_on_an_unmapped_address_still_reports() {
        // "read as 0" and "genuinely 0" are different, which is the whole point
        // of the fault log; a watch has to behave the same way.
        let mut bus = Bus::new(rom32k());
        bus.watch(Watch::data(0x6_0000));
        bus.read_data(0x6_0000);
        assert_eq!(bus.take_watch_hits().len(), 1);
        assert_eq!(bus.fault_count, 1, "and it is still a fault");
    }

    #[test]
    fn watch_hit_count_survives_draining_but_the_buffer_is_capped() {
        let mut bus = Bus::new(rom32k());
        bus.watch(Watch::data(0x0_D180));
        use nxu8_core::Memory;
        for i in 0..(MAX_BUFFERED_WATCH_HITS as u32 + 25) {
            bus.write_data(0x0_D180, i as u8);
        }
        assert_eq!(bus.watch_hit_count(), MAX_BUFFERED_WATCH_HITS as u64 + 25);
        assert_eq!(bus.take_watch_hits().len(), MAX_BUFFERED_WATCH_HITS);
        // Draining does not reset the counter, so "many, most dropped" is
        // distinguishable from "none".
        assert_eq!(bus.watch_hit_count(), MAX_BUFFERED_WATCH_HITS as u64 + 25);
        assert!(bus.take_watch_hits().is_empty());
    }

    #[test]
    fn watches_are_replaced_by_address_and_removable() {
        let mut bus = Bus::new(rom32k());
        bus.watch(Watch::data(0x0_D180));
        bus.watch(Watch {
            address: 0x0_D180,
            read: true,
            write: false,
            execute: false,
        });
        assert_eq!(bus.watches().len(), 1, "a second watch replaces the first");

        assert!(bus.unwatch(0x0_D180));
        assert!(
            !bus.unwatch(0x0_D180),
            "removing twice reports nothing removed"
        );
        assert!(bus.watches().is_empty());
    }

    // ----------------------------------------------------------------- overlays

    #[test]
    fn a_planted_block_can_be_listed_and_removed() {
        let mut bus = Bus::new(rom32k());
        bus.load_code(0x1_0000, &[0x8E, 0xF2]);
        assert_eq!(bus.overlays(), &[(0x1_0000, vec![0x8E, 0xF2])]);

        assert!(bus.clear_code(0x1_0000));
        assert!(bus.overlays().is_empty());
        assert!(
            !bus.clear_code(0x1_0000),
            "clearing twice reports nothing removed"
        );
        assert_eq!(
            bus.read_code(0x1_0000),
            0x0000,
            "the image shows through again"
        );
    }

    // ---------------------------------------------------------------- snapshots

    #[test]
    fn a_snapshot_round_trips_the_state_it_covers() {
        let mut bus = Bus::new(rom32k());
        bus.write_data(0x0_D180, 0x11);
        bus.load_code(0x1_0000, &[0x8E, 0xF2]);
        bus.read_data(0x6_0000); // one fault, so the counters are non-trivial
        let snapshot = bus.snapshot();

        bus.write_data(0x0_D180, 0x99);
        bus.clear_code(0x1_0000);
        bus.read_data(0x7_0000);
        assert_ne!(bus.snapshot(), snapshot);

        bus.restore(&snapshot);
        assert_eq!(bus.snapshot(), snapshot);
        assert_eq!(bus.read_data(0x0_D180), 0x11);
        assert_eq!(bus.read_code(0x1_0000), 0xF28E, "patches come back too");
        assert_eq!(bus.fault_count, 1);
    }
}
