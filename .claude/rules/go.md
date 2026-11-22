---
paths:
  - "**/*.go"
  - "**/go.mod"
  - "**/go.sum"
---

# Go rules

These come on top of the shared rules in `.claude/SDLC.md`. The checker is `.claude/checks/go.sh`: gofmt, go vet, `go test -race`, staticcheck, and govulncheck on full runs.

- **Versions**: use the Go version in `go.mod` and what it offers: `log/slog`, `errors.Join`, `slices`, `maps`, method+path patterns in `net/http`.
- **Errors**: wrap once per layer with context: `fmt.Errorf("load user %d: %w", id, err)`. Branch with `errors.Is/As`.
- **Context**: `ctx context.Context` is the first parameter of anything that does I/O or may block. Never store it in a struct. Honor cancellation.
- **Interfaces**: declare them in the consumer package, keep them to 1–3 methods, return concrete types.
- **Concurrency**: every goroutine has an owner and an exit path. The sender closes channels. Shared state sits behind a mutex or inside one goroutine. Tests run with `-race`.
- **Packages**: put non-public code in `internal/`. Don't add a package for one file's worth of code.
- **State**: no `init()` side effects.
- **I/O**: servers and clients get timeouts (no `http.DefaultClient` for external calls). Cap untrusted bodies (`http.MaxBytesReader`). Close bodies. SQL uses `rows.Close()` + `rows.Err()`, and `defer tx.Rollback()`.
- **Logging**: the repo's logger, else `log/slog`.
- **Dependencies**: run `go mod tidy`; never edit `go.sum` by hand.
- **Docs**: every exported identifier has a doc comment.

## Tests

- Table-driven with `t.Run`. Name each case after its AC: `t.Run("AC-2 rejects duplicate order", ...)`.
- Add `t.Parallel()` when there is no shared state. Use `t.Helper()` in helpers, plus `t.TempDir()`, `t.Setenv()` and `t.Context()` (Go ≥1.24).
- Use `httptest` for HTTP and hand-written fakes over mock frameworks. Never `time.Sleep` to synchronize.
- Test behavior through the exported API (`package foo_test`) unless internals are the point.
- Parsers and decoders of untrusted input get a `Fuzz` test.
- Run one test: `go test ./pkg/... -run 'TestX/AC-2'`.

## Review blockers

- `context.Background()`/`TODO()` in a request path. `http.DefaultClient`, or no timeout, for external calls.
- Missing `Body.Close`, `rows.Close`/`rows.Err`, `defer` inside a loop, or a ticker that is never stopped.
- Error `_ =`'d where failure matters; `panic` in a request path.

## Self-check

- [ ] `go mod tidy` is clean
- [ ] Exported identifiers have doc comments
