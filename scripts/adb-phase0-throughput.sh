#!/usr/bin/env bash
# ADB Phase 0 throughput + cancellation probe.
#
# Builds two local test fixtures (1 large file, N small files) and pipes
# them as a tar stream to `adb shell tar -x -C <dest>` so we can compare:
#   * end-to-end wall time
#   * cancellation behaviour (Ctrl-C in the middle of streaming)
#   * leftover files on the device after cancel
#
# This is a single-device sanity script; full benchmark vs MTP belongs
# in Phase 5.

set -u
ADB="${ADB:-adb}"
if ! command -v "$ADB" >/dev/null 2>&1; then
    echo "!! adb not found"; exit 2
fi

SERIAL=$("$ADB" devices | awk 'NR>1 && $2=="device" {print $1; exit}')
if [[ -z "$SERIAL" ]]; then echo "!! no device"; exit 3; fi
echo "==> device $SERIAL"

OUT_ROOT="$(cd "$(dirname "$0")" && pwd)/.adb-phase0/$SERIAL"
mkdir -p "$OUT_ROOT"

FIX="$(mktemp -d)"
trap 'rm -rf "$FIX"' EXIT

# --- Fixture 1: single 256 MiB file (smaller than plan's 1GB to stay quick) ---
echo "==> creating 256MiB single file fixture"
mkdir -p "$FIX/big"
dd if=/dev/urandom of="$FIX/big/blob.bin" bs=1m count=256 2>/dev/null

# --- Fixture 2: 2,000 small files (4 KiB each) across nested dirs ---
echo "==> creating 2000 small-file fixture"
mkdir -p "$FIX/many"
( cd "$FIX/many" && for i in $(seq 1 2000); do
    d=$((i / 100))
    mkdir -p "d$d"
    dd if=/dev/urandom of="d$d/f$i.bin" bs=1k count=4 2>/dev/null
done )

DEST_BIG="/sdcard/Download/crossmtp-tp-big-$$"
DEST_MANY="/sdcard/Download/crossmtp-tp-many-$$"
"$ADB" -s "$SERIAL" shell "mkdir -p '$DEST_BIG' '$DEST_MANY'"

run_stream() {
    local label=$1; local src=$2; local dest=$3
    echo
    echo "==> $label  src=$src  dest=$dest"
    local t0=$(date +%s)
    ( cd "$FIX" && tar -cf - "$src" ) | "$ADB" -s "$SERIAL" shell "tar -x -C '$dest'"
    local rc=$?
    local t1=$(date +%s)
    local bytes=$(du -sk "$FIX/$src" | awk '{print $1*1024}')
    local mbs=$(awk -v b="$bytes" -v t="$((t1-t0))" 'BEGIN{ if(t<=0)t=1; printf "%.2f", b/1048576/t }')
    echo "    rc=$rc  wall=$((t1-t0))s  size=${bytes}B  ~${mbs} MiB/s"
    echo "$label rc=$rc wall=$((t1-t0))s bytes=$bytes mibps=$mbs" >> "$OUT_ROOT/throughput.txt"
}

: > "$OUT_ROOT/throughput.txt"
run_stream "big-256MiB" big "$DEST_BIG"
run_stream "many-2000x4KiB" many "$DEST_MANY"

# --- Cancellation probe: kill the local tar | adb pipeline mid-stream ---
echo
echo "==> cancellation probe"
DEST_CANCEL="/sdcard/Download/crossmtp-tp-cancel-$$"
"$ADB" -s "$SERIAL" shell "mkdir -p '$DEST_CANCEL'"
( cd "$FIX" && tar -cf - many ) | "$ADB" -s "$SERIAL" shell "tar -x -C '$DEST_CANCEL'" &
PIPE_PID=$!
sleep 2
echo "    killing pid $PIPE_PID"
kill -INT $PIPE_PID 2>/dev/null
wait $PIPE_PID 2>/dev/null
LEFT=$("$ADB" -s "$SERIAL" shell "find '$DEST_CANCEL' -type f | wc -l" | tr -d '\r')
echo "    leftover files after cancel: $LEFT"
echo "cancel leftover_files=$LEFT" >> "$OUT_ROOT/throughput.txt"

# --- Check stray adb shell tar on device ---
STRAY=$("$ADB" -s "$SERIAL" shell "ps -A 2>/dev/null | grep -E 'tar' | grep -v grep | wc -l" | tr -d '\r')
echo "    stray tar processes on device: $STRAY"
echo "cancel stray_tar_on_device=$STRAY" >> "$OUT_ROOT/throughput.txt"

# --- cleanup ---
"$ADB" -s "$SERIAL" shell "rm -rf '$DEST_BIG' '$DEST_MANY' '$DEST_CANCEL'"
echo
echo "==> done. Results: $OUT_ROOT/throughput.txt"
cat "$OUT_ROOT/throughput.txt"
