#!/usr/bin/env bash
# End-to-end smoke test under Wine, no game involved: stages smoke-host.exe with
# uniloader.dll copied to version.dll and the hello plugin in plugins/, runs it in a private
# Wine prefix and checks that the forwarded call answered and that the plugin started.
#
#   scripts/smoke.sh           build (release, Windows target) and run
#   scripts/smoke.sh --no-build   run what is already built
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
for arg in "$@"; do
    case "$arg" in
        --no-build) BUILD=0 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

if (( BUILD )); then
    distrobox enter build-box -- bash -c "export PATH=/opt/rust/cargo/bin:\$PATH RUSTUP_HOME=/opt/rust/rustup CARGO_HOME=\$HOME/.cargo; cd '$ROOT' && nice -n 10 cargo xwin build --release --target $TARGET -p uniloader -p hello-plugin -p lua-bridge -p smoke-host"
fi

WINE="${SMOKE_WINE:-$(ls -d "$HOME"/.local/share/Steam/compatibilitytools.d/GE-Proton*/files/bin/wine 2>/dev/null | sort -V | tail -1)}"
if [[ ! -x "$WINE" ]]; then
    echo "no wine found; set SMOKE_WINE" >&2
    exit 1
fi
WINESERVER="$(dirname "$WINE")/wineserver"

mkdir -p "$STAGE/plugins" "$STAGE/extra" "$STAGE/uniloader/config"
rm -rf "$STAGE/uniloader/logs"
cp "$OUT/smoke-host.exe" "$STAGE/"
cp "$OUT/uniloader.dll" "$STAGE/version.dll"
cp "$OUT/hello.dll" "$OUT/lua_bridge.dll" "$STAGE/plugins/"
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

echo "== running under $WINE"
status=0
# Native first for version.dll: Wine would otherwise use its builtin and skip the proxy.
( cd "$STAGE" && WINEDEBUG=-all WINEDLLOVERRIDES="version=n,b" timeout 120 "$WINE" smoke-host.exe ) \
    | tee "$STAGE/run.log" || status=$?
timeout 30 "$WINESERVER" -k 2> /dev/null || true

log="$(ls "$STAGE"/uniloader/logs/*.log 2> /dev/null | tail -1 || true)"
echo "== loader log: ${log:-none}"
[[ -n "$log" ]] && cat "$log"

fail=0
grep -q '^forwarded' "$STAGE/run.log" || { echo "FAIL: version.dll call was not forwarded"; fail=1; }
[[ -n "$log" ]] || { echo "FAIL: the loader wrote no log"; fail=1; }
if [[ -n "$log" ]]; then
    grep -q 'proxy for version.dll' "$log" || { echo "FAIL: proxy summary missing"; fail=1; }
    grep -q '2 of 2 plugins running' "$log" || { echo "FAIL: hello and lua did not both start"; fail=1; }
    grep -q 'lua smoke: version .* get ok=true -> lua.speed = 2.5' "$log" \
        || { echo "FAIL: the Lua script did not run or could not read its setting"; fail=1; }
    grep -q 'loaded asi plugins/standin.asi' "$log" || { echo "FAIL: .asi not loaded"; fail=1; }
    grep -q 'loaded library extra/chained.dll' "$log" || { echo "FAIL: load list not loaded"; fail=1; }
fi
if (( status != 0 )) && (( fail == 0 )); then
    echo "FAIL: smoke-host exited with $status"
    fail=1
fi
(( fail == 0 )) && echo "== smoke passed"
exit "$fail"
