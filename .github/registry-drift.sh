#!/usr/bin/env bash
# Has the registry changed a file Preflight ports? Compares each file listed in
# .github/registry-port.txt at the listed commit with the registry's main.
# Exit 0: none changed. Exit 1: names each changed or missing file, with a link.
# Run by the Registry drift workflow every week; also runs locally (needs git).
set -euo pipefail

list="$(dirname "$0")/registry-port.txt"
repo=https://github.com/modelcontextprotocol/registry
pin=$(awk '$1 == "commit" { print $2 }' "$list")
paths=$(awk '!/^#/ && NF && $1 != "commit" { print $1 }' "$list")

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
git init -q "$tmp"
git -C "$tmp" fetch -q --depth=1 --filter=blob:none "$repo" "$pin" refs/heads/main:refs/remotes/registry/main
main=$(git -C "$tmp" rev-parse registry/main)

changed=0
for p in $paths; do
  old=$(git -C "$tmp" rev-parse -q --verify "$pin:$p" || true)
  new=$(git -C "$tmp" rev-parse -q --verify "registry/main:$p" || true)
  if [ -z "$old" ]; then
    echo "LISTED WRONG  $p isn't in the registry at ${pin:0:7}"
    changed=1
  elif [ -z "$new" ]; then
    echo "GONE     $p is no longer on main (moved or deleted)"
    changed=1
  elif [ "$old" != "$new" ]; then
    echo "CHANGED  $p  $repo/compare/$pin...$main"
    changed=1
  else
    echo "same     $p"
  fi
done

if [ "$changed" = 1 ]; then
  echo
  echo "The port needs a look (checked against ${pin:0:7}, main is ${main:0:7})."
  echo "Read the change; update the port if it needs it, then move the commit in .github/registry-port.txt."
  exit 1
fi
echo
echo "No change since ${pin:0:7} (main is ${main:0:7})."
