#!/usr/bin/env bash
# Rust checker for .claude/check.sh, run once per Cargo workspace: cargo fmt, cargo clippy
# with warnings denied, cargo test (both --locked, so Cargo.lock must be committed), and
# cargo deny on full runs when it is installed and the workspace has a deny.toml.
# A scoped run checks only the packages owning the given files (-p); a workspace-level file
# (root manifest, Cargo.lock, tool config) checks the whole workspace.
# Usage: rust.sh --full | rust.sh <repo-relative paths...>
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd -P)
cd "$root"
# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$root/.claude/checks/lib.sh"
lang_init rust
require cargo

# abs DIR: absolute form of a repo-relative directory.
abs() { if [ "$1" = . ]; then printf '%s\n' "$root"; else printf '%s\n' "$root/$1"; fi; }

# workspace_of DIR: repo-relative directory of the workspace root for the manifest in DIR.
workspace_of() {
  local ws
  ws=$(cargo locate-project --workspace --message-format plain --manifest-path "$(abs "$1")/Cargo.toml") || return 1
  ws=$(dirname "$ws")
  if [ "$ws" = "$root" ]; then echo .; else printf '%s\n' "${ws#"$root"/}"; fi
}

current="rust: locate workspaces"
full=0
targets="" # lines of "<workspace dir><TAB><package manifest dir, or * for the whole workspace>"
if [ "${1:-}" = "--full" ]; then
  full=1
  files=$(repo_files)
  while IFS= read -r f; do
    case "${f##*/}" in Cargo.toml) targets+="$(workspace_of "$(dirname "$f")")"$'\t*\n' ;; esac
  done <<<"$files"
else
  for f in "$@"; do
    man=$(nearest Cargo.toml "$(dirname "$f")")
    if [ -z "$man" ]; then
      echo "== rust: $f is not in a Cargo package, not checked"
      continue
    fi
    ws=$(workspace_of "$man")
    case "${f##*/}" in
      *.rs) ;;
      Cargo.toml) [ "$man" != "$ws" ] || man="*" ;;
      *) man="*" ;;
    esac
    targets+="$ws"$'\t'"$man"$'\n'
  done
fi

workspaces=()
while IFS= read -r w; do workspaces+=("$w"); done < <(printf '%s' "$targets" | cut -f1 | sort -u)
[ ${#workspaces[@]} -gt 0 ] || exit 0

for ws in "${workspaces[@]}"; do
  cd "$(abs "$ws")"
  mans=()
  while IFS= read -r m; do mans+=("$m"); done < <(printf '%s' "$targets" | awk -F'\t' -v w="$ws" '$1 == w { print $2 }' | sort -u)

  # Map manifest dirs to package names; a virtual manifest or "*" means the whole workspace.
  names=()
  for m in "${mans[@]}"; do
    if [ "$m" = "*" ]; then
      names=()
      break
    fi
    require jq
    current="rust: cargo metadata"
    if [ -z "${meta:-}" ]; then meta=$(cargo metadata --no-deps --format-version 1); fi
    n=$(jq -r --arg p "$(abs "$m")/Cargo.toml" '.packages[] | select(.manifest_path == $p) | .name' <<<"$meta")
    if [ -z "$n" ]; then
      names=()
      break
    fi
    names+=("$n")
  done
  unset meta

  if [ ${#names[@]} -eq 0 ]; then
    echo "== rust: workspace $ws"
    sel=(--workspace)
    fmt_sel=(--all)
  else
    echo "== rust: packages ${names[*]} (workspace $ws)"
    sel=()
    for n in "${names[@]}"; do sel+=(-p "$n"); done
    fmt_sel=("${sel[@]}")
  fi

  step fmt
  cargo fmt "${fmt_sel[@]}" --check

  step clippy
  cargo clippy "${sel[@]}" --all-targets --locked -- -D warnings

  step test
  cargo test "${sel[@]}" --locked

  if [ "$full" -eq 1 ]; then
    if ! command -v cargo-deny >/dev/null; then
      skip cargo-deny "not installed"
    elif [ ! -f deny.toml ]; then
      skip cargo-deny "no deny.toml"
    else
      step deny
      cargo deny check
    fi
  fi
done
