#!/usr/bin/env bash
# The gate: cargo fmt --check, clippy (pedantic, -D warnings) and the tests on the host, then
# rustdoc, then clippy and a release build for x86_64-pc-windows-msvc (the loader and plugins
# only exist on Windows), and a Linux release build of the config editor. Runs inside the
# `build-box` distrobox and re-execs itself there from the host. Nothing is filtered: each step's full output is on screen and in
# target/gate-logs/<step>.log.
#
#   scripts/gate.sh            full gate
#   scripts/gate.sh --host     host steps only (fmt, clippy, tests, docs)
#
# Env: UNILOADER_JOBS=N (default: 90% of the cores), UNILOADER_NICE=N (default 10).
set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
ROOT="$(cd "$(dirname "$SCRIPT")/.." && pwd)"

if [[ "${CONTAINER_ID:-}" != build-box ]]; then
    exec distrobox enter build-box -- bash -c "export PATH=/opt/rust/cargo/bin:\$PATH RUSTUP_HOME=/opt/rust/rustup CARGO_HOME=\$HOME/.cargo; $(printf '%q ' "$SCRIPT" "$@")"
fi

HOST_ONLY=0
for arg in "$@"; do
    case "$arg" in
        --host) HOST_ONLY=1 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

TARGET=x86_64-pc-windows-msvc
LOGS="$ROOT/target/gate-logs"
mkdir -p "$LOGS"
export CARGO_BUILD_JOBS="${UNILOADER_JOBS:-$(( $(nproc) * 9 / 10 > 0 ? $(nproc) * 9 / 10 : 1 ))}"
renice -n "${UNILOADER_NICE:-10}" -p $$ > /dev/null

step() {
    local name="$1"
    shift
    echo "== $name"
    "$@" > >(tee "$LOGS/$name.log") 2>&1
}

cd "$ROOT"
step fmt cargo fmt --all --check
step clippy cargo clippy --workspace --all-targets -- -D warnings
step test cargo test --workspace
step doc env RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps
if (( HOST_ONLY )); then
    echo "== host gate passed"
    exit 0
fi
# The Windows target: `cargo xwin` supplies the MSVC CRT/SDK, lld-link and clang-cl, the same
# way the DayZ loader builds, so no .cargo/config.toml is needed.
step clippy-windows cargo xwin clippy --workspace --all-targets --target "$TARGET" -- -D warnings
step build-windows cargo xwin build --release --workspace --target "$TARGET"
# The config editor is the one program that also runs on Linux: target/release/uniloader-config.
step build-linux cargo build --release -p uniloader-config
echo "== gate passed"
