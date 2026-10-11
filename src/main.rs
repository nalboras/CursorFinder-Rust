#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    cursor_finder::windows_app::run();

    #[cfg(not(windows))]
    {
        eprintln!("CursorFinder is a Windows application.");
        std::process::exit(1);
    }
}
