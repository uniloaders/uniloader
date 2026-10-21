---
name: dayz-plugin-loader
description: >-
  How the DayZ plugin ecosystem that uniloader grew out of was built: the dxgi.dll proxy loader, plugin discovery and load order, the C ABI (PluginInfo, HostApi, PluginCallbacks), fault isolation, the overlay, repo and docs layout, and the lessons its AGENTS.md records.
created: 2026-10-21T20:15+0200
last_edited: 2026-10-21T20:15+0200
---

# The DayZ plugin loader (2026-10-18)

Question: how is the owner's DayZ plugin ecosystem (`/run/media/system/Data/Projects/dayz-plugins/`)
organised, as a model for the TSW plugin SDK and later for uniloader? Read-only study of the repos
on 2026-10-18, done for the tsw-sdk concept. Paths are relative to `dayz-plugins/` and line numbers
are as of that day. uniloader's core was later copied from this loader; `AGENTS.md` in this repo
lists the rules that carried over.

## The loader

- **Binary:** Rust, shipped as `dxgi.dll`, a proxy forwarding every DXGI call to the system library
  (`dayz-plugin-loader/README.md:3-6`). Target `x86_64-pc-windows-msvc`, because MSVC structured
  exception handling is what contains a faulting plugin (`AGENTS.md:47-51`). Under Wine:
  `WINEDLLOVERRIDES="dxgi=n,b"` (`README.md:64`).
- **Little in `DllMain`:** it runs under the loader lock, so initialisation waits for the first DXGI
  call (`AGENTS.md:84`).
- **Discovery:** every `DayZ/plugins/*.dll`, alphabetically (`README.md:86-98`;
  `crates/dayz-plugin-loader/src/win/plugins.rs:186-207`). A DLL is loaded only if its bytes contain
  the export name `dayz_plugin_describe`, so unrelated DLLs never have their `DllMain` run
  (`README.md:100-103`, `plugins.rs:172`).
- **No manifest file:** the describe export returns `PluginInfo` (`crates/dayz-plugin-api/src/types.rs:161-176`):
  `struct_size`, `api_version`, `name` (`[a-z0-9_-]+`, also the config namespace), `version`,
  `description`, `dependencies`, `dependency_count`.
- **Dependencies** (`types.rs:144-156`): `struct_size`, `kind` (Plugin, Library, File, Symbol),
  `name`, `version`, `optional`. SDK form: `Dependency::plugin("dayz-vr", ">=0.2, <1")`,
  `::library("openxr_loader.dll")`, `::file(…)`, `::symbol("render.prepare_view")`, `.optional()`.
  Versions are comma-separated comparators; a bare version means `>=`; components compare
  numerically (`1.10` > `1.9`) (`README.md:137-151`).
- **Game-build compatibility** is not a plugin field. Plugins ask for symbols by name; the separate
  `dayz-data` repo resolves them: `patterns.json` (byte signatures, the source of truth) and
  `builds/<ver>.json` (a cache keyed by sha256, or PE timestamp plus image size, with a byte check
  per address) (`dayz-data/README.md:11-30`).
- **Load order:** describe every DLL, then sort by the dependency graph (`plugins.rs:383-433`).
  Plugins in a cycle stay unloaded; a plugin missing a plugin dependency waits and starts once it
  appears.

## The ABI

- `API_VERSION = 9` (`crates/dayz-plugin-api/src/lib.rs:55`, changelog of 2–9 at `:40-54`; the
  README still said 6). An exact match is required (`plugins.rs:297-299`). Every growable struct
  starts with `struct_size` and only ever appends fields; a too-small callback table is rejected
  (`plugins.rs:368`).
- Exports (`lib.rs:57-72`):

  ```rust
  pub type DescribeFn = unsafe extern "C" fn() -> *const PluginInfo;   // "dayz_plugin_describe"
  pub type StartFn = unsafe extern "C" fn(*const HostApi, PluginHandle, *mut PluginCallbacks) -> Status; // "dayz_plugin_start"
  pub type StopFn = unsafe extern "C" fn(*mut c_void, StopReason);   // "dayz_plugin_stop"
  ```

- **Plugin → loader:** `PluginCallbacks` (`callbacks.rs:49-140`), all optional: swapchain, present,
  resize, hotkey, setting changed, command, message, event, enable/disable, UI, dialog, input
  (returns an `InputResponse`), game event, chat, RPC.
