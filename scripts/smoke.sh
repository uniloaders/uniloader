#!/usr/bin/env bash
# End-to-end smoke test under Wine, no game involved: stages smoke-host.exe with
# uniloader.dll copied to version.dll and the hello plugin in plugins/, runs it in a private
# Wine prefix and checks that the forwarded call answered and that the plugin started.
#
#   scripts/smoke.sh           build (release, Windows target) and run
#   scripts/smoke.sh --no-build   run what is already built
#   scripts/smoke.sh --vm      native Windows instead: the dual-boot Windows VM (`winvm`,
#                              skills-private/dual-boot-windows-as-vm.md), which must be
#                              running; the stage is under target/smoke-vm (no DXVK) and
#                              reaches the guest as D:\ over virtiofs. The VM has no GPU:
#                              Direct3D 11 is WARP, OpenGL is GDI's, Vulkan is absent.
#
# Env: SMOKE_WINE=path to a wine binary (default: the newest GE-Proton's).
# The prefix lives in target/smoke-prefix and is only ever touched through its own
# wineserver; nothing else running under Wine or Proton is affected.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET=x86_64-pc-windows-msvc
OUT="$ROOT/target/$TARGET/release"
STAGE="$ROOT/target/smoke"
export WINEPREFIX="$ROOT/target/smoke-prefix"

BUILD=1
VM=0
for arg in "$@"; do
    case "$arg" in
        --no-build) BUILD=0 ;;
        --vm) VM=1 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

if (( BUILD )); then
    distrobox enter build-box -- bash -c "export PATH=/opt/rust/cargo/bin:\$PATH RUSTUP_HOME=/opt/rust/rustup CARGO_HOME=\$HOME/.cargo; cd '$ROOT' && nice -n 10 cargo xwin build --release --target $TARGET -p uniloader -p hello-plugin -p lua-bridge -p kitchen-sink -p overlay -p smoke-host"
fi

if (( VM )); then
    STAGE="$ROOT/target/smoke-vm"
    case "$STAGE" in
        /run/media/system/Data/*) ;;
        *) echo "--vm needs the tree under /run/media/system/Data (the guest's D:)" >&2; exit 1 ;;
    esac
    rel="${STAGE#/run/media/system/Data}"
    GUEST_STAGE="D:${rel//\//\\}"
    winvm state | grep -q ': running' || { echo "the Windows VM is not running (winvm start)" >&2; exit 1; }
fi

if (( ! VM )); then
WINE="${SMOKE_WINE:-$(ls -d "$HOME"/.local/share/Steam/compatibilitytools.d/GE-Proton*/files/bin/wine 2>/dev/null | sort -V | tail -1)}"
if [[ ! -x "$WINE" ]]; then
    echo "no wine found; set SMOKE_WINE" >&2
    exit 1
fi
WINESERVER="$(dirname "$WINE")/wineserver"
# Direct3D 11 the way Proton games get it: DXVK, staged next to the host rather than installed
# into the prefix. Wine's own wined3d is not usable from a bare Proton prefix.
DXVK="$(dirname "$WINE")/../lib/wine/dxvk/x86_64-windows"
fi

# The VM maps these DLLs over virtiofs and keeps their pages cached: a `cp` over the same
# file (same inode) made the next run fault with STATUS_IN_PAGE_ERROR (0xc0000006). Fresh
# files every run.
if (( VM )); then
    [[ "$STAGE" == "$ROOT/target/smoke-vm" ]] && rm -rf "$STAGE"
