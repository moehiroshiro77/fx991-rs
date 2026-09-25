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

//! Platform window fix-ups.
//!
//! One thing needs a platform-specific call, and it is Windows-only: blocking
//! maximise, which `resizable(false)` alone does not do because Aero Snap goes
//! through `WM_SYSCOMMAND` rather than the window styles.

use winit::window::Window;

/// Stop the window from being maximised, on Windows.
///
/// `with_resizable(false)` clears `WS_SIZEBOX`, and
/// `with_enabled_buttons(.)` clears `WS_MAXIMIZEBOX`, which together remove the
/// drag borders and grey out the title-bar button.  Neither stops
/// `WM_SYSCOMMAND`/`SC_MAXIMIZE`, and that is the path Aero Snap uses: drag the
/// window to the top of the screen, or press **Win+Up**, and the window still
/// fills the display -- with a client area the skin cannot fill.
///
/// winit has no hook for the window procedure, so the window is subclassed: the
/// original procedure is saved and a small one is installed in front of it that
/// swallows `SC_MAXIMIZE` (and `SC_RESTORE`, so a maximised window cannot come
/// back through the same door) and forwards everything else.
///
/// This is the only platform-specific code in the crate, and it is `cfg`-gated:
/// macOS clears `NSWindowStyleMask::Resizable`, which *is* its zoom button, and
/// X11/Wayland clear the maximise hint from `resizable(false)`, so neither needs
/// anything here.
#[cfg(windows)]
pub fn block_maximise(window: &Window) {
    use std::sync::OnceLock;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    const GWLP_WNDPROC: i32 = -4;
    const WM_SYSCOMMAND: u32 = 0x0112;
    const SC_MAXIMIZE: usize = 0xF030;

    #[link(name = "user32")]
    extern "system" {
        fn GetWindowLongPtrW(hwnd: *mut core::ffi::c_void, index: i32) -> isize;
        fn SetWindowLongPtrW(hwnd: *mut core::ffi::c_void, index: i32, value: isize) -> isize;
        fn CallWindowProcW(
            prev: isize,
            hwnd: *mut core::ffi::c_void,
            msg: u32,
            wparam: usize,
            lparam: isize,
        ) -> isize;
    }

    /// The procedure the window had before we replaced it.
    ///
    /// A `static` rather than a captured value because a window procedure is a
    /// bare `extern "system"` fn with no room for state.  One window per
    /// process, so one slot is enough.
    static PREVIOUS: OnceLock<isize> = OnceLock::new();

    unsafe extern "system" fn hook(
        hwnd: *mut core::ffi::c_void,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        if msg == WM_SYSCOMMAND {
            // The low four bits are reserved by the system and must be masked
            // off before comparing (documented behaviour of WM_SYSCOMMAND).
            let command = wparam & !0x0F;
            // Swallow "maximise".  `SC_RESTORE` is left alone: it is also the
            // command that brings a minimised window back, so blocking it would
            // strand the user with a taskbar icon they cannot click.
            if command == SC_MAXIMIZE {
                return 0;
            }
        }
        let previous = PREVIOUS.get().copied().unwrap_or(0);
        unsafe { CallWindowProcW(previous, hwnd, msg, wparam, lparam) }
    }

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return;
    };
    let hwnd = win32.hwnd.get() as *mut core::ffi::c_void;
    // SAFETY: `hwnd` comes from winit and is valid for the window's lifetime;
    // `hook` is a valid `extern "system"` procedure.
    unsafe {
        let previous = GetWindowLongPtrW(hwnd, GWLP_WNDPROC);
        if previous != 0 && PREVIOUS.set(previous).is_ok() {
            let hook_address = hook as unsafe extern "system" fn(
                *mut core::ffi::c_void,
                u32,
                usize,
                isize,
            ) -> isize;
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, hook_address as usize as isize);
        }
    }
}

#[cfg(not(windows))]
pub fn block_maximise(_window: &Window) {}
