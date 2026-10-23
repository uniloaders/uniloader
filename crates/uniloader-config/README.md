# uniloader-config

A window for editing a game's uniloader configuration, for Windows and Linux (macOS should
build too, untested): plugins on or off, every plugin's settings with a control for its type,
the keys plugins suggest, your own binds and aliases, and `loader.toml`.

## Finding the config

Started without an argument, it looks next to itself: in the game's folder (where
`uniloader/config/` or a `plugins/` folder with a plugin is), in the loader's `uniloader`
folder, or in `uniloader/config` itself, and up to two folders above. A
`uniloader-root.txt` that moves the loader's root elsewhere is followed (under Linux its
`Z:\` path is the host's `/`). Given a path as its argument (any of those folders, or a file
in them), it opens that. Otherwise it asks, with the system's folder or file picker; the
last ten locations are listed there (`recent.toml` in the platform's config directory).

## What the settings are

Plugins register their settings, keys and commands while they start, and the plugins are
Windows DLLs that only the game can load. So the loader writes what each plugin registered to
`uniloader/config/schema/<plugin>.json` whenever it starts one (`uniloader_core::schema`),
and `schema/loader.json` with the game's executable name. The editor reads those:

- the setting's type gives the control (switch, slider within the range, list of choices,
  checkboxes for flags, colour picker, key recorder, file picker, text with the format for
  dates and times), with its description, default, Reset (removes the stored value) and the
  loader's own validation (`uniloader_core::settings::validate`) before anything is stored;
- a secret is never shown, only `<set>` or `<unset>`; a new value can be typed (masked) or
  the value cleared. One the loader keeps as a hash (the RCON password) is stored as the same
  Argon2id hash;
- stored values no setting describes, and every value of a plugin that never ran with this
  loader, are edited as plain text.

The values stay in `config/<plugin>.toml`; edits keep comments and layout (`toml_edit`).
Keys and binds use the loader's own bind table and key grammar (`uniloader_core::binds`,
`keys`), so `binds.cfg` is written exactly as the loader writes it, and a script that would
put a secret into it is refused, as the loader refuses it.

## Saving

Changed files are replaced atomically (a temporary file, then a rename). A file someone else
changed since it was read is not overwritten without asking. While the game runs (its
executable from `loader.json` is among the processes, also under Proton), the editor says so:
the loader reads the files only at start and rewrites `binds.cfg` and a plugin's settings file
when they change in game, so close the game first; changes apply at its next start.

## Building

`scripts/gate.sh` builds it with the workspace: `target/x86_64-pc-windows-msvc/release/
uniloader-config.exe` (Windows) and `target/release/uniloader-config` (Linux), one file each.

## Why egui

egui (through eframe) is what the in-game overlay already draws with, so both settings
editors share one toolkit and one version, and it is pure Rust: the Windows build
cross-compiles with `cargo xwin` like the rest of the workspace, and the result is a single
executable with no runtime to install. Only the OpenGL backend is built (wgpu would make the
binary several times larger for a form), and `rfd` gives the native file dialogs, through the
XDG desktop portal on Linux so no GTK is needed. Accessibility (AccessKit) is left out for
now, since on Linux it pulls in a D-Bus stack.
