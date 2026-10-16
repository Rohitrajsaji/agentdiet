#!/bin/sh
# Verify the "single binary, zero runtime deps" rule from CLAUDE.md on a built artifact.
#
#   scripts/check-binary-deps.sh [--build] [--require-sqlite] [BINARY]
#
#   --build           run `cargo build --release --locked` first, so a stale binary is never
#                     what gets inspected (otherwise BINARY is checked as it is on disk).
#   --require-sqlite  also fail unless the binary contains SQLite/FTS5 code. Off by default:
#                     link-time optimisation removes SQLite while nothing reachable from
#                     main() uses memory.rs, so an absent string is not a defect today and a
#                     present string is not proof that FTS5 works. Functionality is covered
#                     by the tests in src/memory.rs; this flag only guards against the
#                     bundled SQLite silently disappearing once the engine is wired in.
#   BINARY            defaults to target/release/agentdiet.
#
# Platform support (anything else exits 2 rather than pretending to pass):
#   macOS  `otool -L`: only /usr/lib/libSystem.B.dylib may be loaded.
#   Linux  `readelf`:  no DT_NEEDED entries and no PT_INTERP (fully static, i.e. musl).
#
# Exit codes: 0 = policy satisfied, 1 = policy violated, 2 = could not check
# (bad usage, missing file or tool, unsupported platform, build failure).

set -u

build=0
require_sqlite=0
bin=""
for arg in "$@"; do
    case "$arg" in
        --build) build=1 ;;
        --require-sqlite) require_sqlite=1 ;;
        -h | --help)
            sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        -*)
            echo "check-binary-deps: unknown option: $arg" >&2
            exit 2
            ;;
        *)
            if [ -n "$bin" ]; then
                echo "check-binary-deps: only one BINARY may be given" >&2
                exit 2
            fi
            bin=$arg
            ;;
    esac
done

root=$(cd "$(dirname "$0")/.." && pwd) || exit 2
[ -n "$bin" ] || bin="$root/target/release/agentdiet"

if [ "$build" -eq 1 ]; then
    (cd "$root" && cargo build --release --locked) || {
        echo "check-binary-deps: release build failed" >&2
        exit 2
    }
fi

if [ ! -f "$bin" ]; then
    echo "check-binary-deps: no such file: $bin (run with --build, or build it first)" >&2
    exit 2
fi

need_tool() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "check-binary-deps: required tool not found: $1" >&2
        exit 2
    }
}

echo "binary:  $bin"
echo "built:   $(ls -l "$bin" | awk '{print $6, $7, $8}')"
if command -v shasum >/dev/null 2>&1; then
    echo "sha256:  $(shasum -a 256 "$bin" | awk '{print $1}')"
elif command -v sha256sum >/dev/null 2>&1; then
    echo "sha256:  $(sha256sum "$bin" | awk '{print $1}')"
fi

status=0
fail() {
    echo "FAIL: $1" >&2
    status=1
}

case "$(uname -s)" in
    Darwin)
        need_tool otool
        libs=$(otool -L "$bin" | tail -n +2 | awk '{print $1}')
        echo "dylibs:"
        echo "$libs" | sed 's/^/  /'
        for lib in $libs; do
            [ "$lib" = "/usr/lib/libSystem.B.dylib" ] || fail "unexpected dylib: $lib"
        done
        ;;
    Linux)
        need_tool readelf
        needed=$(readelf -d "$bin" 2>/dev/null | awk '/\(NEEDED\)/ {print $NF}')
        interp=$(readelf -l "$bin" 2>/dev/null | awk '/Requesting program interpreter/ {print $NF}')
        echo "needed:  ${needed:-<none>}"
        echo "interp:  ${interp:-<none>}"
        [ -z "$needed" ] || fail "dynamic dependencies present (expected fully static): $needed"
        [ -z "$interp" ] || fail "dynamic loader requested (expected fully static): $interp"
        ;;
    *)
        echo "check-binary-deps: unsupported platform $(uname -s); only macOS and Linux are checked" >&2
        exit 2
        ;;
esac

sqlite_hits=$(LC_ALL=C grep -a -c -i 'fts5' "$bin" || true)
echo "fts5 matching lines: ${sqlite_hits:-0} (informational; not proof of functionality)"
if [ "$require_sqlite" -eq 1 ] && [ "${sqlite_hits:-0}" -eq 0 ]; then
    fail "--require-sqlite given but the binary contains no FTS5/SQLite code"
fi

if [ "$status" -eq 0 ]; then
    echo "OK: runtime dependency policy satisfied"
fi
exit "$status"
