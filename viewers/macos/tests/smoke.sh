#!/usr/bin/env bash
# Drive diskonaut-mac through a script of real keys and clicks, and check what it holds after
# each (see src/mac/script.rs). Needs a logged-in macOS session, and no permissions: the events
# are the app's own. The window comes to the front while it runs; leave the machine alone.
#
#   viewers/macos/tests/smoke.sh            # builds, runs, prints ok or what differed
set -euo pipefail

cd "$(dirname "$0")/../../.."
cargo build -q -p diskonaut-mac
app="$PWD/target/debug/diskonaut-mac"

work="$(mktemp -d "${TMPDIR:-/tmp}/diskonaut-mac-smoke.XXXXXX")"
trap 'rm -rf "$work"' EXIT
fx="$work/fx" out="$work/out"
mkdir -p "$fx/alpha/inner" "$fx/beta" "$out"
head -c 300000 /dev/zero >"$fx/alpha/inner/big.bin"
head -c 100000 /dev/zero >"$fx/alpha/a.dat"
head -c 200000 /dev/zero >"$fx/beta/b.dat"
head -c 50000 /dev/zero >"$fx/victim.log"
head -c 30000 /dev/zero >"$fx/other.log"
printf 'hello\nworld\n' >"$fx/it's \$odd.txt"
# As the app reports it: symlinks resolved (/var is /private/var).
fx="$(cd "$fx" && pwd -P)"
# Listing, largest first: alpha beta victim.log other.log it's $odd.txt.
# List rows are 22 points tall from y = 30: row n's middle is 41 + 22n.

cat >"$work/script" <<SCRIPT
state $out/start
key down
state $out/down
key return
state $out/entered
key esc
state $out/back
key cmd+c
clipboard $out/copy
key a
state $out/apparent
key a
key end
state $out/end
click 100 41
click 100 63 1 cmd
state $out/marked
click 100 41
rclick 100 41
menu Copy as Pathname
clipboard $out/menu-copy
rclick 100 63
menu cancel
key ctrl+cmd+s
state $out/no-sidebar
key ctrl+cmd+s
state $out/sidebar
key home
key down
key down
state $out/victim
key cmd+opt+backspace
state $out/alert
key return
state $out/deleted
key space
wait 500
state $out/quicklook
key space
wait 300
click 14 41
state $out/opened
key down
state $out/nested
quit
SCRIPT

# A watchdog, as macOS has no timeout(1): an alert that never closes would otherwise hang here.
DISKONAUT_MAC_SCRIPT="$work/script" "$app" "$fx" &
pid=$!
(sleep 120 && kill "$pid" 2>/dev/null && echo "FAIL timed out after 120s") &
watchdog=$!
status=0
wait "$pid" || status=$?
kill "$watchdog" 2>/dev/null || true
wait "$watchdog" 2>/dev/null || true
if [ "$status" -ne 0 ]; then
    echo "FAIL diskonaut-mac exited with status $status"
    exit 1
fi

failures=0
expect() { # file, line expected in it
    if ! grep -qxF -- "$2" "$out/$1"; then
        echo "FAIL $1: expected '$2', got:"
        sed 's/^/    /' "$out/$1" 2>/dev/null || echo "    (missing)"
        failures=$((failures + 1))
    fi
}
expect start "selected: alpha"
expect start "scanning: false"
expect down "selected: beta"
expect entered "path: beta"
expect entered "title: beta"
expect back "path: "
expect back "selected: beta"
expect copy "$fx/beta"
expect apparent "apparent: true"
expect end "selected: it's \$odd.txt"
expect end 'preview: Text(["hello", "world"])'
expect marked "marked: alpha | beta"
expect menu-copy "$fx/alpha"
expect no-sidebar "sidebar: false"
expect sidebar "sidebar: true"
expect victim "selected: victim.log"
expect alert "key window:  (_NSAlertPanel), active true"
expect deleted "listing: alpha | beta | other.log | it's \$odd.txt"
expect deleted "status: Deleted 1 item, freeing 52.0K"
expect quicklook "key window:  (QLPreviewPanel), active true"
# alpha's expander, at the list's left: it opens in place, and ↓ goes into it.
expect opened "rows: alpha | alpha/inner | alpha/a.dat | beta | other.log | it's \$odd.txt"
expect opened "cursor: alpha"
expect nested "cursor: alpha/inner"
[ -e "$fx/victim.log" ] && { echo "FAIL victim.log is still on disk"; failures=$((failures + 1)); }
[ -e "$fx/other.log" ] || { echo "FAIL other.log was deleted"; failures=$((failures + 1)); }

if [ "$failures" -eq 0 ]; then echo ok; else echo "$failures failed"; exit 1; fi
