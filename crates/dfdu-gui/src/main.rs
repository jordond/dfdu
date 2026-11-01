//! `dfdu-gui`: the dfdu browser in a window, on the same session driver as the terminal browser
//! (`dfdu_session`). The command line (`args`) names what to open, built as `dfdu` builds it
//! (`launch`); without a directory the start screen (`start`) lists the disks. The window
//! (`app`) shows the browser (`browser`): the toolbar (`toolbar`), the list (`list`) and the
//! status bar (`status_bar`), coloured by the theme's tokens (`theme`), with the terminal's keys
//! (`keys`). A thread asks for a frame when the engine's model changes (`bridge`).
//!
//! The window draws with wgpu, or with OpenGL (glow) when wgpu cannot start.

mod app;
mod args;
mod bridge;
mod browser;
mod keys;
mod launch;
mod list;
mod shot;
mod start;
mod status_bar;
mod theme;
mod toolbar;

#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;

use crate::app::Gui;
use crate::launch::Launch;

/// The window's title and the application's name.
const NAME: &str = "dfdu";

fn main() -> ExitCode {
    let args = args::Args::parse();
    let launch = match launch::prepare(&args) {
        Ok(launch) => launch,
        Err(e) => {
            eprintln!("dfdu-gui: {e}");
            return ExitCode::from(1);
        }
    };
    let pending = Cell::new(Some(launch));
    let mut result = run(eframe::Renderer::Wgpu, &pending);
    if let Err(eframe::Error::Wgpu(e)) = &result {
        // The window was not created: nothing was started yet.
        if let Some(launch) = pending.take() {
            eprintln!("dfdu-gui: wgpu cannot draw here ({e}); using OpenGL");
            pending.set(Some(launch));
            result = run(eframe::Renderer::Glow, &pending);
        }
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dfdu-gui: {e}");
            ExitCode::from(1)
        }
    }
}

/// Opens the window with `renderer` on the launch in `pending`, which it takes once the window
/// is created.
fn run(renderer: eframe::Renderer, pending: &Cell<Option<Launch>>) -> eframe::Result {
    let opts = eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_title(NAME)
            .with_app_id(NAME)
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([480.0, 320.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        NAME,
        opts,
        Box::new(|cc| {
            let launch = pending.take().ok_or("the window was opened twice")?;
            let gui = Gui::new(launch, &cc.egui_ctx);
            Ok(match std::env::var_os(shot::VAR) {
                Some(path) => Box::new(shot::Shot {
                    gui,
                    path: Some(path.into()),
                    opened: Instant::now(),
                    done: None,
                }),
                None => Box::new(gui),
            })
        }),
    )
}
