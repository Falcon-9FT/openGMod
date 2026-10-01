#!/bin/sh
# Runs crates/gmod-glua/testdata/semantics.lua, translated by gmod-glua, under a LuaJIT binary
# and compares the output with semantics.expected (what the untranslated file printed under a
# LuaJIT that understands GMod's additions natively).
#
#   scripts/glua-semantics.sh /path/to/luajit
#
# Any LuaJIT works, 2.0 included: the translation is plain Lua 5.1 with goto.
set -eu
luajit=${1:?usage: scripts/glua-semantics.sh /path/to/luajit}
root=$(cd "$(dirname "$0")/.." && pwd)
data="$root/crates/gmod-glua/testdata"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo run -q --release --manifest-path "$root/Cargo.toml" -p gmod-check -- glua "$data/semantics.lua" > "$tmp/semantics.lua"
"$luajit" "$tmp/semantics.lua" > "$tmp/output.txt"
if diff -u "$data/semantics.expected" "$tmp/output.txt"; then
    echo "semantics: identical ($("$luajit" -v | head -n 1))"
else
    echo "semantics: DIFFERENT" >&2
    exit 1
fi
