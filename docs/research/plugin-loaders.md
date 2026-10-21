---
name: plugin-loaders
description: >-
  Survey of existing game mod and plugin loaders (Ultimate ASI Loader, Reloaded-II/III, me3, Lovely, Raidcore Nexus, BepInEx, MelonLoader, UE4SS, script extenders, ReShade, Special K, Frida, hudhook, dll-syringe) against uniloader's needs: any game, Rust plugins through a C ABI, a real host API, Proton. Why uniloader was built, which crates to reuse, and which scripting language to embed.
created: 2026-10-21T20:15+0200
last_edited: 2026-10-21T20:15+0200
---

# Existing plugin loaders (2026-10-18)

Question: is there an actively maintained (commits in 2025–2026), game-agnostic native plugin
loader for Windows games that loads Rust `cdylib` plugins, ideally with a scripting layer, and
works under Wine/Proton? At the time the owner had two home-grown Rust loaders (DayZ: a `dxgi.dll`
proxy with a rich C ABI, see [dayz-plugin-loader.md](dayz-plugin-loader.md); TSW 7: a `dwmapi.dll`
proxy with UE 4.26 reflection via patternsleuth) and considered replacing both with one loader,
either adopted or new.

Web research on 2026-10-18. Activity dates are the last push to GitHub (`gh api`), crate versions
from crates.io, both as of that day. Nothing was installed or run.

**Result: nothing existing fits; build one generic Rust loader.** That became uniloader (see
`AGENTS.md`, "Origin").

## Candidates

