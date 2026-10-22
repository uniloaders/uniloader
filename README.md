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
- **Binds** in a readable grammar (`ctrl+shift+r`, `sc29`, `mouse5`). Plugins suggest
  default keys; the user binds any key to console script, moves or removes a plugin's
  suggestion, and that is saved. They only fire while the game window has focus.
- **A console** in the Counter-Strike style: built-in commands plus the commands and
  variables plugins register, `;` between commands, `"..."` quoting, aliases, `+`/`-` hold
  binds, cfg files and `wait`. See [Console and binds](#console-and-binds).
- **Dependencies** and load order, with version requirements.
- **Metadata and targets**: authors with links, home page, issue tracker, download page and
  where to look for updates; and which game the plugin is for (see [Targets](#targets)).
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
its `overlay` service. It draws on Direct3D 11 and 12 (through DXGI), OpenGL and Vulkan; the
details per API are in its [README](plugins/overlay/README.md).

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
    config/<plugin>.toml, binds.cfg      written by the loader
    config/autoexec.cfg, <name>.cfg      yours: run at start / by `exec <name>`
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
game_keys = ["f1", "ctrl+f12"]  # keys the game uses: binds on them work, and are pointed out
```

Command line: `--loader-console` opens a console window, `--noplugins` loads nothing,
`--loader-log=debug` overrides the level.

## Console and binds

The console window, the overlay's console, cfg files, binds, aliases and a plugin's
`console_exec` all run the same script language, the one Counter-Strike players know:
commands separated by `;` or newlines, words by spaces, `"..."` groups words (inside quotes
`\"` is a quote and `\\` a backslash; other backslashes are literal, so `"C:\Games"` works),
`//` starts a comment. `help` lists the built-ins.

```text
bind A "exec plugin.command arg1 arg2"           // exec: a cfg file, or else that command
bind CTRL+F9 "set plugin.setting value; reload"  // two commands on one key
unbind f9                                        // removes a plugin's suggested key too
alias +fast "set uniloader.speed 4"              // + runs on press, - on release
alias -fast "reset uniloader.speed"
bind mouse5 +fast
bind add "incrementvar uniloader.speed 0.1 4 0.1"
binds                                            // every bind and where it comes from
binds reset all                                  // the plugins' suggestions back
rebind tsw-coop.menu ctrl+f10                    // move a plugin's suggestion
```

Plugins suggest keys (`host.hotkey`, `host.bind_default`); when two suggest the same key, the
first keeps it and the other is reported as shadowed. Everything the user changes is saved to
`config/binds.cfg`, keyed by the plugin's entry name for moved or removed suggestions, so a
removal survives a plugin changing its default. `config/autoexec.cfg`, if present, runs once
after every plugin has started; `exec name` runs `config/name.cfg`.

One command buffer runs everything in order: a bind and a typed command never interleave, and
`wait [ms]` runs the rest of a script a tick (16 ms) or `ms` later.

A setting a plugin marks secret (`Setting::secret()`) is shown as `<set>` or `<unset>`
everywhere, a command that sets one is echoed and logged as `set name <redacted>`, and a bind
or alias may not hold one.

### Remote console (RCON)

The loader speaks Source RCON, so `mcrcon`, `rcon-cli`, ARRCON or Python's `rcon` can run
console scripts in the game as if typed there. Whoever has the password can do anything the
console can, plugins' commands included, so it is off by default:

```text
set uniloader.rcon_password "at least twelve characters"   // stored as an Argon2id hash
set uniloader.rcon true                                     // listens on 127.0.0.1:48622
```

`uniloader.rcon_bind` (`127.0.0.1`; `0.0.0.0` for the network), `uniloader.rcon_port`
(48622) and `uniloader.rcon_allow` (addresses or networks, `192.168.2.0/24, 100.64.0.0/10`)
restart the server when changed. RCON itself cannot change these settings, nor bind or alias
anything that mentions them. The password crosses the network in clear text: use a VPN such as
Tailscale, never an open port. Four connections at most; three failed logins from one address
within 10 minutes ban it for 15. Logins and scripts (redacted, without their output) are
written to `logs/rcon.log`.

## Game speed

The loader can run any game faster or slower, the way Cheat Engine's speedhack does: it
scales the clocks games measure frame time with (`QueryPerformanceCounter`,
`GetTickCount64`, `GetTickCount`, and `timeGetTime` once `winmm.dll` is loaded). The wall
clock stays real. Nothing is hooked until the speed first differs from 1, and a change never
makes the clocks jump or run backwards.

The loader lists itself as the plugin `uniloader`, so its settings work like any plugin's,
including the overlay's settings editor and `uniloader/config/uniloader.toml`:

| Name | What |
| --- | --- |
| `uniloader.speed` | Setting, 0.1 to 4, default 1. `uniloader.speed 1.2` in the console sets it. |
| `uniloader.speed_up`, `uniloader.speed_down` | Command and hotkey: a tenth faster or slower. |
| `uniloader.speed_reset` | Command and hotkey: back to 1. |

The hotkeys are unbound by default, because games and plugins have taken most function keys;
bind them in the overlay's settings editor or in the console (`bind ctrl+add
uniloader.speed_up`, or `alias +fast "set uniloader.speed 4"; alias -fast "reset
uniloader.speed"; bind mouse5 +fast` to run fast while a button is held). A saved speed other
than 1 is applied at startup and logged as a warning.

**For plugins:** the speed is the setting `uniloader.speed` (`host.get("uniloader.speed")`),
and every change is published as the event `loader.speed` (`topics::SPEED`, payload a
little-endian `f64`). A plugin that cannot work at another speed, a multiplayer session where
every machine must agree on time, subscribes to it and sets `uniloader.speed` back to 1.

## Targets

A plugin can say which game it is for. Each filter names a property of the running game and
lists the values it accepts; every filter must accept for the plugin to count as meant for
this game:

```rust
const TARGETS: &'static [Target] = &[
    Target::new("exe.name", &["TrainSimWorld.exe"]),
    Target::new("pe.version.CompanyName", &["Dovetail Games"]),
    Target::new("pe.version.ProductName", &["TrainSimWorld5", "TrainSimWorld6", "TrainSimWorld7"]),
];
```

A mismatch never stops the loader. The plugin still starts with a warning in the log, the
event `loader.plugin_mismatch` and a dialog in the overlay, unless it set
`REFUSE_ON_MISMATCH`; then it is listed as not running, with the reason.

`facts` in the console lists every property the loader read from the game, with its value,
and `plugin info NAME` shows what a plugin declared and whether it matched. The properties:
the executable's name, path and folder, the Steam install folder, app ID and build ID; the PE
headers (machine, timestamp, linker version, subsystem, the PDB GUID that identifies one
build exactly); every string of the version resource (`pe.version.ProductName`, custom keys
included, and `pe.version.040904b0.ProductName` per language) and its fixed part
(`pe.fixed.file_version`); the Unreal or Unity engine version; .NET runtime; and on request
`file.sha256`.

Values match by kind. Text is matched ignoring case, with `*` and `?` wildcards, or as a
version after a comparator (`>=2021.3`). Versions accept a prefix (`4.26` accepts `4.26.1`)
or comparisons (`>=4.26 <5`). Numbers are matched as decimal or `0x` hex, with comparisons,
and `&0x20` accepts when those bits are set. A property the loader could not read fails its
filter.

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
| `examples/smoke-host` | A stand-in game for `scripts/smoke.sh`: imports `version.dll`, presents Direct3D 11, OpenGL and Vulkan frames. |

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
also checks that the overlay drew on each graphics API: the host reads its last frame back,
counts the pixels the overlay changed, and saves the frame as `target/smoke/capture.bmp`
(`capture-gl.bmp`, `capture-vk.bmp`).

## License

MIT.
