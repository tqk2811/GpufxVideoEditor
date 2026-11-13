//! Makes a DPI change place the window where Windows suggests.
//!
//! When the window is dragged onto a monitor with another scale, Windows sends `WM_DPICHANGED` with a
//! suggested rectangle. winit 0.30 ignores it and moves the window to a rectangle of its own. With
//! monitors of different scales this left the taskbar button on the wrong monitor's taskbar, one drag
//! behind, when the taskbar shows apps where their window is open. A subclass lets winit handle the
//! message as before, then swaps winit's rectangle for the suggested one in the move it makes.

#[cfg(windows)]
mod imp {
    use std::cell::Cell;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsZoomed, SWP_NOMOVE, SWP_NOSIZE, WINDOWPOS, WM_DPICHANGED, WM_NCDESTROY, WM_WINDOWPOSCHANGING};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    const SUBCLASS_ID: usize = 0x6770_6678;

    thread_local! {
        /// The rectangle Windows suggested, while winit handles the `WM_DPICHANGED` that carried it.
        static SUGGESTED: Cell<Option<RECT>> = const { Cell::new(None) };
    }

    pub fn install(window: &winit::window::Window) {
        let Ok(handle) = window.window_handle() else { return };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else { return };
        let hwnd = handle.hwnd.get() as HWND;
        // SAFETY: `hwnd` is the live window winit just created, on this thread.
        unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0) };
    }

    unsafe extern "system" fn subclass_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
        match message {
            WM_DPICHANGED => {
                // SAFETY: for WM_DPICHANGED, `lparam` points to the suggested RECT.
                let suggested = unsafe { *(lparam as *const RECT) };
                SUGGESTED.set(Some(suggested));
                let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
                SUGGESTED.set(None);
                result
            }
            WM_WINDOWPOSCHANGING => {
                if let Some(rect) = SUGGESTED.get() {
                    // SAFETY: for WM_WINDOWPOSCHANGING, `lparam` points to a writable WINDOWPOS.
                    let pos = unsafe { &mut *(lparam as *mut WINDOWPOS) };
                    // A maximised window keeps the placement Windows gives it.
                    if pos.flags & (SWP_NOMOVE | SWP_NOSIZE) == 0 && unsafe { IsZoomed(hwnd) } == 0 {
                        pos.x = rect.left;
                        pos.y = rect.top;
                        pos.cx = rect.right - rect.left;
                        pos.cy = rect.bottom - rect.top;
                    }
                }
                unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
            }
            WM_NCDESTROY => {
                unsafe { RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID) };
                unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
            }
            _ => unsafe { DefSubclassProc(hwnd, message, wparam, lparam) },
        }
    }
}

#[cfg(windows)]
pub use imp::install;

#[cfg(not(windows))]
pub fn install(_window: &winit::window::Window) {}
