# Agent notes: uniloader

Rules and context for anyone, human or agent, working in this repository.

## Origin

uniloader replaces the DayZ plugin loader (`../dayz-plugins/dayz-plugin-loader`) and the TSW
loader planned in `../tsw-plugins/tsw-sdk`. Research on 2026-10-18 found no existing loader
that works for any game, takes Rust plugins as first-class citizens and has a real host API
(Ultimate ASI Loader only loads DLLs, Reloaded-II is C#-only, Reloaded-III is a spec, Raidcore
Nexus is Guild Wars 2 only). Code copied from the DayZ loader says so in its module docs,
with the commit it came from. Its `AGENTS.md` explains why things are the way they are; the
load-bearing rules carry over and are repeated below.

## Rules

- **Game-agnostic core.** Nothing game-specific goes into the loader crates. Engine knowledge
  (Unreal, Enfusion, a renderer) is an adapter plugin that offers services to other plugins.
- `unsafe` only in the FFI modules, every block with a `// SAFETY:` comment.
- **Never hold a lock across a call into a plugin.** Collect what to deliver, unlock, then
  deliver.
- **Plugins are never unloaded** (`FreeLibrary` with live hooks, threads and pointers is not
  safe). `plugin stop` silences a plugin; its DLL stays resident.
- **A DLL is only loaded if its bytes contain the describe export name**, so a stray DLL in
  `plugins/` never has its `DllMain` run. `.asi` files are the deliberate exception.
- **`DllMain` does almost nothing**: it runs under the loader lock. Work starts on a thread.
- ABI enums that travel from a plugin to the loader are transparent integers, not Rust enums.

## Build and verification

- Run `scripts/gate.sh`. It runs inside `build-box` and keeps every step's full output in
  `target/gate-logs/`. Never filter build output without keeping the full log.
- Files ≤ 1000 lines (enforced by `crates/uniloader-core/tests/size_limits.rs`), functions ≤
  100 lines (clippy `too_many_lines`). No `unwrap`/`expect` outside tests.
- Lint configuration only in `[workspace.lints]`.

## Conventions

- Imperative commit subjects, one revertable commit per step, trailer
  `Co-Authored-By: <model> <noreply@anthropic.com>`.
- No remote yet: do not create a GitHub repository or push without asking the owner.
- GitHub Actions never run under the owner's account: keep only `workflow_dispatch` live and
  leave automatic triggers commented out with a note saying why.
