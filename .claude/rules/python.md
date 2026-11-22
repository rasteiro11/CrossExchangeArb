---
paths:
  - "**/*.py"
  - "**/pyproject.toml"
  - "**/uv.lock"
---

# Python rules

These come on top of the shared rules in `.claude/SDLC.md`. The checker is `.claude/checks/python.sh`, which runs `ruff format --check`, `ruff check`, pyright (if installed) and `pytest`, all through `uv run --locked`.

- **Scope**: Python is for research and analysis (`research/`). It never sits in the trading path and never reimplements strategy logic. It analyzes the engine's replay output instead, so the backtest cannot drift from the live engine.
- **Tooling**: one uv project per directory with `pyproject.toml`. `uv.lock` is committed. Pin dev tools (ruff, pytest, pyright) as dev dependencies. Type hints go on public functions.
- **Precision**: use `decimal.Decimal` when reproducing or asserting engine numbers. Floats are fine for statistics, never for asserting P&L equality.
- **Data**: polars or duckdb over the NDJSON/Parquet the engine writes. Logic that tests need lives in modules, not notebooks.
- **I/O**: research code reads local files only. No exchange or network access, and no API keys.
- **Notebooks**: clear outputs that contain account data before committing.

## Tests

- pytest. Name tests after the AC: `def test_ac_2_rejects_duplicate_order():`. Use `pytest.mark.parametrize` with `ids` for table cases.
- Use `tmp_path` and `monkeypatch` for files and env.
- Run one test: `uv run pytest -k ac_2`.

## Review blockers

- Network or exchange access, or secrets, in research code or notebooks.
- Strategy or fee logic reimplemented in Python instead of read from engine output.

## Self-check

- [ ] `uv.lock` was updated by uv, not by hand