fi
mkdir -p "$STAGE/plugins" "$STAGE/extra" "$STAGE/uniloader/config"
rm -rf "$STAGE/uniloader/logs"
cp "$OUT/smoke-host.exe" "$STAGE/"
cp "$OUT/uniloader.dll" "$STAGE/version.dll"
# Proton ships these read-only; `install` gives the copies a mode the next run can replace.
(( VM )) || install -m 0644 "$DXVK/d3d11.dll" "$DXVK/dxgi.dll" "$STAGE/"
cp "$OUT/hello.dll" "$OUT/lua_bridge.dll" "$OUT/kitchen_sink.dll" "$OUT/overlay.dll" "$STAGE/plugins/"
mkdir -p "$STAGE/plugins/lua"
cat > "$STAGE/plugins/lua/smoke.lua" <<'LUA'
uniloader.setting("speed", "Speed", 2.5, function(v) uniloader.log("speed now " .. v) end)
uniloader.hotkey("wave", "Wave", "f10", function() print("wave") end)
uniloader.command("echo", "Echo the arguments.", "<text>", function(args) print(args) end)
local ok, lines = uniloader.exec("get lua.speed")
uniloader.log("lua smoke: version " .. uniloader.version .. ", get ok=" .. tostring(ok) .. " -> " .. tostring(lines[1]))
LUA
# Stand-ins for an .asi mod and a chain-loaded library such as UE4SS: any DLL will do.
cp "$OUT/hello.dll" "$STAGE/plugins/standin.asi"
cp "$OUT/hello.dll" "$STAGE/extra/chained.dll"
printf 'load = ["extra/chained.dll"]\n' > "$STAGE/uniloader/config/loader.toml"

status=0
if (( VM )); then
    echo "== running in the Windows VM ($GUEST_STAGE)"
    # In the logged-on user's desktop session: from ssh (session 0) DXGI refuses a swapchain
    # (0x887A0022). The run writes its output and exit code into the stage, which is shared.
    rm -f "$STAGE/run.log" "$STAGE/exit.txt"
    winvm desktop --cwd "$GUEST_STAGE" cmd.exe /c "smoke-host.exe > run.log 2>&1 & call echo %^errorlevel% > exit.txt"
    winvm ps "\$t = 0; while (-not (Test-Path '$GUEST_STAGE\\exit.txt') -and \$t -lt 150) { Start-Sleep 1; \$t++ }" \
        --timeout 170
    [[ -f "$STAGE/exit.txt" ]] || { echo "FAIL: smoke-host did not finish in the VM" >&2; exit 1; }
    tr -d '\r' < "$STAGE/run.log"
    tr -d '\r' < "$STAGE/run.log" > "$STAGE/run.log.tmp" && mv "$STAGE/run.log.tmp" "$STAGE/run.log"
    status="$(tr -dc '0-9' < "$STAGE/exit.txt")"
else
echo "== running under $WINE"
# Native first for version.dll: Wine would otherwise use its builtin and skip the proxy.
# DXVK's VR bridges stay off: with them it opens whatever OpenXR or OpenVR runtime is active,
# and a smoke test must never touch a VR session that is running.
( cd "$STAGE" && WINEDEBUG=-all \
    WINEDLLOVERRIDES="version=n,b;d3d11,dxgi=n;wineopenxr,openvr_api_dxvk=d" \
    DXVK_LOG_LEVEL=none DXVK_STATE_CACHE=0 timeout 120 "$WINE" smoke-host.exe ) \
    | tee "$STAGE/run.log" || status=$?
timeout 30 "$WINESERVER" -k 2> /dev/null || true
fi

log="$(ls "$STAGE"/uniloader/logs/*.log 2> /dev/null | tail -1 || true)"
echo "== loader log: ${log:-none}"
[[ -n "$log" ]] && cat "$log"

# Without a GPU (the VM) Windows offers only OpenGL 1.1 and no Vulkan: those parts are
# reported as skipped when the loader says so, never silently.
NO_GL=0
NO_VK=0
if (( VM )) && [[ -n "$log" ]]; then
    grep -q 'OpenGL 1.1 is older than 3.0' "$log" && NO_GL=1 && echo "== OpenGL overlay skipped: the VM has OpenGL 1.1 only"
    grep -q 'Unable to find a Vulkan driver' "$log" && NO_VK=1 && echo "== Vulkan overlay skipped: the VM has no Vulkan driver"
fi

