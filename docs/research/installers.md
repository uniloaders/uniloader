---
name: installers
description: >-
  How to install uniloader into games for users: MelonLoader.Installer and RaiPal studied from their source (stack, platforms, game discovery, how loaders are described, install tracking, uninstall, Proton DLL overrides), why a RaiPal mod list beats a fork or an own installer, what an install registry should store, and the risks of editing Steam launch options.
created: 2026-10-21T20:15+0200
last_edited: 2026-10-21T20:15+0200
---

# Installers: MelonLoader.Installer and RaiPal (2026-10-18)

Question from the owner: a cross-platform (Linux, Windows, ideally macOS) Rust installer for
uniloader, modelled on the MelonLoader installer, that remembers installs so they can be listed,
updated, repaired and removed; or, preferably, a fork of RaiPal with uniloader support. At the time
uniloader was installed by shell script (tsw-sdk `scripts/deploy.sh`, which moves an existing UE4SS
`dwmapi.dll` aside and chain-loads it).

Both projects were shallow-cloned into a scratch directory on 2026-10-18 and read; nothing was built
or run. File references are into those clones.

**Result:** neither as framed. RaiPal describes mod loaders as JSON entries in downloadable mod
lists, so uniloader needs **a mod list, not a fork**. Small missing features belong upstream or in
uniloader. An own installer only makes sense if macOS or a full undo of every install is a hard
requirement. What was done afterwards is in `tsw-plugins/tasks/done.md` (uniloader section).

## MelonLoader.Installer

