// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The UAC-elevated helper is this same binary started again with a private
    // flag. It has to be dispatched before Tauri boots: the single-instance
    // plugin would otherwise mistake the helper for a second launch of the app
    // and terminate it.
    if let Some(code) = windle_lib::commands::optimize::elevated_entrypoint() {
        std::process::exit(code);
    }

    windle_lib::run()
}
