#![windows_subsystem = "windows"]

use std::process::ExitCode;

fn main() -> ExitCode {
    match looppanel::tray::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            looppanel::tray::show_fatal_error(&error);
            ExitCode::FAILURE
        }
    }
}