[LavaGang/MelonLoader.Installer](https://github.com/LavaGang/MelonLoader.Installer), last commit
2026-09-27.

- C# on .NET 9 with Avalonia, Apache-2.0 (`MelonLoader.Installer.csproj`, `LICENSE.md`). Builds for
  Windows, Linux and macOS, x64 and arm64.
- Finds games through Steam, Epic, GOG and the Meta store on Windows; only Steam on Linux and macOS
  (`GameLaunchers/*.cs`). Steam: `config/libraryfolders.vdf` and each library's `*.acf`
  (`SteamLauncher.cs`). Accepts only Unity games (`UnityPlayer.dll/.so/.dylib` or a `*_Data` folder,
  `GameManager.cs:106,256-279`).
- Installs a release from the GitHub API, or a nightly through nightly.link (`MLManager.cs:135`,
  `GitHubAPI.cs`), unzipped into the game folder (`MLManager.cs:386`). Proxy names: `version`,
  `winmm`, `winhttp`, plus `.so`/`.dylib` bootstraps (`MLManager.cs:15-30`).
- **No install registry:** `games.txt` only lists games added by hand (`PathManager.cs`); whether
  MelonLoader is installed is worked out each time from a proxy file and its version
  (`MLVersion.cs:68`).
- Uninstall deletes `MelonLoader/`, every proxy-named file unless its copyright says Microsoft, a
  fixed list of extra files, optionally the user's folders (`MLManager.cs:195-260`). No backup or
  restore of a replaced DLL.
- Proton: it sets nothing; it shows `WINEDLLOVERRIDES="<proxy>=n,b" %command%` to copy
  (`Views/DetailsView.axaml.cs:59`).

## RaiPal

[Raicuparta/rai-pal](https://github.com/Raicuparta/rai-pal), v0.24.0 released 2026-10-17, last
commit on the default branch 2026-10-06: very active.

- Tauri v2. Rust backend `backend/core` (about 10 700 lines) and `backend/tauri-app`; React and
  Mantine frontend. **GPL-3.0-or-later**: a fork stays GPL (uniloader itself, a separate program,
  stays MIT).
- Platforms: Windows (NSIS) and Linux (AppImage) (`Tauri.toml` targets); no macOS paths anywhere.
- Game discovery (`core/src/game_providers/`): Steam (via `steamlocate` plus its own appinfo and VDF
  readers, including non-Steam shortcuts), Epic, GOG, Heroic, itch, Xbox, manual
  (`GameProviderId`, `game_provider.rs:26`).
- Engine and architecture: `game_engines/{unity,unreal,godot,gamemaker}.rs`; Unreal from `+UE4`
  strings in the exe, the build version file or a `*-Shipping.exe`; architecture from the PE or ELF
  header (`architecture.rs`).
- **Loaders are not code.** BepInEx, UE4SS, UEVR and the rest are entries in a JSON mod list,
  described by `GameMod`, `ModInstall`, `ModRun`, `ModDownload` (`core/src/mods/game_mod.rs:28-90`).
  An install is a list of extract actions (zip → destination), write actions (a file with given
  contents), `wineDllOverrides` and a `manifestPath`. Paths use placeholders such as
  `{{GameExecutableFolderPath}}`, `{{GameInstalledModsPath}}`, `{{MaybeWineRoot}}`
  (`replacement_token.rs:13`).
- Mod lists come from the default database
  (`https://raicuparta.github.io/rai-pal-db/mod-db/3/mods.json`), any URL (`url_mod_provider.rs`,
  or the link `rai-pal://add-mod-source?url=…`), or a local folder (`folder_mod_provider.rs`).
- **Install tracking:** writing a copy of the mod's JSON to its `manifestPath` marks it installed.
  **Uninstall deletes every extract and write destination, recursively for folders**
  (`installed_mod.rs:41`): an action pointing at a whole `plugins/` folder would delete the user's
  own plugins. Nothing is backed up, and the Wine override is never removed.
- **Proton:** writes `"dwmapi"="native,builtin"` into `steamapps/compatdata/<appid>/pfx/user.reg`
  after a backup to `user.reg.bak` (`wine.rs:145`, `steam/steam_proton.rs:67`); it only reads
  `localconfig.vdf`, never writes launch options (`steam_shortcut.rs:196`). The prefix must exist,
  i.e. the game must have run once.
- RaiPal's UE4SS entry already uses `dwmapi.dll`, keeps its payload in RaiPal's own folder and points
  to it with an `override.txt` next to the exe.

Adding uniloader for Windows games needs no code: one custom mod list with entries such as
`uniloader-dwmapi` and `uniloader-version` (extract the DLL to
`{{GameExecutableFolderPath}}/dwmapi.dll`, one extract per plugin file, `wineDllOverrides:
["dwmapi"]`).

## What a mod list cannot express

1. **Moving an existing DLL aside and restoring it on uninstall** (what `deploy.sh` does with UE4SS's
   `dwmapi.dll`): a `backup` field in `ModInstall`, handled in `game_mod.rs::install` and
   `installed_mod.rs::uninstall`, plus the schema in rai-pal-db's `mod.ts`; roughly 80–120 lines.
   Chain-loading the old DLL could be a write action producing the loader config.
2. A placeholder for the proxy name (optional; separate entries per name work).
3. On uniloader's side: relocating its root, like UE4SS's `override.txt`, so `plugins/` and
   `uniloader/` can live outside the game folder. At the time they were fixed relative to the game
   folder (`crates/uniloader/src/config.rs:36-44`); this was built afterwards as
   `uniloader-root.txt` (README).

## If an own installer is built anyway

- Crates: `steamlocate` 2.x, `keyvalues-parser` (for `localconfig.vdf`), `pelite` (architecture,
  export checks), `directories` (per-OS folders), `reqwest` or `ureq` with `zip` and `sha2`,
  `eframe`/`egui`. RaiPal's `wine.rs` logic and its Epic/GOG providers would have to be rewritten
  (or taken under GPL-3).
- **Install registry:** `installs.toml` under `directories::ProjectDirs`
  (`~/.local/share/uniloader/`, `%APPDATA%\uniloader\`, `~/Library/Application Support/uniloader/`),
  plus a marker `<game>/uniloader/install.toml` so a lost registry can be rebuilt by scanning. Per
  install: id, store and app id, title, exe path and folder, proxy name and architecture, uniloader
  version and hash, every installed file with its hash, displaced files with their original hash,
  added config lines, the Wine override method and the previous value, timestamps. Update and repair
  compare hashes; uninstall deletes only recorded files and restores displaced ones.
- **Steam launch options** (`userdata/<account>/config/localconfig.vdf`,
  `…/apps/<appid>/LaunchOptions`): Steam must be fully closed (it keeps the file in memory and
  overwrites it on exit); one copy per Steam account; the case of `Software/Valve` varies; existing
  options, including another `WINEDLLOVERRIDES`, must be merged and quotes escaped; back up first.
  Prefer RaiPal's `user.reg` route (Steam may stay open, only the game must be closed); use launch
  options only when the prefix does not exist yet, or show the text to copy.

## Open

- Whether RaiPal upstream would take a `backup` field (an outward-facing request; the owner's call,
  see `tsw-plugins/tasks/done.md`).
