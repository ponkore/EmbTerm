#!/bin/sh
# macOS への移植のプローブ（docs/research/macos-port.md §3）。
# GUI の Emacs が一瞬立ち上がり、result.txt と shot-*.png を書いて終わる。
set -e
cd "$(dirname "$0")"
EMACS=${EMACS:-emacs}
INCLUDE=${EMACS_INCLUDE:-/Applications/Emacs.app/Contents/Resources/include}
for src in spike ivar; do
  clang -O1 -fobjc-arc -dynamiclib -I"$INCLUDE" \
        -framework Cocoa -framework QuartzCore -o "$src.dylib" "$src.m"
done
PROBE_DIR="$PWD" "$EMACS" -Q -l "$PWD/probe.el"
cat result.txt
