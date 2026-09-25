//! `disktree`: find what is eating a volume, mark it, and remove it.
//!
//! The window opens on a treemap of the scanned root — the home directory
//! unless another path is given — with a breadcrumb bar, a selection line, and a
//! live free-space meter. Marking is non-destructive until the review screen
//! is confirmed.

// A release build on Windows is a GUI program, so launching it from the
// Start menu does not open a console window next to it. Debug builds keep
// the console for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod git;
mod marks;
mod palette;
mod state;
#[cfg(test)]
mod tests;
mod treemap_view;
mod ui;
mod views;
mod widgets;

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use disktree_core::scan::ScanOptions;
use gpui_kit::{AppContext as _, WindowOptions, px, size};
use state::Disktree;

/// What the command line asked for.
#[derive(Debug)]
struct Args {
    root: PathBuf,
    options: ScanOptions,
    depth: u32,
}

const USAGE: &str = "\
disktree — a treemap of what is using your disk

usage: disktree [OPTIONS] [PATH]

arguments:
  PATH              directory to scan (default: the home directory)

The window opens on a treemap of the root, largest first. Space marks the
selected tile, Enter opens it, c reviews the marked list, ? lists every key.

options:
  -a, --apparent-size   measure apparent length instead of allocated blocks
  -l, --follow-links    follow symlinks
  -H, --no-hidden       skip dotfiles and dot-directories
  -D, --disk            scan the whole disk the home directory is on
  -X, --cross-filesystems
                        also measure other disks, network shares and pseudo
                        filesystems mounted below PATH (off by default)
  -d, --depth N         how many levels to draw at once (1-6, default 3)
      --metric files    rank by file count instead of bytes
  -W, --walk            Windows: always walk the directories, never read the
                        NTFS file table (read by default for a whole drive
                        or the home directory, when run as administrator)
  -h, --help            show this help
";

fn main() -> Result<()> {
    attach_parent_console();
    let args = parse_args()?;
    let root = args.root.clone();
    let depth = args.depth;
    let title_root = root.clone();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_omarchy::init(cx);
            let options = args.options.clone();
            let root_for_app = root.clone();
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(gpui_kit::WindowBounds::Windowed(
                            gpui_kit::Bounds::new(
                                gpui_kit::point(px(120.), px(90.)),
                                size(px(1440.), px(900.)),
                            ),
                        )),
                        titlebar: Some(gpui_kit::TitlebarOptions {
                            title: Some(
                                format!(
                                    "disktree · {}",
                                    marks::display_path(
                                        &title_root,
                                        disktree_core::paths::home_dir()
                                            .as_deref(),
                                    )
                                )
                                .into(),
                            ),
                            ..Default::default()
                        }),
                        // Below this the treemap stops being readable, so ask
                        // the compositor not to go there.
                        window_min_size: Some(size(px(900.), px(600.))),
                        ..Default::default()
                    },
                    move |_, cx| {
                        cx.new(|cx| {
                            Disktree::new(
                                root_for_app.clone(),
                                options.clone(),
                                depth,
                                cx,
                            )
                        })
                    },
                )
                .expect("open the disktree window");

            // The treemap owns the keyboard from the first frame; there is no
            // text field to focus first.
            let _ = window.update(cx, |this, window, cx| {
                let focus = this.focus.clone();
                window.focus(&focus, cx);
            });
            cx.activate(true);
        });
    Ok(())
}

/// A GUI-subsystem program starts without a console, so `--help` and
/// argument errors would vanish when it is run from a terminal. Borrow the
/// terminal's console when there is one; from the Start menu there is none
/// and nothing changes.
#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "AttachConsole is a plain Win32 call with no pointers; it has \
              no safe wrapper"
)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole,
    };
    // SAFETY: takes a process id by value and touches no Rust memory. A
    // failure only means there is no parent console, which is fine.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(not(windows))]
const fn attach_parent_console() {}

fn parse_args() -> Result<Args> {
    let mut root: Option<PathBuf> = None;
    let mut options = ScanOptions::default();
    let mut depth = 3_u32;
    let mut disk = false;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "-a" | "--apparent-size" => options.apparent_size = true,
            "-l" | "--follow-links" => options.follow_links = true,
            "-H" | "--no-hidden" => options.include_hidden = false,
            // Staying on one volume is the default; the flag is kept so
            // old invocations still work.
            "-x" | "--one-filesystem" => options.one_filesystem = true,
            "-X" | "--cross-filesystems" => options.one_filesystem = false,
            "-W" | "--walk" => {
                options.file_table = disktree_core::scan::FileTable::Never;
            }
            "-D" | "--disk" => disk = true,
            "-d" | "--depth" => {
                let value = args.next().context("--depth needs a number")?;
                depth = value.parse().context("--depth needs a number")?;
                anyhow::ensure!(
                    (1..=6).contains(&depth),
                    "--depth must be 1 to 6"
                );
            }
            "--metric" => {
                let value = args.next().context("--metric needs a value")?;
                options.metric = match value.as_str() {
                    "files" => disktree_core::tree::Metric::Files,
                    "bytes" | "size" => disktree_core::tree::Metric::Bytes,
                    other => anyhow::bail!(
                        "unknown metric {other}; try bytes or files"
                    ),
                };
            }
            other if other.starts_with('-') => {
                anyhow::bail!("unknown option {other}\n\n{USAGE}");
            }
            path => {
                anyhow::ensure!(root.is_none(), "only one path can be scanned");
                root = Some(PathBuf::from(path));
            }
        }
    }

    anyhow::ensure!(
        !(disk && root.is_some()),
        "--disk and a PATH cannot be combined"
    );
    let home = disktree_core::paths::home_dir();
    let root = match root {
        // `/` on Unix; on Windows the top of the current drive.
        _ if disk => home
            .as_deref()
            .and_then(disktree_core::space::volume_root_for)
            .unwrap_or_else(|| PathBuf::from(std::path::MAIN_SEPARATOR_STR)),
        Some(root) => root,
        None => home.context("no path given and no home directory is set")?,
    };
    // Store the depth as the initial view setting rather than a scan option: it
    // is a display choice the run-time `[` and `]` keys also change.
    // Canonical, so a later widening recognises this tree in the wider walk.
    let root = disktree_core::paths::canonical(&root);
    let metadata = std::fs::metadata(&root)
        .with_context(|| format!("cannot read {}", root.display()))?;
    anyhow::ensure!(metadata.is_dir(), "{} is not a directory", root.display());

    Ok(Args {
        root,
        options,
        depth: depth.clamp(1, 6),
    })
}
