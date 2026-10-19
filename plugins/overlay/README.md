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

The same from the loader console: `overlay.toggle console|settings|mouse`. For testing by code,
`overlay.input type <text>`, `overlay.input key enter` (escape, tab, arrows and a few more)
and `overlay.input click <x> <y>` post the window messages real input would. They go through
the same input path and take no focus from other windows.

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
| Direct3D 12 (native DXGI, vkd3d-proton) | drawn through Direct3D 11 on 12; verified in Train Sim World 7 under Proton |
| OpenGL 3.0 and later (WGL) | drawn; verified in the Wine smoke test |
| Vulkan | not hooked yet |

The hook is universal for DXGI. `Present`, `Present1` and `ResizeBuffers` are virtual
functions shared by every swapchain of one implementation. A throwaway swapchain on a hidden
window is enough to find the vtable, and patching it through the loader covers every swapchain
the game makes. This works whether or not uniloader itself is the `dxgi.dll` proxy, and
whether the game made its swapchain before or after the plugin loaded.

Direct3D 12 gets a throwaway swapchain of its own, because under Proton it presents through
vkd3d-proton's swapchain class rather than DXVK's; when the class differs it is hooked too.
The game's direct queue is taken from `ExecuteCommandLists`, and a Direct3D 11 device layered
over it (`D3D11On12CreateDevice`, native on Windows and implemented by DXVK over vkd3d-proton)
runs the same egui renderer into the backbuffer, wrapped and released within each `Present`.
That layer exists only while something is shown: hidden for 3 s, it is dropped. It is never
dropped as the render thread ends, though, because that runs under the loader lock and the
layer's teardown waits for DXVK's worker threads, which need the same lock (the game hung
on quit that way). `d3d12.dll` is loaded at run time, so where it cannot load, only the
Direct3D 12 part is skipped.

## Showcase cards and a game's own UI

Cards a plugin sets with `card_set` are drawn at the bottom centre while an overlay window
is open. A game UI plugin that shows them its own way (tsw-sdk's `tsw_ui` on Train Sim
World's main menu) subscribes to the `overlay.card` topic, keeps its own
`showcase::Showcase` from the messages ([`card_bus`](../../crates/uniloader-overlay/src/card_bus.rs)),
and sets `overlay.cards_elsewhere = true`; the overlay then draws no cards. Every card is
sent again whenever plugins are announced running, since events published while plugins
are still starting reach nobody.

`OpenGL` is drawn from a detour on `opengl32!wglSwapBuffers`. `gdi32!SwapBuffers` ends up
there too, so the detour sees every frame however the game presents. The detour goes in when
`opengl32.dll` is loaded: at start, or within the first minute for games that load it at run
time (SDL, GLFW). The overlay never loads it itself, so a `Direct3D` game is untouched. A
small glow painter (`win/gl/painter.rs`) draws egui into the default framebuffer in the game's
own context. It needs `OpenGL` 3.0, uses GLSL 1.30 or 1.50 as the context allows, and reads
every piece of state it changes beforehand and puts it back afterwards, because the game draws
on with the same context. A context the game replaces gets a new painter. Present subscribers
get `GraphicsApi::OPENGL`, with the `HDC` as `swapchain` and the `HGLRC` as `device`.

Vulkan (`win/vk`) is drawn from a detour on the driver's `vkQueuePresentKHR`. Which function
that is comes from a throwaway instance and device made once `vulkan-1.dll` is loaded (at
start, or within the first minute): every device of a driver gets the same functions, so
detouring them sees the game's calls whether it went through the loader's exports or
`vkGetDeviceProcAddr`. The overlay needs to know which device a queue belongs to and a
swapchain's format and window, so it also detours `vkCreateDevice`, `vkGetDeviceQueue(2)`,
`vkCreateWin32SurfaceKHR`, `vkCreateSwapchainKHR` and their destroy calls, and records them
as the game makes them. A game that made its device before the overlay loaded is therefore
not drawn (it says so once). Inside the present, a command buffer of the overlay's waits for
the game's semaphores, draws egui into the image with its own render pass and pipeline
(`win/vk/render.rs`, SPIR-V built from `shaders/` by `shaders/build.sh`), and signals a
semaphore the present then waits for instead. The image stays in `PRESENT_SRC_KHR` throughout.
Under Proton every `Direct3D` game presents through Vulkan as well (DXVK, vkd3d-proton);
while DXGI presents are coming in, the Vulkan path stands aside, so those frames are drawn
once, by the DXGI hook. Present subscribers get `GraphicsApi::VULKAN`, with the
`VkSwapchainKHR` as `swapchain` and the `VkDevice` as `device`.

`ui` builds an egui frame and knows nothing about devices; `win` owns the renderers, each one
behind the same `Gui::run`.

## VR

A desktop overlay draws onto the game's flat swapchain. A VR game submits its eye images to
OpenXR or OpenVR separately, so in the headset the overlay is not there at all. That is
harmless, but it means VR needs its own path. Because panels are filled one widget at a time
through the service, the same panels can be rendered to a texture and submitted as an
OpenXR/OpenVR quad layer without any consumer changing.
