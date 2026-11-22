#!/usr/bin/env bash
# PostToolUse hook for Edit|Write: format the file Claude just changed with its language's
# formatter, when that formatter is installed. Only that file is touched: rustfmt reads stdin
# so it does not follow `mod x;` into other files, and runs from the file's directory so it
# finds that crate's rustfmt.toml. Rust uses edition 2024 (.claude/rules/rust.md).
# Never blocks: always exits 0.
f=$(jq -r '.tool_input.file_path // empty' 2>/dev/null)
if [ -z "$f" ] || [ ! -f "$f" ]; then exit 0; fi

case "$f" in
  *.go) command -v gofmt >/dev/null && gofmt -w "$f" ;;
  *.py)
    # Inside a uv project use its pinned ruff, as the gate does; else a global ruff.
    proj=$(dirname "$f")
    while [ "$proj" != / ] && [ ! -f "$proj/pyproject.toml" ]; do proj=$(dirname "$proj"); done
    if [ -f "$proj/pyproject.toml" ] && command -v uv >/dev/null &&
      uv run --locked --directory "$proj" ruff format --quiet "$f"; then
      :
    elif command -v ruff >/dev/null; then
      ruff format --quiet "$f"
    fi
    ;;
  *.rs)
    if command -v rustfmt >/dev/null; then
      tmp=$(mktemp)
      # cat > keeps the file's inode and permissions; unchanged files keep their mtime.
      if (cd "$(dirname "$f")" && rustfmt --edition 2024) <"$f" >"$tmp" && ! cmp -s "$tmp" "$f"; then
        cat "$tmp" >"$f"
      fi
      rm -f "$tmp"
    fi
    ;;
esac >/dev/null 2>&1
exit 0
