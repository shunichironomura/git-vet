# git-vet implementation plan

Based on `SPEC.md` v0.1. Keep Phase 1 small but end-to-end: after Phase 1, `git-vet` should be usable locally to review files, record sign-offs, inspect review diffs, and gate a release. Later phases add collaboration and polish.

## Architecture

- Use a thin imperative CLI shell around a functional core.
- CLI: `clap` derive, binary name `git-vet`, subcommands matching the spec.
- Core domain types:
  - `RepoPath` / normalized path relative to repo root
  - `BlobOid`
  - `ReviewChannel` / typed review pipeline name, defaulting to `default`
  - `NotesRef` / `refs/notes/vet/<channel>`
  - `ReviewState::{Vetted, Stale { baseline }, New}`
  - `ReviewRecord`
  - `ReviewedSet`
- Git integration:
  - Prefer `gix` for repository discovery, object IDs, and tracked file/blob queries where practical.
  - Isolate shell-outs behind small traits/helpers where Git CLI behavior is the source of truth, especially `git notes`, `git diff`, and `git log --follow`.
  - Introduce a `NotesStore` trait immediately so the initial `git notes --ref=vet/<channel> ...` backend can be replaced later if needed.
- Path handling:
  - Normalize user paths once at command entry.
  - Respect `GIT_PREFIX` / repo prefix so `git vet mark foo.rs` works correctly from subdirectories.
  - Only operate on paths tracked at `HEAD`; untracked/missing paths are usage errors.
- Error handling:
  - Use `thiserror` for typed errors.
  - Map release-gate failures to exit code `1`; usage/runtime errors to exit code `2`.

## Phase 1 — MVP: local review and release gate

Goal: a usable local tool for the main workflow: status backlog → diff → mark → check.

### Commands

- `git-vet [--channel <channel>] mark <paths...>`
  - Resolve each path to `HEAD:<path>` blob.
  - Append a provenance record to `refs/notes/vet/<channel>` for that blob. Omit `--channel` to use `default`.
  - Make marking idempotent by reading existing records, appending the new record, sorting/deduplicating, then force-writing the note.
  - Configure `notes.mergeStrategy=cat_sort_uniq` on first write.
- `git-vet [--channel <channel>] status [--json] [--check]`
  - List all tracked, in-scope files, skipping submodules/gitlinks.
  - Load reviewed blob OIDs once from `refs/notes/vet/<channel>`.
  - Apply `.vetignore` using gitignore syntax.
  - Default mode: stably grouped human-readable output for `vetted`, `stale`, and `new`.
  - `--json`: stably sorted JSON object with active `channel` and file records from the spec.
  - `--check`: fast gate path for the selected channel; do only current-blob membership checks, print unreviewed files, exit `1` if any are unreviewed.
- `git-vet [--channel <channel>] diff <path>`
  - If current blob is reviewed, report that the file is up to date.
  - If `new`, show full-file diff from empty tree to current blob.
  - If `stale`, find the newest reviewed historical blob for the path with rename following and show cumulative diff from that blob to `HEAD`.
- `git-vet [--channel <channel>] prune`
  - Prune notes for the selected channel (`git notes --ref=vet/<channel> prune` semantics).

### Core algorithms

- Implement `load_reviewed_set(channel)` once per command invocation.
- Implement `classify_path(path, reviewed_set)`:
  - current blob reviewed in the selected channel → `Vetted`
  - otherwise walk path history newest-to-oldest, following renames, to find a reviewed baseline → `Stale`
  - otherwise → `New`
- Keep history walking out of `status --check`.
- Parse note records enough to populate `last_vetted_at` and `vetted_by` for JSON/status metadata.
- Validate channel names by ensuring `refs/notes/vet/<channel>` is a valid Git refname.

### Tests and acceptance criteria

- Add integration tests that create temporary Git repositories and run the binary.
- Cover:
  - empty default-channel notes ref means tracked files are `new`
  - `mark` makes a file `vetted` in the default channel
  - editing and committing a file makes it `stale`
  - `diff` for `new` and `stale` files shows the expected Git diff
  - `status --check` exits `0` only when all in-scope tracked files are reviewed in the selected channel
  - `.vetignore` excludes files from status/check
  - untracked or missing paths exit `2`
  - default channel writes to `refs/notes/vet/default`
  - channels are independent for `mark`, `status`, `diff`, and `status --check`
  - invalid channel names exit `2`
- Manual smoke test: install/build locally and verify `git vet status` works through Git's `git-*` dispatch when the binary is on `PATH`.

## Phase 2 — Spec completion and collaboration niceties

Goal: finish the remaining command surface and improve team workflows without changing the storage model.

- `git-vet [--channel <channel>] review <paths...>`
  - For each path: show `diff`, prompt for confirmation, then `mark` on approval.
  - Handle non-interactive terminals safely by failing with a clear message unless an explicit confirmation flag is added.
- `git-vet [--channel <channel>] log <path>`
  - Show provenance records for the current blob.
  - Make clear that records are blob-keyed and may include reviews from other paths with identical content.
- `git-vet [--channel <channel>] unmark <paths...>`
  - Remove the note for each current blob.
  - Warn that this affects all paths sharing the same blob.
- `git-vet [--channel <channel>] sync`
  - Fetch `refs/notes/vet/<channel>`, merge with `cat_sort_uniq`, and push the selected channel ref.
  - Prefer the current branch's upstream remote, falling back to `origin`.
  - Produce clear diagnostics when no remote exists.
- Improve edge-case behavior:
  - Warn when the working tree differs from `HEAD` for paths being marked/reviewed.
  - Add explicit handling/tests for renames, identical content in multiple files, deleted files, binary files, and submodules.

## Phase 3 — Packaging, documentation, and polish

Goal: make the tool easy to install, discover, and maintain.

- Packaging:
  - Ensure `Cargo.toml` has `[[bin]] name = "git-vet"` if needed.
  - Add release metadata suitable for `cargo install git-vet`.
  - Optionally provide a `vet` alias/symlink installation note.
- Documentation:
  - Add README usage examples for the steady-state and legacy-repo onboarding workflows.
  - Document blob-keyed behavior, especially identical-content files and renames.
  - Add `.vetignore` examples.
  - Ship `git-vet.1` so `git help vet` works.
- Developer quality:
  - Add CI for `cargo fmt`, `cargo clippy`, and tests.
  - Add property/unit tests for note-record sorting/deduplication and path normalization.
  - Add performance checks for large repositories; optimize only if profiling shows a bottleneck.
