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
to other plugins: an Unreal Engine adapter (objects, names, reflection, the game tick), an
Enfusion adapter for DayZ, a Direct3D present hook for overlays. **Lua scripting** is a
bridge plugin too, so a game that needs no scripting ships without a runtime.

`.asi` files work both ways: uniloader loads the `.asi` mods of the Ultimate ASI Loader
ecosystem from `plugins/`, and it can itself be loaded as an `.asi`.

## Repository layout

| Crate | What it is |
| --- | --- |
| `crates/uniloader-core` | Platform independent logic: settings, key grammar, hotkeys, dependency ordering, console parsing, config files. Builds and tests on any host. |

## Building

The host is an immutable OS, so everything compiles inside the `build-box` container,
cross-compiling to `x86_64-pc-windows-msvc` (the MSVC target has the structured exception
handling that contains a faulting plugin).

```bash
scripts/gate.sh          # fmt, clippy, tests, docs, Windows clippy and release build
scripts/gate.sh --host   # host steps only
```

## License

MIT.
