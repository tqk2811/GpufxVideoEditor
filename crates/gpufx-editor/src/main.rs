fn main() {
    if let Err(message) = gpufx_editor::window::run() {
        eprintln!("gpufx-editor: {message}");
        std::process::exit(1);
    }
}
