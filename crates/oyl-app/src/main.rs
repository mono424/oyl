//! Oyl: a native paper-writing IDE.
//!
//! ```text
//! oyl                 reopen the last paper, else the library
//! oyl <folder>        open that paper (a folder without oyl.toml is adopted)
//! oyl --self-test     drive a real window through Oyl's main paths, in a
//!                       temporary data folder, and exit 0 when all pass
//! ```
//!
//! `OYL_DATA_DIR` moves everything Oyl keeps (settings, library,
//! caches, agent environments) to another folder; `OYL_LOG=debug` prints
//! more than warnings.

mod actions;
mod assets;
mod cloud_view;
mod coalesce;
mod fade;
mod fuzzy;
mod gate;
mod layout;
mod library_view;
mod logger;
mod overlays;
mod palette;
mod pdf_viewer;
mod persist;
mod selftest;
mod shell;
mod status;

use std::path::PathBuf;

use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Size, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowOptions,
    point, px, size,
};
use oyl_ui::Appearance;

use crate::persist::{AppPaths, WindowFrame};
use crate::shell::{Agents, Launch, Shell, ShellOptions};

/// What the command line asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Command {
    /// Open the app (on a folder, or as it was).
    Run(Launch),
    /// Run the self-test.
    SelfTest,
    /// Print the usage.
    Help,
}

const USAGE: &str = "usage: oyl [FOLDER] | oyl --self-test";

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let args: Vec<String> = args.into_iter().collect();
    match args.as_slice() {
        [] => Ok(Command::Run(Launch::Restore)),
        [flag] if flag == "--self-test" => Ok(Command::SelfTest),
        [flag] if flag == "--help" || flag == "-h" => Ok(Command::Help),
        [flag] if flag.starts_with('-') => Err(format!("unknown option {flag}\n{USAGE}")),
        [folder] => {
            let path = PathBuf::from(folder);
            Ok(Command::Run(Launch::Folder(
                std::path::absolute(&path).unwrap_or(path),
            )))
        }
        _ => Err(USAGE.to_string()),
    }
}

/// A GUI-launched process on macOS starts with SIGCHLD ignored: children
/// would be reaped by the kernel (`wait()` fails with ECHILD) and would
/// inherit the ignore (`uv run` then never reaps its Python and hangs).
/// Puts the default disposition back.
#[cfg(unix)]
fn restore_sigchld() {
    // SAFETY: called first thing in `main`, before any thread exists or any
    // child is spawned; it only resets one signal's disposition.
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
    }
}

/// Whether SIGCHLD is ignored in this process (what [`restore_sigchld`]
/// undoes); the self-test checks it stays undone.
#[cfg(unix)]
pub(crate) fn sigchld_ignored() -> bool {
    // SAFETY: a query only: a null `act` leaves the disposition unchanged,
    // and `old` is a valid, zeroed `sigaction` to write into.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut old) == 0
            && old.sa_sigaction == libc::SIG_IGN
    }
}

