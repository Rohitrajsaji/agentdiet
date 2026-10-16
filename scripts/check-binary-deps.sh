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
# The script fails closed. Exit 0 means a supported, well-formed artifact was inspected and
# satisfies the policy; it is never the answer to "I could not tell".
#
#   macOS  The file must start with Mach-O (or fat) magic. `otool -L` must exit 0 and print
#          output that parses completely (a header line, then one `<path> (compatibility
#          version ...)` line per dylib). Apple's otool exits 0 and prints "is not an object
#          file" for junk, so the exit status alone proves nothing. /usr/lib/libSystem.B.dylib
#          must be listed and must be the only dylib.
#   Linux  The file must start with ELF magic and `readelf -h`/`-l` must describe it. There may
#          be no DT_NEEDED entry and no PT_INTERP (fully static, i.e. musl).
#
# Exit codes: 0 = policy satisfied, 1 = policy violated (a forbidden dependency was found in a
# valid artifact), 2 = could not check (bad usage, missing/unreadable/truncated/wrong-format
# file, missing or failing inspector, unparseable inspector output, unsupported platform, build
# failure).

set -u

die() {
    echo "check-binary-deps: $1" >&2
    exit 2
}

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
        -*) die "unknown option: $arg" ;;
        *)
            [ -z "$bin" ] || die "only one BINARY may be given"
            bin=$arg
            ;;
    esac
done

root=$(cd "$(dirname "$0")/.." && pwd) || exit 2
[ -n "$bin" ] || bin="$root/target/release/agentdiet"
# A leading dash would be read as an option by the inspectors.
case "$bin" in
    -*) bin=./$bin ;;
esac

if [ "$build" -eq 1 ]; then
    (cd "$root" && cargo build --release --locked) || die "release build failed"
fi

[ -f "$bin" ] || die "no such file: $bin (run with --build, or build it first)"
[ -r "$bin" ] || die "cannot read: $bin"

need_tool() {
    command -v "$1" >/dev/null 2>&1 || die "required tool not found: $1"
}

need_tool od
need_tool awk
need_tool grep

# The first four bytes as lowercase hex, "" if the file is shorter (truncated or empty).
hex=$(od -An -tx1 -N4 "$bin" 2>&1)
od_status=$?
[ "$od_status" -eq 0 ] || die "cannot read the header of $bin: $hex"
magic=$(printf '%s' "$hex" | tr -d ' \n')
if [ "${#magic}" -ne 8 ]; then
    die "$bin is truncated or empty (fewer than 4 bytes); not an executable"
fi

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
        # Mach-O 64/32-bit little endian, or a fat/universal container (also the magic of Java
        # class files, which the otool parse below rejects).
        case "$magic" in
            cffaedfe | cefaedfe | cafebabe | cafebabf) ;;
            *) die "$bin is not a Mach-O executable (magic $magic)" ;;
        esac
        need_tool otool
        out=$(otool -L "$bin" 2>&1)
        otool_status=$?
        [ "$otool_status" -eq 0 ] || die "otool -L failed (exit $otool_status): $out"
        # Parse every line: nothing may be left over. awk prints one `DYLIB <path>` line per
        # dependency, or `BAD <line>` for anything it does not understand.
        parsed=$(printf '%s\n' "$out" | awk -v bin="$bin" '
            function bad(l) { print "BAD " l; broken = 1 }
            NR == 1 && $0 != bin ":" && index($0, bin " (architecture ") != 1 { bad($0); next }
            NR == 1 { headers = 1; next }
            index($0, bin " (architecture ") == 1 && substr($0, length($0) - 1) == "):" { headers++; next }
            /^\t.+ \(compatibility version [0-9.]+, current version [0-9.]+[^)]*\)$/ {
                line = substr($0, 2)
                sub(/ \(compatibility version .*$/, "", line)
                print "DYLIB " line
                next
            }
            { bad($0) }
            END { if (!headers) print "BAD <no output>" }
        ')
        awk_status=$?
        [ "$awk_status" -eq 0 ] || die "could not parse the otool output (awk exit $awk_status)"
        case "$parsed" in
            *BAD*) die "unexpected otool output for $bin: $(printf '%s\n' "$parsed" | grep '^BAD ' | head -n 3 | tr '\n' ';')" ;;
        esac
        libs=$(printf '%s\n' "$parsed" | sed -n 's/^DYLIB //p')
        echo "dylibs:"
        printf '%s\n' "$libs" | sed 's/^/  /'
        printf '%s\n' "$libs" | grep -qx '/usr/lib/libSystem.B.dylib' ||
            die "libSystem.B.dylib is not among the dylibs of $bin; refusing to treat it as a valid executable"
        extra=$(printf '%s\n' "$libs" | grep -vx '/usr/lib/libSystem.B.dylib')
        grep_status=$?
        [ "$grep_status" -le 1 ] || die "internal error filtering dylibs (grep exit $grep_status)"
        for lib in $extra; do
            fail "unexpected dylib: $lib"
        done
        ;;
    Linux)
        case "$magic" in
            7f454c46) ;;
            *) die "$bin is not an ELF executable (magic $magic)" ;;
        esac
        need_tool readelf
        header=$(readelf -h "$bin" 2>&1)
        rc=$?
        [ "$rc" -eq 0 ] || die "readelf -h failed (exit $rc): $header"
        printf '%s\n' "$header" | grep -q 'ELF Header:' ||
            die "readelf -h did not describe an ELF header for $bin"
        phdrs=$(readelf -l "$bin" 2>&1)
        rc=$?
        [ "$rc" -eq 0 ] || die "readelf -l failed (exit $rc): $phdrs"
        printf '%s\n' "$phdrs" | grep -q 'Program Headers:' ||
            die "$bin has no program headers; not a loadable executable"
        dyn=$(readelf -d "$bin" 2>&1)
        rc=$?
        [ "$rc" -eq 0 ] || die "readelf -d failed (exit $rc): $dyn"
        if printf '%s\n' "$dyn" | grep -q 'There is no dynamic section'; then
            needed=""
        elif printf '%s\n' "$dyn" | grep -q 'Dynamic section at offset'; then
            needed=$(printf '%s\n' "$dyn" | awk '/\(NEEDED\)/ {print $NF}')
        else
            die "unexpected readelf -d output for $bin"
        fi
        interp=$(printf '%s\n' "$phdrs" | awk '/Requesting program interpreter/ {print $NF}')
        echo "needed:  ${needed:-<none>}"
        echo "interp:  ${interp:-<none>}"
        [ -z "$needed" ] || fail "dynamic dependencies present (expected fully static): $needed"
        [ -z "$interp" ] || fail "dynamic loader requested (expected fully static): $interp"
        ;;
    *)
        die "unsupported platform $(uname -s); only macOS and Linux are checked"
        ;;
esac

sqlite_hits=$(LC_ALL=C grep -a -c -i 'fts5' "$bin")
grep_status=$?
# grep -c exits 1 when the count is 0; anything above is a real error.
[ "$grep_status" -le 1 ] || die "could not scan $bin for FTS5 code (grep exit $grep_status)"
echo "fts5 matching lines: ${sqlite_hits:-0} (informational; not proof of functionality)"
if [ "$require_sqlite" -eq 1 ] && [ "${sqlite_hits:-0}" -eq 0 ]; then
    fail "--require-sqlite given but the binary contains no FTS5/SQLite code"
fi

if [ "$status" -eq 0 ]; then
    echo "OK: runtime dependency policy satisfied"
fi
exit "$status"
