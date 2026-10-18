//! Gagent desktop application.

// Release builds on Windows are GUI programs: no console window appears.
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

fn main() {
    let dirs = gagent::config::AppDirs::default_location();
    if let Err(e) = gagent::gui::run(dirs) {
        let message = format!("Gagent could not start the window:\n\n{e}\n\nIf you are on Linux, make sure an X11 or Wayland session and OpenGL drivers are available.");
        eprintln!("{message}");
        let _ = rfd::MessageDialog::new()
            .set_title("Gagent")
            .set_description(&message)
            .set_level(rfd::MessageLevel::Error)
            .show();
        std::process::exit(1);
    }
}
