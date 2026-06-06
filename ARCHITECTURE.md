# Architecture

## Module dependency order

The library is split into modules that form a directed acyclic graph (DAG). The list below is topologically sorted from low-level domain/support modules to the CLI shell:

1. `path` — repo-relative path types and lexical path normalization helpers.
2. `channel` — review channels and typed Git notes refs.
3. `git_types` — typed Git object IDs, file modes, tracked files, and historical blobs.
4. `review` — review records, reviewed sets, derived review states, and note-record parsing/rendering.
5. `error` — application error type and conversions from lower-level error types.
6. `vetignore` — `.vetignore` loading and matching.
7. `git` — repository discovery, tracked-file lookup, history walking, rename following, and diff rendering.
8. `notes` — Git notes storage backend and `NotesStore` trait.
9. `status_output` — human-readable and JSON status rendering.
10. `commands` — command workflows for mark, status, check, and diff.
11. `cli` — clap-based CLI shell and `run_cli` entry point.

`src/lib.rs` declares these modules and re-exports the public API used by the binary and external callers.
