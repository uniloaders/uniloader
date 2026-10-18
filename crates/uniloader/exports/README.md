# Proxy export lists

One file per system DLL the loader can stand in for: `<dll>.txt`, one export name per line,
`#` comments. `build.rs` exports the union of all files from `uniloader.dll`, each as a naked
jump through one table slot; at load time the slots are filled from the system DLL named like
the loader's own file. Copying `uniloader.dll` to `dxgi.dll` makes it a DXGI proxy, to
`version.dll` a version proxy, and so on. Any other name of a system DLL whose exports are all
in the union works too (`xinput1_3.dll` uses the `xinput1_4` names).

Sources:

- `dxgi`, `d3d10`, `d3d11`, `d3d12`, `dinput8`, `xinput1_4`, `opengl32`, `winhttp`:
  [techiew/UltimateProxyDLL](https://github.com/techiew/UltimateProxyDLL) (public domain);
  [Bluscream/UniversalProxyDLL](https://github.com/Bluscream/UniversalProxyDLL) has the same
  lists for these.
- `dwmapi`: the 44 named exports of the UE4SS proxy, as tsw-coop shipped them.
- `version`, `winmm`: Wine's `dlls/<dll>/<dll>.spec`.

Deliberately not here:

- **Game-shipped libraries** (`steam_api64`, `bink2w64`): there is no system copy to forward
  to. They need a "load the renamed original from the game directory" mode first; the fork
  above has their lists.
- **`vcruntime140*`, `msvcp140*`**: exporting `memcpy`, `memset` and friends as jump stubs would
  replace the loader's own C runtime, and `msvcp140` names are C++-mangled, not identifiers.
- **Ordinal-only exports**: not forwarded yet. No game seen so far imports one.