| Candidate | Language / licence | Last push | Game-agnostic | Rust plugins first-class | API surface | Proton | Maturity |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| [Ultimate ASI Loader](https://github.com/ThirteenAG/Ultimate-ASI-Loader) | C++ / MIT | 2026-10-16 | **yes**, about 30 proxy names (dinput8, dxgi, version, winmm, d3d9–12, xinput …) | any DLL renamed `.asi`; only an exported `InitializeASI` hook | loads plugins, file-override "update" folder, zip packages, crash dumps, hot-reloaded C++ snippets; no settings, hotkeys, dependencies, messaging or isolation | works with `WINEDLLOVERRIDES` (README does not mention Wine) | very high, 2.3k stars |
| [Reloaded-II](https://github.com/Reloaded-Project/Reloaded-II) | C# / GPL-3 | 2026-09-23 | yes | no, mods are .NET; native only through P/Invoke | dependencies, load order, config UI, inter-mod interfaces, hooks | works | high |
| [Reloaded-III](https://github.com/Reloaded-Project/Reloaded-III) | spec (Rust intended) | 2026-03-04 | planned | planned | specification only | n/a | not usable |
| [me3](https://github.com/garyttierney/me3) | Rust / MIT + Apache | 2026-10-14 | no, FromSoftware games | loads native DLLs, no public SDK | profiles, asset override | supported | high |
| [ModEngine2](https://github.com/soulsmods/ModEngine2) | C++ / MIT | archived 2024 | no | | | | superseded by me3 |
| [Lovely](https://github.com/ethangreen-dev/lovely-injector) | Rust / MIT | 2026-09-23 | no, LÖVE games | no, patches Lua source | Lua patches | works | medium |
| [Raidcore Nexus](https://github.com/RaidcoreGG/Nexus) + [nexus-rs](https://github.com/Zerthox/nexus-rs) | C++ / own licence | 2026-09-22 / 2026-07-30 | no, Guild Wars 2 | **yes**, C ABI with Rust bindings | keybinds, events, settings, ImGui, data links, updater | works | high; **the best addon API to copy** |
| [BepInEx](https://github.com/BepInEx/BepInEx) | C# / LGPL-2.1 | 2026-10-16 | Unity/.NET only | no | config, dependencies, Harmony hooks | works | very high |
| [MelonLoader](https://github.com/LavaGang/MelonLoader) | C# / Apache-2 | 2026-10-06 | Unity only | no | like BepInEx | works | high |
| [RE-UE4SS](https://github.com/UE4SS-RE/RE-UE4SS) | C++ / MIT | 2026-10-17 | no, Unreal | C++ mods only | Lua, UE reflection, hotkeys, GUI | works | high; a possible companion for Unreal games |
| SKSE, [RED4ext](https://github.com/wopss/RED4ext) | C++ | 2026-09 / 2026-03 | no, per engine | C++ | engine-specific | works | high |
| [ReShade addons](https://github.com/crosire/reshade) | C++ / BSD-3 | 2026-09-24 | yes, rendering only | C++ interface, not a C ABI | render-API events, ImGui overlay | works with DXVK | very high |
| [Special K](https://github.com/SpecialKO/SpecialK) | C++ / GPL-3 | 2026-10-17 | yes | loads extra DLLs early or late only | mostly its own features | partial | high |
| [Frida](https://github.com/frida/frida) | C / own licence | 2026-10-17 | yes | frida-rust bindings; hooks are JS | interceptor, JS scripting | Gadget under Wine is fiddly | very high; better for research than shipping |
| [hudhook](https://github.com/veeenu/hudhook) | Rust / MIT | 2026-10-16 (0.9.3) | yes | library | overlay only (dear imgui via imgui-rs, not egui), DX9/11/12, OpenGL | works | good |
| [dll-syringe](https://github.com/OpenByteDev/dll-syringe) | Rust / MIT | 2026-07-26 (0.18.2) | yes | library | injection and remote calls | works | good |

Searches on GitHub and crates.io for Rust "mod loader", "dll proxy plugin loader" and "universal
mod loader" found only tiny single-game projects (e.g. minimal-asi-loader, cardamom-loader,
NMS.rs, 0–3 stars each).

## Why none was adopted

Nothing combines "any game", "Rust plugins first-class through a C ABI" and a rich host API. The
DayZ loader already offered more than any game-agnostic option. Ultimate ASI Loader is the only
mature agnostic one, and it is only a loading mechanism. Reloaded-III would be the natural project
to contribute to, but after more than a year it is still a specification.

## Recommended shape (as proposed)

- **Generic core** from the DayZ loader's C ABI (settings, hotkeys, console, dependencies and load
  order, messaging, hook registry, per-plugin SEH isolation, overlay), with everything
  DayZ-specific removed.
- **One proxy shim** that can be named dxgi, dwmapi, dinput8, version or winmm, as UAL does; under
  Proton `WINEDLLOVERRIDES="<name>=n,b"`, and for dxgi chain to DXVK's dxgi.
- **`.asi` both ways:** export `InitializeASI` so UAL users can drop uniloader in as an `.asi`, and
  load existing `.asi` mods. (Built; see uniloader's README.)
- **Game knowledge as adapter plugins** publishing services through the messaging registry, like
  Nexus's data links.
- **Copy Nexus's addon API** (events, keybinds, data links, quick access) as the shape of the C ABI.

Crates (versions of 2026-10-18):

| Need | Crate | Note |
| :--- | :--- | :--- |
| Detours | `retour` 0.4.0-alpha.4 | slow releases, pin it. (The DayZ loader had already dropped it: its C code does not cross-compile under clang-cl.) |
| Mid-function hooks | `ilhook` 2.3 | captures registers |
| Battle-tested C core | `minhook` 0.9 | |
| ABI | hand-written `#[repr(C)]` tables with a version field, or `stabby` 72.1.16 | avoid `abi_stable` (no release since 2023) |
| Pattern scanning | `patternsleuth` | for the Unreal adapter |
| Overlay | `egui` 0.37 | hudhook's present/swapchain hooks are reusable, its UI is imgui |
| Loading | `libloading` 0.9 | |

## Scripting: Lua through `mlua`, as a bridge plugin

- **`mlua`** (0.12.2, active) is the easiest to embed from Rust: vendored Lua 5.1–5.5, LuaJIT and
  Luau, so it cross-compiles to Windows without system dependencies. Lua is what modders know
  (UE4SS, Balatro/Lovely, Garry's Mod, WoW, Factorio). Luau adds sandboxing, LuaJIT speed and FFI.
- Weaker fits: rhai (Rust-native, unknown to modders), rquickjs (fine, JS less common here), pyo3
  (best known, but heavy to bundle, GIL and threading problems inside a game, awkward under Proton).
- **Put scripting in a bridge plugin**, not the core: an ordinary cdylib on the same C ABI. The core
  stays small, games can ship without a script runtime, and a fault in the script layer stays in
  that plugin. (Built as `plugins/lua-bridge`.)

## Open (at the time)

- Reloaded-III's progress; worth rechecking before investing in a parallel spec.
