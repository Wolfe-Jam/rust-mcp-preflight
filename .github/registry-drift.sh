#!/usr/bin/env bash
# Has the registry changed something Preflight ports? Compares each file listed
# in .github/registry-port.txt at the listed commit with the registry's main,
# and checks that each listed issue is still open.
# Exit 0: nothing changed. Exit 1: names what changed, with a link.
# Exit 2: couldn't check (the list is malformed, or GitHub couldn't be reached).
# Run by the Registry drift workflow every week; also runs locally (needs git,
# curl and python3). GH_TOKEN, if set, is used for the issue lookups.
set -uo pipefail

list="$(dirname "$0")/registry-port.txt"
repo=modelcontextprotocol/registry
url=https://github.com/$repo

cant_check() { echo "COULDN'T CHECK: $1"; exit 2; }

# Fields from the list: Windows line endings dropped, comments (even
# indented) and blank lines skipped.
fields() { awk -v k="$1" '{ sub(/\r$/, "") } $1 ~ /^#/ || !NF { next } k == "path" && $1 != "commit" && $1 != "issue" { print $1; next } $1 == k { print $2 }' "$list"; }

pin=$(fields commit)
[[ $pin =~ ^[0-9a-f]{40}$ ]] || cant_check "the commit in $list must be one full 40-character SHA, got '$pin'"
[ -n "$(fields path)" ] || cant_check "no registry paths listed in $list"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
git init -q "$tmp"
git -C "$tmp" fetch -q --depth=1 --filter=blob:none "$url" "$pin" refs/heads/main:refs/remotes/registry/main ||
  cant_check "couldn't fetch $url at ${pin:0:7} and main; run again"
main=$(git -C "$tmp" rev-parse registry/main)

changed=0
while read -r p; do
  old=$(git -C "$tmp" rev-parse -q --verify "$pin:$p")
  new=$(git -C "$tmp" rev-parse -q --verify "registry/main:$p")
  if [ -z "$old" ]; then
    echo "LISTED WRONG  $p isn't in the registry at ${pin:0:7}"
    changed=1
  elif [ -z "$new" ]; then
    echo "GONE     $p is no longer on main (moved or deleted)"
    changed=1
  elif [ "$old" != "$new" ]; then
    echo "CHANGED  $p  $url/compare/$pin...$main"
    changed=1
  else
    echo "same     $p"
  fi
done < <(fields path)

while read -r n; do
  [ -n "$n" ] || continue
  auth=()
  [ -n "${GH_TOKEN:-}" ] && auth=(-H "Authorization: Bearer $GH_TOKEN")
  state=$(curl -fsS --max-time 20 -H "Accept: application/vnd.github+json" ${auth[@]+"${auth[@]}"} \
    "https://api.github.com/repos/$repo/issues/$n" |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["state"])' 2>/dev/null) ||
    cant_check "couldn't read registry issue #$n (no network, or no such issue)"
  if [ "$state" = open ]; then
    echo "open     issue #$n"
  else
    echo "CLOSED   issue #$n  $url/issues/$n (Preflight's message says it's open)"
    changed=1
  fi
done < <(fields issue)

if [ "$changed" = 1 ]; then
  echo
  echo "The port needs a look (checked against ${pin:0:7}, main is ${main:0:7})."
  echo "Read the change; update the port if it needs it, then move the commit in .github/registry-port.txt."
  exit 1
fi
echo
echo "No change since ${pin:0:7} (main is ${main:0:7})."
