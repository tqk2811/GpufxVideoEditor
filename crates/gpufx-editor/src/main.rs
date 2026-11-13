// A release build is a window app, so Windows opens no console for it. A debug build keeps the console
// for its log lines.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(message) = gpufx_editor::window::run() {
        eprintln!("gpufx-editor: {message}");
        show_error(&message);
        std::process::exit(1);
    }
}

/// A window app has no console to print to, so a failed start is shown in a message box as well.
#[cfg(windows)]
fn show_error(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (text, caption) = (wide(message), wide("gpufx editor"));
    // SAFETY: both strings end with a nul and outlive the call.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), MB_OK | MB_ICONERROR) };
}

#[cfg(not(windows))]
fn show_error(_message: &str) {}
