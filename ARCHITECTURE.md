# Architecture

`git-vet` is a Rust library plus a thin `git-vet` binary. The library keeps Git-facing I/O at the command/adapter boundary and represents review semantics with typed domain values (`RepoPath`, `BlobOid`, `ReviewChannel`, `NotesRef`, `ReviewState`, `ReviewRecord`, `ReviewedSet`).

## Module dependency order

The library modules form a directed acyclic graph (DAG). The list below is topologically sorted from low-level domain/support modules to the CLI shell:

1. `path` — repository-relative UTF-8 path type, lexical normalization helpers, and current-working-directory/Git-prefix conversion.
2. `git_ref_format` — small boundary adapter for strict `git check-ref-format` validation. It owns the unforgeable `StrictGitRefName` proof type returned only after Git accepts a concrete ref name without normalization.
3. `channel` — review channels, type-state channel candidates, default channel selection, typed Git notes refs (`refs/notes/vet/<channel>`), and channel validation errors. It converts an unvalidated channel candidate into a validated candidate only when given a matching `StrictGitRefName` proof.
4. `remote` — typed sync remote names, remote-name provenance, and remote-selection errors.
5. `git_types` — typed Git object IDs, file modes, tracked files, and commit/blob display/serialization wrappers.
6. `review` — review records, vetter identity, reviewed sets, derived review states, classified files, and note-record parsing/rendering/sort-deduplication.
7. `error` — application error type and conversions from lower-level path/channel/remote/JSON/I/O/Git errors.
8. `vetignore` — repo-root `.vetignore` loading and matching with gitignore syntax.
9. `git` — repository discovery, path normalization against repo prefix, HEAD tree/blob queries with `gix`, review/user config lookup, dirty-path checks, remote selection, history walking, rename-aware raw-log parsing, and Git diff streaming.
10. `notes` — Git-notes storage adapter, `NotesStore` trait, note listing/showing/writing/removal/pruning, and channel sync fetch/merge/push behavior.
11. `status_output` — human-readable, colored, JSON, and check-mode status rendering.
12. `sync_progress` — typed sync progress steps/outcomes plus interactive spinner and plain non-TTY renderers for sync progress.
13. `commands` — command workflows and core orchestration, including dirty-path prompting, sync progress orchestration, and review-state classification.
14. `cli` — clap-based CLI shell, global `--channel`, channel selection via CLI/config/default priority, subcommand dispatch, channel ref validation, repository/notes-store construction, TTY-aware progress-reporter selection, and process exit-code mapping.

`src/lib.rs` declares these modules and re-exports the public API used by the binary and external callers. `src/main.rs` is only the binary entry point: it delegates to `run_cli` and maps errors to exit code `2`.
