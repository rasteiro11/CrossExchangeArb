#!/usr/bin/env bash
# Go checker for .claude/check.sh, run once per Go module: gofmt, go vet, go test -race,
# staticcheck if installed, and govulncheck if installed (full runs only).
# gofmt gets its file list from git, not `go list ./...`, because ./... skips directories whose
# files are all behind build tags; vet and test run on the packages that build here.
# Usage: go.sh --full | go.sh <repo-relative paths...>
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd -P)
cd "$root"
# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$root/.claude/checks/lib.sh"
lang_init go
require go
gopath=$(go env GOPATH)
gobin=$(go env GOBIN)
PATH="$PATH:${gobin:-${gopath%%:*}/bin}"

full=0
targets="" # lines of "<module dir><TAB><package pattern>"
fmt=""     # lines of "<module dir><TAB><file>" to gofmt
whole=""   # module dirs whose every .go file is gofmt-ed
if [ "${1:-}" = "--full" ]; then
  full=1
  files=$(repo_files)
  while IFS= read -r f; do
    case "${f##*/}" in go.mod)
      targets+="$(dirname "$f")"$'\t'"./..."$'\n'
      whole+="$(dirname "$f")"$'\n'
      ;;
    esac
  done <<<"$files"
else
  for f in "$@"; do
    d=$(dirname "$f")
    mod=$(nearest go.mod "$d")
    if [ -z "$mod" ]; then
      echo "== go: $f is not in a Go module, not checked"
      continue
    fi
    case "${f##*/}" in
      go.mod | go.sum)
        targets+="$mod"$'\t'"./..."$'\n'
        whole+="$mod"$'\n'
        ;;
      *)
        # A deleted file's directory may be gone too.
        [ -d "$d" ] || continue
        if [ "$d" = "$mod" ]; then
          pkg=.
        elif [ "$mod" = . ]; then
          pkg="./$d"
        else
          pkg="./${d#"$mod"/}"
        fi
        targets+="$mod"$'\t'"$pkg"$'\n'
        if [ -f "$f" ]; then fmt+="$mod"$'\t'"$f"$'\n'; fi
        ;;
    esac
  done
fi

# Every .go file git knows of in those modules, each assigned to its nearest go.mod so a
# nested module's files are not counted twice.
if [ -n "$whole" ]; then
  files=$(repo_files)
  while IFS= read -r f; do
    case "$f" in *.go) ;; *) continue ;; esac
    [ -f "$f" ] || continue
    mod=$(nearest go.mod "$(dirname "$f")")
    if [ -n "$mod" ] && grep -qxF -- "$mod" <<<"$whole"; then fmt+="$mod"$'\t'"$f"$'\n'; fi
  done <<<"$files"
fi

mods=()
while IFS= read -r m; do mods+=("$m"); done < <(printf '%s' "$targets" | cut -f1 | sort -u)
[ ${#mods[@]} -gt 0 ] || exit 0

# Packages with something to build for this platform and the default tags.
# shellcheck disable=SC2016 # a Go template, not a shell expansion
buildable_tmpl='{{if or .GoFiles .CgoFiles .TestGoFiles .XTestGoFiles .InvalidGoFiles}}{{.ImportPath}}{{end}}'

for mod in "${mods[@]}"; do
  pkgs=()
  while IFS= read -r p; do pkgs+=("$p"); done < <(printf '%s' "$targets" | awk -F'\t' -v m="$mod" '$1 == m { print $2 }' | sort -u)
  cd "$root/$mod"
  echo "== go: module $mod"

  step gofmt
  mod_files=$(printf '%s' "$fmt" | awk -F'\t' -v m="$mod" '$1 == m { print $2 }' | sort -u)
  if [ -n "$mod_files" ]; then
    bad=$(printf '%s\n' "$mod_files" | sed "s|^|$root/|" | tr '\n' '\0' | xargs -0 gofmt -l)
    if [ -n "$bad" ]; then
      printf 'not gofmt-ed:\n%s\n' "$bad" >&2
      false
    fi
  fi

  # -e: list packages with errors too; vet and test report those errors.
  listed=$(go list -e -f "$buildable_tmpl" "${pkgs[@]}")
  build=()
  while IFS= read -r ip; do if [ -n "$ip" ]; then build+=("$ip"); fi; done <<<"$listed"
  if [ ${#build[@]} -eq 0 ]; then
    echo "== go: nothing to build in $mod for this platform and default build tags"
  else
    step vet
    go vet "${build[@]}"

    step "test -race"
    go test -race "${build[@]}"

    if command -v staticcheck >/dev/null; then
      step staticcheck
      staticcheck "${build[@]}"
    else
      skip staticcheck "not installed"
    fi
  fi

  if [ "$full" -eq 1 ]; then
    if command -v govulncheck >/dev/null; then
      step govulncheck
      govulncheck ./...
    else
      skip govulncheck "not installed"
    fi
  fi
done
