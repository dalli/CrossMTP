#!/usr/bin/env bash
# Phase 1 manual verification harness.
#
# Run with a real Android device attached and authorised for MTP. This
# script exercises every public mtp-session code path through mtp-cli and
# leaves artifacts under /tmp/crossmtp-verify so the orchestrator phase
# can replay the same operations.
#
# Exit 0 on success, non-zero on first failure. Each step prints what it
# did and, on failure, what we observed.

set -u
WORKDIR="${TMPDIR:-/tmp}/crossmtp-verify"
mkdir -p "$WORKDIR"

run() {
    local label=$1; shift
    echo
    echo "==> $label"
    echo "    \$ $*"
    "$@"
    local rc=$?
    if [[ $rc -ne 0 ]]; then
        echo "    !! exit $rc"
        return $rc
    fi
}

CLI=(cargo run --quiet -p mtp-cli --)

run "1. list devices"            "${CLI[@]}" devices             || exit 1
run "2. list storages"           "${CLI[@]}" storages            || exit 1

read -r -p "Enter a storage_id from above (e.g. 0x00010001): " SID
run "3. list root of $SID"       "${CLI[@]}" ls "$SID"           || exit 1

read -r -p "Enter a file_id to download from the listing above: " FID
DEST="$WORKDIR/pulled-$FID.bin"
run "4. download file $FID"      "${CLI[@]}" pull "$SID" "$FID" "$DEST" || exit 1
ls -lh "$DEST"

read -r -p "Enter a folder_id under the same storage to upload INTO: " PID
SAMPLE="$WORKDIR/sample-upload.txt"
date > "$SAMPLE"
run "5. upload sample file"      "${CLI[@]}" push "$SID" "$PID" "$SAMPLE" || exit 1

echo
echo "all checks passed."
