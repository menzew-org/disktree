# disktree

![disktree: a home directory as a treemap, coloured by kind of data, with reclaimable space hatched and the selection, findings and free space in the side panel](assets/screenshot.png)

Find what is filling a disk, mark what should go, and remove it — with the
volume's free space in view the whole time.

disktree is a treemap for Omarchy. It scans your home directory by default,
draws every directory as a nested mosaic sized by what it really costs on disk,
and lets you walk into it with the keyboard or the mouse. Mark as much as you
like; nothing happens until you review the list and commit, and the permanent
path always asks first.

Built with [GPUI](https://gpui-kit.com/) through
[gpui-omarchy](https://github.com/huacnlee/gpui-omarchy), so it follows your
Omarchy theme and behaves like the rest of the desktop.

This is a fork of [tobi/disktree](https://github.com/tobi/disktree). It adds:

- **Windows.** The same app on Windows 10 and 11, with the Recycle Bin, and
  run as administrator, whole-drive scans read straight from the NTFS file
  table — see [On Windows](#on-windows).
- **An instant start.** The last scan is on screen at once while a fresh one
  runs.
- **A legend you can click** to see only one kind of data or one age, a
  **Kind** that names what a file is, and the **age** of a folder's bytes.

## Install

Everything is on the
[latest release](https://github.com/menzew-org/disktree/releases/latest),
with a SHA-256 checksum beside each file.

### Windows 10 and 11

Download `disktree-<version>-x86_64-windows.msi` and open it. It installs
for your account only, with no administrator prompt: the program goes to
`%LOCALAPPDATA%\Programs\disktree`, a **disktree** shortcut appears in the
Start menu, and `disktree` works in a new terminal. Remove it from
**Settings → Apps → Installed apps**; installing a newer version replaces
the old one.

Until releases are code-signed, SmartScreen may say *Windows protected your
PC* the first time: choose **More info → Run anyway**. The checksum next to
the download lets you confirm it is the file the release built.

Prefer not to install? `disktree-<version>-x86_64-windows.zip` holds the
same `disktree.exe`; unpack it anywhere and run it.

For the fast whole-drive scan, start it as administrator: right-click the
Start menu shortcut → **Run as administrator**. See
[On Windows](#on-windows).

### Linux (Omarchy and other Wayland or X11 desktops)

Download `disktree-<version>-x86_64-linux.tar.gz`, unpack it, and run
`./install.sh` inside (or just copy `disktree` onto your `PATH`).

### From source

You need Rust 1.97 or newer. On Linux:

```sh
git clone https://github.com/menzew-org/disktree
cd disktree
make install
```

`make install` builds a release binary and puts three things under `~/.local`
(no root needed):

- `~/.local/bin/disktree`
- a desktop entry, so disktree is in the launcher and in a file manager's
  **Open with** for a directory (it adds a handler; it never becomes the
  default)
- an icon

`sudo make install PREFIX=/usr/local` installs system-wide; `make uninstall`
removes exactly what was installed.

It needs a Wayland or X11 session with a GPU that GPUI can drive (Vulkan).

On Windows, install [Rust](https://rustup.rs) and Visual Studio's build
tools (*Desktop development with C++*), then:

```powershell
git clone https://github.com/menzew-org/disktree
cd disktree
cargo build --release   # target\release\disktree.exe
```

## Use

Open **disktree** from the Start menu or your launcher: it scans your home
folder. From a terminal:

```sh
disktree              # scan your home folder
disktree --disk       # the whole disk it is on: / on Linux, C:\ on Windows
disktree ~/src        # or any folder; on Windows, e.g. disktree D:\Projects
disktree --help       # every option: apparent size, skip hidden, …
```

### The screen

- **Top:** the trail from `/`, then what is measured — **Size**, **Files** or
  **Age**, **Hidden files**, **Apparent size**, and the depth drawn. In the
  tree a crumb goes there, and its ▾ lists its siblings, largest first with
  their share and size, to jump sideways (arrows and Enter work too). Above
  the scanned root a crumb is dimmer, and clicking it widens the scan to
  there (see below).
- **Under it:** the scan totals, the filter when one is typed, and the legend.
  Every legend entry is a switch: click **Code**, **Cache** or
  **Reclaimable** — or, in **Age** mode, **Older** — and the mosaic shows
  only that, at its true size, where it lives, with how much of it is in
  the directory on screen. It stays on as you go in and out; click it again
  or press `esc` to see everything. Categories are judged file by file, so
  **Code** leaves out the `node_modules` inside a checkout.
- **Mosaic:** colour is the *kind* of data — code, agent scratch,
  toolchains, synced files, git, media, documents, caches — at one muted
  level, lighter with depth. A diagonal hatch is space that can be had back
  (caches, sync history, package stores, build output), independent of
  colour. Top-level directories carry a strip of their colour and a name
  band; deeper open directories a slim label row. In **Age** mode colour is
  the last write instead, from this week to older.
- **Panel:** the selection — its size set large, share of the scan, files,
  and its kind: for a file, what it is (*Java archive*, not the colour of
  the folder it sits in); for a checkout, what git says (changes, stashes,
  unpushed commits). A file shows its last write; a folder its newest
  write and how its bytes spread over the age bands — *70 % last written
  over a year ago · oldest 4 years ago* — since a big folder's newest write
  is nearly always today. Then *Worth a look*, the largest things that
  could plausibly go; what is marked; and the disk, free now and after the
  marks, with the way to the review screen. Drag its left edge to resize
  it; double-click the edge to reset. Hovering a tile shows its size, age
  and, for a file, what it is.

One colour is kept apart: amber marks the selection, the main action, and
what can be had back.

The kinds come from directory names and a few shapes (a bare git repository,
`target` beside a `Cargo.toml`). Some of the names are specific to one
machine; see `crates/disktree-core/src/classify.rs`.

### Marking

Space, X, Enter and the arrows act on the tile under the mouse if the mouse
moved last, and on the keyboard selection after you use an arrow or Tab.

A marked tile takes the danger colour, and so does everything inside it:
removing a directory takes its contents with it. Marking a directory absorbs
any marks already inside it, and something inside a marked directory cannot be
marked or kept on its own; its panel offers to unmark the directory instead.
Marking is reversible — press it again — and the saving is never counted twice.

### Zooming and going in

Scroll to magnify toward the pointer. The wheel magnifies until the directory
under the pointer fills the view, and the next notch goes into it — one
continuous motion, with the directory's contents growing into place. Scroll the
other way to come back out. Enter goes into the selected directory at any
depth, and Backspace or Escape goes up one level. `+` and `-` magnify without
going in; `0` resets.

### Removing

`c` (or **Review…**) opens the list of everything marked. Unmark anything
there, then choose:

- **Move to trash** — the default when a trash is available (`trash-put` from
  trash-cli, then `gio trash`, then a built-in XDG trash). Recoverable until
  the trash is emptied, so it commits directly.
- **Delete permanently** — `rm -rf` semantics. It always asks first, in a dialog
  that names what goes and how much comes back.

When it finishes, disktree scans again so the numbers on screen match the disk,
and shows how much free space was actually gained.

## Keys

| key | does |
| --- | --- |
| `space` / `x` | mark or unmark the tile you point at |
| `ctrl`-click | mark without moving the selection |
| `enter` | open that directory, at any depth |
| `⌫` / `esc` | go up one directory |
| `←` `↑` `↓` `→` | move between tiles at this level |
| `tab` | next largest sibling |
| scroll | zoom toward a directory, then go into it |
| `shift`-scroll | pan the magnified view |
| `[` `]` | draw fewer or more levels at once |
| `-` `=` `0` | magnify, shrink, reset the view |
| `ctrl =` `ctrl -` `ctrl 0` | interface zoom |
| `/` | filter by name: only matches keep their colour; `enter` shows only them, `esc` clears |
| `c` | review the marked list |
| `t` | rank by size or by file count |
| `d` | disk usage or apparent size |
| `i` | include or skip hidden entries |
| `r` | scan again |
| `g` | the whole disk |
| `p` | show or hide the selection line |
| `?` | every key |
| `q` | quit |

On the review screen: `m` trash, `p` permanent, `!` unmark all, `enter`
commits, `esc` goes back.

## What it measures

- **Disk usage** by default: `st_blocks × 512`, the number `du` reports and the
  space that actually comes back when a file is deleted. Apparent size (what
  `ls -l` shows) is one toggle away.
- **Hardlinks once.** Two names for one inode cost one file.
- **Hidden entries included**, because `~/.cache` is often the biggest thing in
  a home directory. Symlinks are not followed.
- **The last scan first.** Every finished scan is kept (in `~/.cache/disktree`,
  or `%LOCALAPPDATA%\disktree\cache` on Windows), so the next start shows
  it at once — the status bar says how old it is — while a fresh scan
  runs and replaces it, keeping your place. Removal waits for the fresh
  one: it promises what comes back, so it will not act on old sizes.

The scan follows [dust](https://github.com/bootandy/dust)'s approach: one rayon
scope per root, a completion counter per directory so no directory is built
before its last subdirectory lands, and one bottom-up pass that aggregates sizes
and removes duplicate hardlinks.

## The whole disk

Click `/` (or any directory above the scanned root) in the trail, press
`g`, run `disktree --disk`, or use the launcher's *Scan the whole disk*
action. `g` and `--disk` scan the disk your home directory lives on — `/`
on Omarchy.

Widening is memoized: the tree already measured is handed to the wider walk
and reused where it is reached, so going from `~` to `/` reads only what is
outside `~` (on this machine, seconds instead of a full rescan). The current
view stays on screen until the wider tree lands, which then opens with the
directory you came from selected. Going back down is just navigation.

A scan stays on one volume, and a volume is the mount *source*, not the
device number: btrfs gives each subvolume its own `st_dev`, so `/home`,
`/var/log` and `/var/cache/pacman/pkg` are included, while `/proc`,
`/sys`, `/run`, tmpfs, `/boot`, other disks, network shares and automount
points are left out (checked by path, so an automounted NAS is never
mounted just to be measured). Snapshot subvolumes are left out too: their
files share blocks with the live ones, and counting them would count the disk
twice. `-X` crosses into everything.

Without root, some system directories cannot be read; they are counted as
unreadable in the top bar rather than guessed at.

## What it refuses to do

The removal rules live in `crates/disktree-core/src/removal.rs`, and each one is
tested:

- only paths under the scanned root can be removed;
- the filesystem root, the scanned root and your home directory are refused;
- a mount point is refused, since removing it would reach into another
  filesystem;
- system trees (`/usr`, `/etc`, `/boot`, `/var/lib`, `/nix/store`, …) are
  refused even where permissions would allow it: packages own them, and
  pacman, paccache or `journalctl --vacuum` are the tools;
- a symlink is unlinked, never followed;
- nothing is passed through a shell — a file called `-rf` is just a file.

## On Hyprland

Hyprland tiles new windows, so disktree opens into whatever tile it is given.
It is designed for a roomy window; float it, or give it a rule:

```
windowrule = float, class:^(disktree)$
windowrule = size 1400 900, class:^(disktree)$
```

## On Windows

disktree runs on Windows 10 and 11; see [Install](#windows-10-and-11).
Everything above applies, and so do the keys. What is different there:

- **The home directory** is your profile, `C:\Users\you`. `--disk` and `g`
  scan the drive it is on, `C:\`.
- **Move to trash** uses the Recycle Bin, the same one File Explorer uses.
- **Run it as administrator for fast scans.** A whole drive, or your home
  folder, is then read straight from the NTFS file table (`$MFT`) rather
  than walked folder by folder — on a 500 GB drive with two million files,
  14 seconds instead of two minutes. The status bar says *from the file
  table*; without the rights it says *run as administrator to scan
  faster* and walks as before. Folders below your home are small enough
  that walking them is quicker, so they are always walked. `--walk` turns
  the file table off.
- **Sizes** from the file table are what each file really occupies —
  compressed, sparse and OneDrive-placeholder files count at what they
  cost — and hardlinks are charged once. A walk cannot learn either
  without opening every file, so it counts the apparent length and every
  hardlink in full: Windows itself hardlinks `C:\Windows\WinSxS` into
  `System32`, so a walked whole-drive scan can total more than the drive
  holds. Your profile is not affected.
- **NTFS's own files** — `$MFT`, `$LogFile`, `$Extend` — appear at the top
  of a whole-drive scan read from the file table, because they are real
  space. Like everything at the top of a drive whose name starts with `$`,
  they cannot be removed.
- **Junctions and links are not followed**, like symlinks elsewhere. That
  also keeps a scan off volumes mounted in a folder.
- **Refused:** `C:\Windows`, `Program Files` and `Program Files (x86)`, and
  what Windows keeps at the top of every drive — `$Recycle.Bin`,
  `System Volume Information`, `Recovery` and the page, swap and
  hibernation files. Matching ignores case, as Windows does.
- **The theme** is gpui-omarchy's default; there is no Omarchy theme to
  follow.

## Develop

```sh
make run      # release build, scanning $HOME
make lint     # rustfmt --check, then clippy with every warning an error
make test     # scanner, layout and removal tests, plus window-harness tests
make ci       # lint, then test
```

On Windows, where there is no `make`, the same gates run through Cargo:
`cargo xtask lint`, `cargo xtask test`, `cargo xtask ci`. `cargo xtask icon`
renders `assets/disktree.svg` into the `.ico` built into `disktree.exe`.

The lint gate is strict on purpose: `clippy::all` and `clippy::pedantic` are
errors, and every exception is written down with its reason in `Cargo.toml`.
The window-harness tests draw real frames and press real keys — including one
that marks a directory, confirms the deletion and checks that the files are
gone while their neighbours are not.

| path | what lives there |
| --- | --- |
| `crates/disktree-core` | scanning, the tree, the squarified layout, free space and removal — no UI |
| `crates/disktree-app/src/state.rs` | every action the interface can take, and the key map |
| `crates/disktree-app/src/views.rs` | the screens |
| `crates/disktree-app/src/treemap_view.rs` | painting the mosaic and its labels |
| `crates/disktree-app/src/ui.rs` | the spacing, type and size scale, in `rem` |
| `crates/disktree-app/src/tests.rs` | end-to-end tests through a real window |
| `crates/disktree-core/src/mft.rs` | reading an NTFS drive from its file table |
| `packaging/`, `assets/`, `Makefile` | the desktop entry, the Windows installer (`packaging/windows`), the icons, and install |

The interface follows the
[GPUI Kit design guides](https://gpui-kit.com/versions/main/docs/design-guides/):
every size is on one `rem` scale so interface zoom keeps its proportions,
primary is reserved for what Enter does, and the only question the app asks is
the one it cannot take back.

## License

MIT
