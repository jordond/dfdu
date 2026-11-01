//! `dfdu-gui`: the dfdu browser in a window, on the same session driver as the terminal browser
//! (`dfdu_session`). The command line (`args`) names what to open, built as `dfdu` builds it
//! (`launch`); without a directory the start screen (`start`) lists the disks. The window
//! (`app`) shows the browser (`browser`): the toolbar (`toolbar`) with the tabs of the views,
//! the list (`list`) with the menu of a row (`menu`), the map (`map`), the cleanup view with its
//! native command pane (`cleanup`), the duplicates (`dupes`), the largest files (`largest`), the
//! keys and ticks of the review views (`review`), the details panel (`details`), the action and
//! status bars (`status_bar`), the delete dialog (`dialog`), the error list and type picker
//! (`windows`), the help (`help`) and the command palette (`palette`), coloured by the theme's
//! tokens (`theme`), with the terminal's keys (`keys`). On macOS a banner says when the window
//! lacks Full Disk Access (`access`). A thread asks for a frame when the
//! engine's model changes (`bridge`). The theme chosen and how the list was shown are kept
//! between runs (`settings`).
//!
//! The window draws with wgpu, or with OpenGL (glow) when wgpu cannot start, with dfdu's mark as
//! its icon.

mod access;
mod app;
mod args;
mod bridge;
mod browser;
mod cleanup;
mod details;
mod dialog;
mod dupes;
mod help;
mod keys;
mod kit;
mod largest;
mod launch;
mod list;
mod map;
mod menu;
mod palette;
mod review;
mod settings;
mod shot;
mod start;
mod status_bar;
mod theme;
mod toolbar;
mod windows;

#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;

use dfdu_session::palette::ThemeFile;

use crate::app::Gui;
use crate::launch::Launch;
use crate::settings::ThemeChoice;

/// The window's title and the application's name.
const NAME: &str = "dfdu";
/// The window's and the application's icon: dfdu's mark, as the website shows it.
const ICON: &[u8] = include_bytes!("../../../assets/logo/dfdu-mark-512.png");

fn main() -> ExitCode {
    let args = args::Args::parse();
    let launch = match launch::prepare(&args) {
        Ok(launch) => launch,
        Err(e) => {
            eprintln!("dfdu-gui: {e}");
            return ExitCode::from(1);
        }
    };
    let themes = dfdu_sys::dirs::theme_dir();
    let theme = match args.theme.as_deref() {
        Some(name) => match theme::named(name, themes.as_deref()) {
            Ok(theme) => Some(theme),
            Err(e) => {
                eprintln!("dfdu-gui: theme {name}: {e}");
                return ExitCode::from(1);
            }
        },
        None => None,
    };
    let pending = Cell::new(Some(launch));
    let mut result = run(eframe::Renderer::Wgpu, &pending, theme.as_ref());
    if let Err(eframe::Error::Wgpu(e)) = &result {
        // The window was not created: nothing was started yet.
        if let Some(launch) = pending.take() {
            eprintln!("dfdu-gui: wgpu cannot draw here ({e}); using OpenGL");
            pending.set(Some(launch));
            result = run(eframe::Renderer::Glow, &pending, theme.as_ref());
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
/// is created, with the theme `--theme` chose, or else the one chosen last.
fn run(
    renderer: eframe::Renderer,
    pending: &Cell<Option<Launch>>,
    theme: Option<&(ThemeChoice, Option<ThemeFile>)>,
) -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(NAME)
        .with_app_id(NAME)
        .with_inner_size([1100.0, 720.0])
        .with_min_inner_size([480.0, 320.0])
        .with_drag_and_drop(true);
    // Without it, eframe's own icon is shown.
    if let Ok(icon) = eframe::icon_data::from_png_bytes(ICON) {
        viewport = viewport.with_icon(icon);
    }
    let opts = eframe::NativeOptions {
        renderer,
        viewport,
        persistence_path: settings::path(),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        NAME,
        opts,
        Box::new(|cc| {
            let launch = pending.take().ok_or("the window was opened twice")?;
            let mut prefs = settings::Prefs::load(cc.storage);
            let themes = dfdu_sys::dirs::theme_dir();
            let file = match theme.cloned() {
                Some((choice, file)) => {
                    prefs.theme = choice;
                    file
                }
                None => theme::saved(&mut prefs.theme, themes.as_deref()),
            };
            let gui = Gui::new(launch, prefs, file, themes, &cc.egui_ctx);
            Ok(match std::env::var_os(shot::VAR) {
                Some(path) => Box::new(shot::Shot {
                    gui,
                    path: Some(path.into()),
                    keys: std::env::var(shot::KEYS).ok(),
                    typed: None,
                    opened: Instant::now(),
                    done: None,
                }),
                None => Box::new(gui),
            })
        }),
    )
}

#[cfg(test)]
mod icon_tests {
    #[test]
    fn the_icon_reads() {
        let icon = eframe::icon_data::from_png_bytes(super::ICON).expect("a PNG");
        assert_eq!((icon.width, icon.height), (512, 512));
    }
}
