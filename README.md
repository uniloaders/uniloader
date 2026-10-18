# uniloader

A plugin loader for any Windows game, written in Rust. It is dropped into the game folder as
one of the system DLLs the game imports (`dxgi.dll`, `dwmapi.dll`, `version.dll`,
`winmm.dll`, `dinput8.dll`, …), forwards every call to the real system library, and hosts
native plugins: ordinary DLLs in `plugins/` that the game knows nothing about.

**Status:** started 2026-10-18. It replaces two single-game loaders by the same owner, the
DayZ plugin loader (`dxgi.dll`) and the Train Sim World 7 loader in `tsw-sdk`
(`dwmapi.dll`); both move onto it.

## What a plugin gets

From the DayZ loader, made game-agnostic:

- **Settings**: typed, validated, persisted per plugin, changeable at run time.
- **Hotkeys** in a readable grammar (`ctrl+shift+r`, `sc29`), with user overrides. They only
  fire while the game window has focus.
- **A console** with built-in commands plus the commands and variables plugins register.
- **Dependencies** and load order, with version requirements.
- **Plugin-to-plugin messaging**: direct messages with replies, broadcast topics.
- **Hooks the loader owns**: byte patches, vtable slots, inline detours, undone when the
  plugin stops.
- **Isolation**: every call into a plugin is guarded against panics and hardware faults. A
  faulting plugin is disabled; the game and the other plugins keep running.

Game knowledge is not in the loader. It comes from **adapter plugins** that publish services
to other plugins: an Unreal Engine adapter (objects, names, reflection, the game tick) and an
Enfusion adapter for DayZ. **Lua scripting** and the **in-game overlay** are bridge plugins
too, so a game that needs neither ships without a runtime or a present hook.

The overlay (`plugins/overlay`) draws a console, a settings editor for every plugin's
settings and hotkeys, and whatever panels, toasts and dialogs other plugins put up through
its `overlay` service. It hooks DXGI universally; the API coverage and the plan for the rest
are in its [README](plugins/overlay/README.md).

`.asi` files work both ways: uniloader loads the `.asi` mods of the Ultimate ASI Loader
ecosystem from `plugins/`, and it can itself be loaded as an `.asi` (under any name that is
not a system DLL's, it only hosts plugins and forwards nothing).

## Installing into a game

```text
<game exe dir>/
  dwmapi.dll            uniloader.dll, renamed to a DLL the game imports
  plugins/
    coop.dll            native plugins
    lua_bridge.dll      the Lua bridge, if wanted
    lua/*.lua           scripts for it
    something.asi       .asi mods
  uniloader/
    config/loader.toml  optional; see below
    config/<plugin>.toml, hotkeys.toml   written by the loader
    logs/loader.log
```

Which name to use: any DLL the game imports from the system directory and whose exports
are listed in `crates/uniloader/exports/` (`dwmapi`, `dxgi`, `d3d10`, `d3d11`, `d3d12`,
`dinput8`, `xinput1_3`/`1_4`, `version`, `winmm`, `winhttp`, `opengl32`). Under Wine or
Proton add a native override, for example `WINEDLLOVERRIDES="dwmapi=n,b"`.

To keep everything but the proxy DLL out of the game's folder (a mod manager's own folder,
as RaiPal does for UE4SS), put `uniloader-root.txt` next to it. Its first line that is
neither empty nor a `#` comment names the root, absolute or relative to the exe's folder.
`plugins/`, `config/` and `logs/` then all live there:

```text
<game exe dir>/dwmapi.dll
<game exe dir>/uniloader-root.txt    Z:\home\me\mods\tsw\uniloader
<that root>/plugins/  config/  logs/
```

`loader.toml`, every key optional:

```toml
enabled = true                  # false: forward only, load nothing
disabled_plugins = ["jump"]     # file stems never loaded
log_level = "info"
asi = true                      # load plugins/*.asi
load = ["ue4ss/UE4SS.dll"]      # chain-load DLLs this one displaced, listed with their version
```

Command line: `--loader-console` opens a console window, `--noplugins` loads nothing,
`--loader-log=debug` overrides the level.

## Repository layout

| Crate | What it is |
| --- | --- |
| `crates/uniloader-api` | The C ABI between loader and plugins. |
| `crates/uniloader-core` | Platform independent logic: settings, key grammar, hotkeys, dependency ordering, console parsing, config files, PE version resources. Builds and tests on any host. |
| `crates/uniloader-sdk` | The safe Rust side of the ABI: implement `Plugin`, call `export_plugin!`. |
| `crates/uniloader` | The loader DLL: proxy, plugin host, host API, console. |
| `crates/uniloader-overlay` | Client for the overlay service: its C ABI and a safe wrapper. |
| `plugins/lua-bridge` | Lua 5.4 scripting as a plugin; its API is documented in `src/api.rs`. |
| `plugins/overlay` | The in-game overlay: console, settings editor, the `overlay` service. |
| `examples/hello-plugin` | The smallest useful plugin. |
| `examples/kitchen-sink` | Uses every host function and the overlay service, and checks the results. |
| `examples/smoke-host` | A stand-in game for `scripts/smoke.sh`: imports `version.dll`, presents Direct3D 11 frames. |

## Building

The host is an immutable OS, so everything compiles inside the `build-box` container,
cross-compiling to `x86_64-pc-windows-msvc` (the MSVC target has the structured exception
handling that contains a faulting plugin).

```bash
scripts/gate.sh          # fmt, clippy, tests, docs, Windows clippy and release build
scripts/gate.sh --host   # host steps only
scripts/smoke.sh         # end to end under Wine with a stand-in game, no real game needed
```

The smoke test runs the stand-in host under GE-Proton's Wine with DXVK, in a private prefix.
It checks forwarding, the plugins, Lua, `.asi` loading and the kitchen sink's self-test. It
also checks that the overlay drew: the host reads its last frame back, counts the pixels the
overlay changed, and saves the frame as `target/smoke/capture.bmp`.

## License

MIT.
