#![windows_subsystem = "windows"]

use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match looppanel::temperature_service::run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if !arguments.is_empty() {
                looppanel::tray::show_fatal_error(&error);
            }
            ExitCode::FAILURE
        }
    }
}
