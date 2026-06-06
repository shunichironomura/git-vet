# Instruction for Coding Agents

## No fallbacks, workarounds, or partial correctness

- This project is not yet published, so breaking changes are acceptable for simpler/clever/clean design and implementation.
  - DO NOT implement fallbacks/workarounds/partial patch for backward compatibility or any other purposes. If a major design change, such as data model modification, is needed, just make the change and update the existing codebase accordingly.

## Type Safety: Encode Semantics in Types, Not Conventions

- **Functional core, imperative shell** The core logic should be as functional as possible with semantics-encoding types and use as little mutable pattern as possible.
- **No flat-string encodings of structured data.**
- **No string-matched control flow**
- **Stringify only at boundaries.** Rendering for diagnostics, debug output, file/wire serialization, or third-party APIs is fine — but the conversion happens at the boundary, not throughout the functional core. Inside the core, pattern-match on the typed variant.

## Git integration philosophy: use `gix` for structured data, `git` for Git behavior

- Prefer `gix` inside the functional core when reading or manipulating structured Git data:
  - repository discovery
  - `HEAD` tree/blob lookup
  - tracked files at `HEAD`
  - object IDs, file modes, refs, and typed domain queries
  - reviewed-set membership and review-state classification
- Prefer invoking the `git` CLI at boundaries where the desired behavior is Git's high-level porcelain behavior rather than merely object access:
  - `git diff` for user-facing patch output; stream stdout instead of re-rendering Git-like diffs manually
  - `git notes` for notes read/write/merge/prune semantics
  - `git log --follow`-style history when exact rename-following/path-history behavior matters
  - `git config` when updating config values such as `notes.mergeStrategy`
- Avoid parsing human-oriented porcelain output. If invoking `git`, prefer documented, narrow, machine-oriented output and isolate parsing in a small adapter.
  - Good: stream `git diff` without parsing; parse object IDs from `git notes list`; parse our own JSON records from `git notes show`.
  - Be careful: `git log --follow` output should use raw/NUL-delimited or otherwise machine-oriented formats.
- Do not reimplement a high-level Git command from low-level `gix` plumbing unless there is a deliberate reason and tests cover Git-compatibility edge cases.
- Keep subprocess use in the imperative shell/adapters; keep the core logic typed and independent of command output formats.

## Useful resources

- Read `SPEC.md` for the specifications.
- Read `PLAN.md` for the implementation plan of the specifications.