fn main() {
    #[cfg(unix)]
    restore_sigchld();
    logger::init();
    let command = match parse_args(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    match command {
        Command::Help => println!("{USAGE}"),
        Command::SelfTest => selftest::run(),
        Command::Run(launch) => run(AppPaths::from_env(), launch),
    }
}

/// Everything the app sets up once, before its first window.
pub(crate) fn init_app(cx: &mut App) {
    gpui_kit::init(cx);
    oyl_ui::install(Appearance::System, cx);
    oyl_panels::init(cx);
    actions::bind_keys(cx);
    cx.set_menus(actions::menus());
    cx.on_action(|_: &actions::Quit, cx| cx.quit());
    cx.on_action(|_: &actions::Hide, cx| cx.hide());
    cx.on_action(|_: &actions::HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &actions::ShowAll, cx| cx.unhide_other_apps());
}

/// The options of Oyl's window: frosted, a transparent title bar with the
/// traffic lights, at least [`layout::WINDOW_MIN`], where it was left.
pub(crate) fn window_options(frame: Option<WindowFrame>, focus: bool, cx: &App) -> WindowOptions {
    let (min_w, min_h) = layout::WINDOW_MIN;
    let bounds = match frame.filter(|frame| frame_is_visible(frame, cx)) {
        Some(frame) => {
            let bounds = Bounds {
                origin: point(px(frame.x), px(frame.y)),
                size: size(px(frame.width), px(frame.height)),
            };
            if frame.fullscreen {
                WindowBounds::Fullscreen(bounds)
            } else if frame.maximized {
                WindowBounds::Maximized(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            }
        }
        None => {
            let (w, h) = layout::WINDOW_DEFAULT;
            WindowBounds::centered(size(px(w), px(h)), cx)
        }
    };
    WindowOptions {
        window_bounds: Some(bounds),
        titlebar: Some(TitlebarOptions {
            title: Some("Oyl".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(
                px(layout::TRAFFIC_LIGHTS.0),
                px(layout::TRAFFIC_LIGHTS.1),
            )),
        }),
        window_background: WindowBackgroundAppearance::Blurred,
        // The header row is the title bar: its empty area moves the window.
        app_owns_titlebar_drag: true,
        window_min_size: Some(Size {
            width: px(min_w),
            height: px(min_h),
        }),
        focus,
        app_id: Some("dev.oyl.Oyl".into()),
        ..Default::default()
    }
}

/// Whether a saved frame still overlaps a display enough to grab it.
fn frame_is_visible(frame: &WindowFrame, cx: &App) -> bool {
    let displays = cx.displays();
    if displays.is_empty() {
        return true;
    }
    displays.iter().any(|display| {
        let screen = display.bounds();
        let left = f32::from(screen.origin.x);
        let top = f32::from(screen.origin.y);
        let right = left + f32::from(screen.size.width);
        let bottom = top + f32::from(screen.size.height);
        // At least 120 × 40 points of the window's top on this display.
        frame.x + frame.width >= left + 120.
            && frame.x <= right - 120.
            && frame.y >= top - 1.
            && frame.y <= bottom - 40.
    })
}

/// Opens Oyl's window and runs until it is closed.
fn run(paths: AppPaths, launch: Launch) {
    gpui_kit::application()
        .with_assets(assets::OylAssets)
        .run(move |cx| {
            init_app(cx);
            let frame = WindowFrame::load(&paths);
            let options = window_options(frame, true, cx);
            let opened = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    Shell::new(
                        ShellOptions {
                            paths: paths.clone(),
                            launch: launch.clone(),
                            agents: Agents::Detect,
                            cloud: Default::default(),
                        },
                        window,
                        cx,
                    )
                })
            });
            let (handle, shell) = match opened {
                Ok(opened) => opened,
                Err(err) => {
                    eprintln!("Oyl could not open its window: {err:#}");
                    std::process::exit(1);
                }
            };
            let _ = handle.update(cx, |_, window, cx| {
                let shell = shell.clone();
                window.on_window_should_close(cx, move |window, cx| {
                    shell.update(cx, |shell, cx| shell.should_close(window, cx))
                });
            });
            let quitting = shell.clone();
            cx.on_app_quit(move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    quitting.update(cx, |shell, cx| shell.save_everything(window, cx));
                });
                async {}
            })
            .detach();
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Command, String> {
        parse_args(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn the_command_line_is_read() {
        assert_eq!(args(&[]), Ok(Command::Run(Launch::Restore)));
        assert_eq!(args(&["--self-test"]), Ok(Command::SelfTest));
        assert_eq!(args(&["--help"]), Ok(Command::Help));
        assert!(args(&["--nope"]).is_err());
        assert!(args(&["a", "b"]).is_err());
        match args(&["paper"]) {
            Ok(Command::Run(Launch::Folder(path))) => {
                assert!(path.is_absolute());
                assert!(path.ends_with("paper"));
            }
            other => panic!("{other:?}"),
        }
    }
}
