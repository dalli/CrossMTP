#!/usr/bin/env bash
# ADB Phase 0 device probe.
#
# Runs the device-side checks listed in docs/plan.md §8 Phase 0 and
# docs/retrospectives/adb-phase-0.md (Track B). Requires:
#   * One Android device connected via USB
#   * USB debugging enabled and this Mac authorised
#   * adb on PATH or ADB env var pointing to the binary
#
# Outputs are written to scripts/.adb-phase0/<serial>/ so multiple
# devices can be probed in sequence. Exit 0 on success, non-zero if a
# hard precondition fails (no adb, no device, unauthorised). Soft
# failures (missing tar option, unwritable path) are recorded and the
# script keeps going so we get a full matrix per device.

set -u

ADB="${ADB:-adb}"
if ! command -v "$ADB" >/dev/null 2>&1; then
    echo "!! adb not found. Set ADB=/path/to/adb or add it to PATH." >&2
    exit 2
fi

OUT_ROOT="$(cd "$(dirname "$0")" && pwd)/.adb-phase0"
mkdir -p "$OUT_ROOT"

# --- device selection ---
DEVICES=$("$ADB" devices | awk 'NR>1 && $2=="device" {print $1}')
if [[ -z "$DEVICES" ]]; then
    echo "!! No authorised device. Check 'adb devices' output." >&2
    "$ADB" devices >&2
    exit 3
fi

SERIAL="${1:-$(echo "$DEVICES" | head -n1)}"
OUT="$OUT_ROOT/$SERIAL"
mkdir -p "$OUT"
echo "==> probing $SERIAL, results in $OUT"

ashell() { "$ADB" -s "$SERIAL" shell "$@"; }
record() { tee "$OUT/$1" >/dev/null; }

# --- 1. device identity ---
echo "==> [1] device identity"
{
    ashell getprop ro.build.version.release
    ashell getprop ro.build.version.sdk
    ashell getprop ro.product.manufacturer
    ashell getprop ro.product.model
    ashell getprop ro.product.device
} | record device.txt
cat "$OUT/device.txt"

# --- 2. tar availability and flavour ---
echo "==> [2] tar availability"
{
    echo "# which tar"
    ashell 'which tar; command -v tar; busybox --list 2>/dev/null | grep -w tar; toybox --version 2>/dev/null'
    echo "# tar --help (first lines)"
    ashell 'tar --help 2>&1 | head -40'
} | record tar.txt
cat "$OUT/tar.txt"

# --- 3. shared storage writable matrix ---
echo "==> [3] shared storage writable matrix"
PATHS=(
    "/sdcard"
    "/sdcard/Download"
    "/sdcard/Documents"
    "/sdcard/DCIM"
    "/storage/emulated/0"
    "/storage/emulated/0/Download"
    "/storage/emulated/0/Android/data"
)
: > "$OUT/storage.txt"
for p in "${PATHS[@]}"; do
    probe="$p/.crossmtp_probe_$$"
    rc=$(ashell "mkdir -p '$p' 2>/dev/null; echo ok > '$probe' 2>/dev/null && cat '$probe' 2>/dev/null && rm -f '$probe' 2>/dev/null; echo rc=\$?")
    echo "$p -> $rc" | tee -a "$OUT/storage.txt"
done

# --- 4. tar -x -C stdin streaming smoke test ---
echo "==> [4] tar -x -C stdin smoke test"
TMPDIR_LOCAL=$(mktemp -d)
mkdir -p "$TMPDIR_LOCAL/probe/sub"
echo "hello" > "$TMPDIR_LOCAL/probe/a.txt"
printf 'utf8-한글-%s\n' "$(date +%s)" > "$TMPDIR_LOCAL/probe/sub/한글.txt"
DEST="/sdcard/Download/crossmtp-phase0-$$"
ashell "mkdir -p '$DEST'"
( cd "$TMPDIR_LOCAL" && tar -cf - probe ) | "$ADB" -s "$SERIAL" shell "tar -x -C '$DEST'"
ashell "ls -la '$DEST/probe' '$DEST/probe/sub' 2>&1" | record tar-extract.txt
cat "$OUT/tar-extract.txt"

# --- 5. manifest probe candidate (find + stat) ---
echo "==> [5] manifest probe candidate"
{
    echo "# find -printf support"
    ashell "find '$DEST/probe' -printf '%P\t%s\t%T@\n' 2>&1 | head -20"
    echo "# fallback: find + stat"
    ashell "find '$DEST/probe' -type f -exec stat -c '%n %s %Y' {} \; 2>&1 | head -20"
    echo "# fallback: ls -lR"
    ashell "ls -lR '$DEST/probe' 2>&1 | head -40"
} | record manifest.txt
cat "$OUT/manifest.txt"

# --- 6. cleanup ---
ashell "rm -rf '$DEST'"
rm -rf "$TMPDIR_LOCAL"

echo
echo "==> done. Inspect $OUT/ and copy findings into docs/retrospectives/adb-phase-0.md Track B table."
