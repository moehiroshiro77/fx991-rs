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

//! fx-991CN X emulator: the scripting façade and the calculator front end.

#![warn(missing_docs)]

pub mod calculator;
pub mod emu;
pub mod listing;
pub mod throttle;

pub use calculator::{Calculator, NotReady};
pub use emu::{AddressKind, Breakpoint, BudgetExceeded, Emu, Mode, Trace};
pub use fx991_chipset::{Chipset, RunMode, TickOutcome};
pub use throttle::Throttle;
