# overlay

An in-game overlay for uniloader: a console, a settings editor, and the `overlay` service
other plugins draw their own panels, toasts and dialogs through.

It is a plugin, not part of the loader. Drawing inside a game means hooking how it presents
frames, which is different for every graphics API and risky in each one. A game that does
not want an overlay should not carry one, and anyone who wants a different overlay can
replace this one: consumers only see the service, never egui.

## Using it

Drop `overlay.dll` into `plugins/`. In game:

| Hotkey | Action |
| --- | --- |
| key under Escape, or `ctrl+shift+c` | console |
| `f11` | settings editor |
| `ctrl+shift+m` | give the mouse back to the game with the windows left open, or take it again |

While a window is open, the overlay has the keyboard and mouse, so typing in the console
does not walk the player forward. It also tells the loader, which then fires only the
overlay's own hotkeys. Window positions, sizes and opacity are saved to
`uniloader/config/overlay-windows.toml`.

## Drawing from a plugin

Depend on the plugin and on the client crate:

```toml
[dependencies]
uniloader-overlay = { git = "https://github.com/uniloaders/uniloader" }
```

```rust
const DEPENDENCIES: &'static [Dependency] =
    &[Dependency::plugin(uniloader_overlay::PLUGIN, ">=0.1").optional()];

if let Some(overlay) = uniloader_overlay::Overlay::connect(&host) {
    overlay.panel("status", "My plugin", false, |ui| {
        ui.heading("Status");
        ui.setting("speed"); // the control for a registered setting, written through the loader
        if ui.button("Reset") { /* ... */ }
    })?;
    overlay.toast(UiLevel::SUCCESS, "My plugin", "Ready.")?;
}
```

The panel's closure runs on the render thread once per frame while the panel is open. It
runs under the loader's fault guard: a plugin that panics or faults in it is disabled, and
the overlay drops everything that plugin registered. `examples/kitchen-sink/src/overlay.rs`
uses every part of the service.

From another language, look the service up by name (`overlay`, version 1) and call the
`#[repr(C)]` table [`OverlayApi`](../../crates/uniloader-overlay/src/abi.rs) directly.

## Graphics APIs

| API | State |
| --- | --- |
| Direct3D 11 (native, DXVK) | drawn |
| Direct3D 10 | hooked, not drawn: the device is not a Direct3D 11 one |
| Direct3D 12 (native DXGI) | hooked, not drawn yet |
| Direct3D 12 under vkd3d-proton | not hooked: its swapchain is a different class |
| Vulkan | not hooked yet |
| OpenGL | not hooked yet |

The hook is universal for DXGI. `Present`, `Present1` and `ResizeBuffers` are virtual
functions shared by every swapchain of one implementation. A throwaway swapchain on a hidden
window is enough to find the vtable, and patching it through the loader covers every swapchain
the game makes. This works whether or not uniloader itself is the `dxgi.dll` proxy, and
whether the game made its swapchain before or after the plugin loaded.

The plan for the rest keeps the split that is already there. `ui` builds an egui frame and
knows nothing about devices; `win` owns one renderer. Each new API is one more renderer behind
the same `Gui::run`:

- **Direct3D 12**: the same Present hook, plus the game's command queue (taken from
  `ExecuteCommandLists`) and an `egui` D3D12 renderer.
- **Vulkan**: hook `vkQueuePresentKHR` and `vkCreateSwapchainKHR` via the loader's
  `vkGetDeviceProcAddr`, and render with an egui Vulkan backend into the swapchain image.
- **OpenGL**: hook `wglSwapBuffers` and draw with the egui glow backend in the game's context.

## VR

A desktop overlay draws onto the game's flat swapchain. A VR game submits its eye images to
OpenXR or OpenVR separately, so in the headset the overlay is not there at all. That is
harmless, but it means VR needs its own path. Because panels are filled one widget at a time
through the service, the same panels can be rendered to a texture and submitted as an
OpenXR/OpenVR quad layer without any consumer changing.