- **Loader → plugin:** `HostApi` (`host.rs:25-376`), a function table: context (game and config
  dirs, command line, args, env, data build), log, settings, hotkey and command registration,
  console print/exec/capture, inter-plugin calls (`plugin_find`, synchronous `plugin_message`,
  `event_subscribe` / `event_publish`), addresses (`symbol_get`, `offset_get`, `symbol_require`),
  hooks (`hook_patch`, `hook_vtable`, `hook_detour`, `hook_remove`), UI (panels, widgets, notices,
  dialogs), input (listen, send, key state, register HID), game streams and chat.
- Conventions: borrowed UTF-8 `Str` slices, every call returns a `Status`, nothing unwinds across
  the boundary (`lib.rs:8-13`). Values that travel from plugin to loader are transparent integers,
  not enums, so an unknown value from a newer plugin cannot cause undefined behaviour
  (`README.md:366-371`).
- SDK: `trait Plugin` (`NAME`, `VERSION`, `DESCRIPTION`, `DEPENDENCIES`, `start`) and the
  `export_plugin!` macro (`dayz-plugin-sdk/src/plugin.rs:48-78`, `src/lib.rs:61-92`). Other
  languages only need the exports and the structs.
- Hooks are owned by the loader and undone in reverse order when a plugin stops or faults
  (`README.md:330-349`).

## Lifecycle, isolation, config, UI

- Console commands `plugin list|deps|load|stop|disable|enable`; stop reasons `Unload`, `Exit`,
  `StartFailed` (`README.md:158-184`).
- **No hot reload, deliberately:** DLLs are never `FreeLibrary`d and a name loads once per launch
  (`README.md:170-173`).
- `config/loader.toml` (`enabled`, `disabled_plugins`, `log_level`); command line `--noplugins`,
  `--loader-log=` (`README.md:76-82`). Per-plugin `plugin-loader/config/<plugin>.toml`, plus
  `hotkeys.toml` and `windows.toml`. Values are stored as text and validated against the plugin's
  setting descriptor.
- **Isolation:** `guard::call` wraps each call in `catch_unwind` plus `microseh::try_seh`
  (`win/guard.rs:6-7, 49, 67`); a fault disables the plugin (`plugins.rs:117`); release builds use
  `panic = "unwind"`. A mingw build would catch only panics, not hardware faults.
- **UI:** an egui overlay on Direct3D 11; the console on the key below Escape, F11 opens a settings
  editor with one section per plugin, stopped ones marked (`win/ui/settings_panel.rs`).

## Repos and docs

- One repo per plugin (loader, `dayz-data`, log, debug, patches, vr; dinput, enforcer and mcp were
  empty folders). No published SDK crate: each workspace points at a sibling path. No tags, no
  release naming; `scripts/build.sh` builds and can deploy.
- **No CI at all**: no `.github/` in any repo or in history. The policy is text only: GitHub Actions
  never run under the owner's account, keep `workflow_dispatch` live, comment automatic triggers
  out with a note. The local gate (`scripts/build.sh` in build-box) runs fmt, clippy, tests, docs,
  Windows clippy and the release build.
- Docs site `dayz-plugins.github.io`: plain Markdown with front matter (`name`, `description`,
  `game_build`, `created`, `last_edited`), `research/` (claims tagged verified, decompiled, or from
  the shipped scripts; RVAs; the game build at the top) and `design/` (status proposal, accepted,
  implemented, superseded). API reference stays in the loader README and rustdoc; code repos have
  no `docs/` folder by rule.

## Lessons recorded (`dayz-plugin-loader/AGENTS.md:47-94`)

- Never hold the state lock across a call into a plugin; collect notifications and deliver after
  unlocking.
- The plugin list is append-only, published through an atomic pointer, so the render thread
  dispatches without a lock.
- `retour` was dropped (its C code does not cross-compile under clang-cl) for a pure-Rust
  `iced-x86` detour. Detours refuse RIP-relative or branching prologues and do not suspend game
  threads, so hooks belong in `start` or `on_swapchain`.
- `RegisterHotKey` failed under Wayland/Wine; hotkeys are polled from `Present` and fire only while
  the game window has focus.
- Never compile an address into code; a global variable gets no byte check (its on-disk bytes are
  only initial data). `dayz-data` rules: never hand-edit `builds/*.json`, provenance must honestly
  be `verified`, `analysis` or `scan`, never invent addresses.
- Size limits: files under 600 lines (1000 hard, enforced by a test), functions under 50 lines.
- Leftovers from the C++ predecessor (`scripts/*.py`, `scripts/ghidra*`, `tools/config-editor/`)
  are marked as not current.

## What uniloader took from it

The proxy model, describe-export gating, the C ABI shape (struct sizes, append-only, transparent
enums), SEH isolation per plugin, "never unload", "nothing in DllMain", the lock rule, and the
egui overlay. See this repo's `AGENTS.md` and `README.md`.