fail=0
grep -q '^forwarded' "$STAGE/run.log" || { echo "FAIL: version.dll call was not forwarded"; fail=1; }
[[ -n "$log" ]] || { echo "FAIL: the loader wrote no log"; fail=1; }
if [[ -n "$log" ]]; then
    grep -q 'proxy for version.dll' "$log" || { echo "FAIL: proxy summary missing"; fail=1; }
    grep -q '4 of 4 plugins running' "$log" || { echo "FAIL: not every plugin started"; fail=1; }
    grep -qE 'kitchen self-test: [0-9]+ passed, 0 failed' "$log" \
        || { echo "FAIL: the kitchen sink self-test failed or did not run"; fail=1; }
    grep -q 'lua smoke: version .* get ok=true -> lua.speed = 2.5' "$log" \
        || { echo "FAIL: the Lua script did not run or could not read its setting"; fail=1; }
    grep -q 'loaded asi plugins/standin.asi' "$log" || { echo "FAIL: .asi not loaded"; fail=1; }
    grep -q 'loaded library extra/chained.dll' "$log" || { echo "FAIL: load list not loaded"; fail=1; }
    # The overlay, end to end: the vtable hook, the renderer, a consumer's panel body drawn
    # through the service, a present subscriber, and a dialog answer delivered after a frame.
    expect=('hooked IDXGISwapChain::Present' 'overlay ready (Direct3D 11)'
        'kitchen panel drawn' 'kitchen saw a present' 'kitchen dialog answered 2'
        'hooked wglSwapBuffers')
    (( NO_GL )) || expect+=('overlay ready (OpenGL)')
    (( NO_VK )) || expect+=('hooked vkQueuePresentKHR (Vulkan)' 'overlay ready (Vulkan)')
    for expected in "${expect[@]}"; do
        grep -q "$expected" "$log" || { echo "FAIL: overlay: no '$expected'"; fail=1; }
    done
fi
grep -qE '^presented [1-9]' "$STAGE/run.log" || { echo "FAIL: the host presented no frames"; fail=1; }
# The host clears every frame to one colour; what differs in the last one is the overlay.
drawn="$(awk '/^drawn pixels/ {print $3}' "$STAGE/run.log")"
(( ${drawn:-0} > 10000 )) || { echo "FAIL: the overlay drew ${drawn:-no} pixels"; fail=1; }
echo "== overlay drew ${drawn:-0} pixels; see $STAGE/capture.bmp"
grep -qE '^swapped [1-9]' "$STAGE/run.log" || { echo "FAIL: the host swapped no OpenGL frames"; fail=1; }
if (( ! NO_GL )); then
gl_drawn="$(awk '/^gl drawn pixels/ {print $4}' "$STAGE/run.log")"
(( ${gl_drawn:-0} > 10000 )) || { echo "FAIL: the OpenGL overlay drew ${gl_drawn:-no} pixels"; fail=1; }
echo "== OpenGL overlay drew ${gl_drawn:-0} pixels; see $STAGE/capture-gl.bmp"
fi
if (( NO_VK )); then
    :
else
grep -qE '^vk presented [1-9]' "$STAGE/run.log" || { echo "FAIL: the host presented no Vulkan frames"; fail=1; }
# Read back from a re-acquired image: an implementation that does not keep its contents is
# reported, not failed.
if grep -q '^vk read back unreliable' "$STAGE/run.log"; then
    echo "== Vulkan overlay: $(grep -m1 '^vk read back' "$STAGE/run.log")"
else
    vk_drawn="$(awk '/^vk drawn pixels/ {print $4}' "$STAGE/run.log")"
    (( ${vk_drawn:-0} > 10000 )) || { echo "FAIL: the Vulkan overlay drew ${vk_drawn:-no} pixels"; fail=1; }
    echo "== Vulkan overlay drew ${vk_drawn:-0} pixels; see $STAGE/capture-vk.bmp"
fi
fi
if (( status != 0 )) && (( fail == 0 )); then
    echo "FAIL: smoke-host exited with $status"
    fail=1
fi
(( fail == 0 )) && echo "== smoke passed"
exit "$fail"
