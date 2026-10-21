---
name: uniloader-research-index
description: >-
  Index of the research behind uniloader: existing plugin loaders, the DayZ loader it grew out of, and installers (MelonLoader.Installer, RaiPal).
created: 2026-10-21T20:15+0200
last_edited: 2026-10-21T20:15+0200
---

# Research notes

Findings that shaped uniloader, one topic per file. Each note says when the research was done, what
the sources were, and which claims are verified or inferred. Game-specific research lives with the
games' plugins (for Train Sim World:
`tsw-plugins/tsw-plugins.github.io/research/`).

| File | Topic |
| :--- | :--- |
| [plugin-loaders.md](plugin-loaders.md) | Existing game plugin loaders against uniloader's needs; why none was adopted, crates to reuse, Lua through `mlua` as a bridge plugin (2026-10-18) |
| [dayz-plugin-loader.md](dayz-plugin-loader.md) | The DayZ loader uniloader grew out of: proxy, discovery and load order, C ABI, isolation, overlay, repo layout, recorded lessons (2026-10-18) |
| [installers.md](installers.md) | MelonLoader.Installer and RaiPal from their source; a RaiPal mod list instead of a fork; install registry and Steam launch options (2026-10-18) |
