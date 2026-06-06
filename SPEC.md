# git-vet — Specification

**Spec version:** 0.1 (for implementation)
**Status:** Draft, intended as the contract for human + AI-agent implementation.

---

## 1. Purpose

`git-vet` tracks a per-file **review state** — *reviewed* vs *not reviewed* — for the files tracked in a Git repository, stored entirely in Git itself.

It exists to run a review pipeline that is **independent of the PR/merge pipeline**. You can keep PR review lightweight (merge AI-written code fast, without reading every line) while running a stricter, parallel review that walks the codebase file by file, and gate a release on *every file being reviewed*.

The unit of review is **file content**: reviewing a file means signing off on its exact bytes as they are at `HEAD`. Any later modification — by a human or an AI agent, through any commit — invalidates that sign-off automatically.

---

## 2. Goals and non-goals

### Goals
- Record, in Git, which files' current content has been human-reviewed.
- Make "not reviewed" the default for any new or modified content, with zero manual bookkeeping to invalidate stale sign-offs.
- Support reviewing only the **diff since the last sign-off**, not the whole file, when a prior reviewed version exists.
- Provide a release gate: succeed only if every in-scope tracked file is reviewed.
- Work offline, locally, and sync across machines/teammates through a notes ref.

### Non-goals (explicitly out of scope for v1)
- **No authorship tracking.** The tool does not know or care whether content was written by a human or an AI. There is no git-ai dependency and no AI/human distinction in the model.
- **No PR gating.** It does not block merges or integrate with the PR pipeline. It is a parallel process.
- **No per-line review state.** Review is per file (per blob), not per line or per hunk.
- **No scope seeding / onboarding markers.** Because there is no authorship bit, every tracked file is in scope by default; onboarding a legacy repo requires no special state (see §8).
- Submodules and untracked files are not handled in v1.

---

## 3. Concepts and definitions

- **Blob OID** — the Git object ID of a file's content. `HEAD:<path>` resolves to the blob OID of `<path>` at `HEAD`. Identical content always has the same blob OID.
- **Reviewed set** — the set of blob OIDs that carry a note in `refs/notes/vet`. A blob is "reviewed" iff it is a member.
- **Vetted (stored state)** — a file is *vetted* iff its current blob OID (at `HEAD`) is in the reviewed set. This is the only persisted state. Everything else is derived.
- **Sign-off** — the act of adding the file's current blob OID to the reviewed set (`git vet mark`).

The stored model is binary: a blob is reviewed, or it is not. "Not reviewed" is never written down; it is the absence of a note.

---

## 4. State model

Stored state per file is binary. Because the key is the blob, any edit produces a new blob with no note, so the file drops out of *reviewed* with no explicit action.

```mermaid
stateDiagram-v2
    direction LR
    [*] --> NotReviewed: file first appears
    NotReviewed --> Vetted: git vet mark
    Vetted --> NotReviewed: any edit -> new blob
    NotReviewed --> NotReviewed: edited again
```

For presentation, *not reviewed* is split into two **derived** sub-states (not stored — computed at read time by §7.1):

| Derived state | Meaning | Review mode |
|---|---|---|
| `vetted` | current blob is in the reviewed set | nothing to do |
| `stale` | current blob not reviewed, but some **earlier** version of this path was | diff review (baseline → HEAD) |
| `new` | current blob not reviewed, and no earlier version of this path was either | full review |

The release gate treats both `stale` and `new` as "not reviewed."

---

## 5. Data model and storage

### 5.1 Notes ref
All state lives in `refs/notes/vet`. Each note is attached to a **blob** object (not a commit), keyed by the blob OID. The presence of the note is the signal; the body is provenance only.

### 5.2 Note body
The body is an append-only set of review records, one per line, so the same content reviewed more than once (or by more than one person) accumulates an audit trail:

```
reviewed-at=2026-06-06T12:30:00+09:00 reviewer=user@example.com commit=<sha-at-review> path=src/foo.rs
```

- `reviewed-at` — ISO 8601 timestamp.
- `reviewer` — from `git config user.email`.
- `commit` — the `HEAD` SHA at review time (provenance; the blob is the real key).
- `path` — the path reviewed (provenance only; a blob can appear at multiple paths).

Records are sorted/deduplicated by the notes merge strategy (§5.4), so re-marking identical content is idempotent.

### 5.3 Sync
Notes refs are **not** pushed or fetched by default. The tool must make this easy:
- Push: `git push <remote> refs/notes/vet`
- Fetch refspec: `+refs/notes/vet:refs/notes/vet`
- `git vet sync` wraps fetch + merge + push of this ref.

### 5.4 Merge strategy
Concurrent sign-offs (different machines, different blobs) must not clobber each other. Configure a line-unioning strategy for the ref:
```
git config notes.mergeStrategy cat_sort_uniq
```
With per-blob, line-record bodies this makes merges conflict-free for the common case (different blobs → different note objects; same blob → records union/dedup).

### 5.5 Pruning
Notes for blobs that no longer exist anywhere in the repo accumulate slowly. `git notes --ref=vet prune` removes them. Expose as `git vet prune`. (Trade-off: prune drops sign-off for content not currently present — acceptable, since absent content has nothing to gate.)

---

## 6. Command surface

Invoked as `git vet <cmd>`, `git-vet <cmd>`, or `vet <cmd>` (§10) — identical behavior in all three.

| Command | Behavior |
|---|---|
| `git vet mark <paths…>` | Sign off the current blob of each path: append a review record note in `refs/notes/vet` keyed on `HEAD:<path>`. Idempotent. |
| `git vet status [--json] [--check]` | Classify every in-scope tracked file (§7.1) as `vetted` / `stale` / `new` and report. |
| `git vet diff <path>` | Show the change to review for `<path>`: the cumulative diff from its last-reviewed version to `HEAD` (§7.2). |
| `git vet review <paths…>` | Convenience: `diff` then prompt then `mark` for each path. |
| `git vet log <path>` | Show provenance records for the current blob of `<path>` (who reviewed this content, when, at which commit). |
| `git vet unmark <paths…>` | Remove the note on the current blob of each path, forcing re-review. (Affects all paths sharing that blob — see §9.) |
| `git vet sync` | Fetch, merge, and push `refs/notes/vet`. |
| `git vet prune` | Remove notes for blobs no longer present (`git notes --ref=vet prune`). |

### 6.1 `status` output modes
- Default: human-readable grouping by derived state.
- `--json`: array of objects `{ "path": str, "state": "vetted"|"stale"|"new", "blob": oid, "baseline": oid|null, "last_reviewed_at": str|null, "reviewer": str|null }`.
- `--check`: release-gate mode. Print the unreviewed files; **exit non-zero** if any in-scope file is `stale` or `new`.

### 6.2 Exit codes
- `0` — success; for `--check`, all in-scope files are `vetted`.
- `1` — for `--check`, one or more in-scope files are not reviewed.
- `2` — usage / runtime error (not a Git repo, bad path, etc.).

---

## 7. Key algorithms

### 7.1 Classify a path
```
cur := blob_oid(HEAD, path)            # git rev-parse HEAD:<path>
if cur ∈ reviewed_set: return Vetted
for b in historical_blobs(path):       # git log --follow -- <path>, newest→oldest, excluding cur
    if b ∈ reviewed_set: return Stale(baseline = b)
return New
```
- `reviewed_set` is loaded **once** per invocation: list every note in `refs/notes/vet`, collect the annotated blob OIDs into a `HashSet<Oid>`. Membership tests are O(1).
- The history walk runs only when `cur` is not vetted, and is bounded to the single path via `--follow` (which also tracks the baseline across renames).

### 7.2 Diff to review
```
classify(path):
  Vetted        -> report "up to date", no diff
  New           -> full review: diff empty-tree → cur (the whole file)
  Stale(base)   -> diff base → cur for this path (cumulative change since last sign-off)
```
Binary files: defer to Git's binary-diff behavior; "full review" of a binary means inspecting it out of band.

### 7.3 Release gate (`status --check`)
```
reviewed_set := load once
fail := false
for path in in_scope(git ls-files):    # minus ignored paths (§9.4)
    if classify(path) != Vetted:
        print path, state
        fail := true
exit(1 if fail else 0)
```
The gate never walks history: an unreviewed file fails regardless of whether it is `new` or `stale`, so only the cheap membership test is needed. (The `new`/`stale` distinction is computed only for `status` display and `diff`/`review`.)

---

## 8. Workflow

### 8.1 Steady state
1. Code lands on `main` through the normal, lightweight PR pipeline — no `git-vet` involvement, nothing blocked.
2. In parallel, on your own schedule, you review files. `git vet status` shows the backlog (`new` + `stale`).
3. For a file, `git vet diff <path>` (or `git vet review <path>`) shows what to read — the whole file if `new`, just the change since your last sign-off if `stale`.
4. `git vet mark <path>` signs off the current content.
5. Any subsequent edit by anyone produces a new blob, dropping the file back into the backlog automatically.
6. `git vet sync` shares state across machines/teammates.
7. At release, `git vet status --check` gates on the whole tree being reviewed.

### 8.2 Onboarding a non-new (legacy) repository
No special handling. On install, the reviewed set is empty, so **every tracked file reads as `new`** — the entire existing tree is the initial review backlog, which is exactly the desired starting point. Work it down incrementally with `review`/`mark`; gate with `--check` once the backlog (minus ignored paths) is empty. There is no seeding step, no baseline marker, and no authorship inference — this simplicity is the direct payoff of dropping authorship from the model.

### 8.3 Multi-machine / team
State is shared by pushing/fetching `refs/notes/vet` (§5.3) with `cat_sort_uniq` merge (§5.4). Sign-offs are per-content, so two people reviewing different files never conflict; two people reviewing identical content simply produce two provenance records on the same blob.

---

## 9. Edge cases and required behaviors

1. **Renames.** State is blob-keyed, so identical content at a new path stays `vetted` (the current blob is still in the set). `--follow` finds the baseline for `stale`/`diff` across renames.
2. **Identical content in multiple files.** They share one blob and therefore one note: reviewing one marks all. This is intentional ("review the content"). `log`/provenance records the paths. Document this behavior; do not try to defeat it.
3. **Deleted files.** Not in `git ls-files`, so excluded from `status` and the gate. Their notes linger until `prune`.
4. **Garbage-collected blobs.** Orphaned notes; cleaned by `prune`.
5. **Path not at HEAD / untracked.** Only `git ls-files` paths are in scope. `mark`/`diff` on an untracked or nonexistent path is a usage error (exit 2).
6. **Working-tree vs HEAD.** Review state is defined against committed content (`HEAD:<path>`), not the dirty working tree. Marking signs off the committed blob; uncommitted edits are not considered. (Implementation may warn if the working tree differs from `HEAD` for a path being marked.)
7. **Submodules.** Out of scope in v1; skip gitlink entries.

### 9.4 Ignore list
`status`/`--check` consult a repo-root `.vetignore` using gitignore syntax, so generated, vendored, and lockfile paths can be excluded and "everything reviewed" is attainable. This is a plain denylist for the gate only — not scope-seeding state, and it stores nothing in notes.

---

## 10. Packaging and invocation

- Single Rust binary named `git-vet` (`[[bin]] name = "git-vet"`). Git's wrapper turns `git vet …` into an exec of `git-vet …` automatically when the binary is on `PATH` — no registration or manifest. The subcommand token is consumed by Git, so argv is identical whether invoked as `git vet mark x` or `git-vet mark x`; **do not dispatch on argv[0]**.
- Optionally ship a `vet` symlink/alias to the same binary for a bare short form.
- `cargo install git-vet` is the install path; `git vet` lights up with no further wiring.
- Ship `git-vet.1` so `git help vet` works; `git help -a` and completion discover `git-*` binaries on `PATH`.
- **Path resolution:** when run as `git vet`, Git sets `GIT_PREFIX` to the subdirectory the user invoked from. Resolve user-supplied relative paths against the repository prefix (gix repository discovery, or `git rev-parse --show-prefix`) so `git vet mark foo.rs` from a subdirectory means the same file as the direct invocation. Normalize once, up front.
- **Shadowing caveat:** Git prioritizes its own builtins over `PATH`. If Git ever shipped a builtin `vet`, `git vet` would resolve to it; direct `git-vet`/`vet` invocation is immune. No such builtin exists today.

---

## 11. Implementation notes

- Language: Rust. CLI: `clap` (derive). 
- Git access: `gix` for repository discovery, blob resolution, ref enumeration, and history walking. **Notes read/write may be shelled out to `git notes --ref=vet …`** if `gix` notes support is insufficient; isolate this behind a small `NotesStore` trait so the backend can be swapped.
- Load the reviewed set once per command into a `HashSet<gix::ObjectId>`.
- Keep the history walk (`--follow`) confined to `diff`/`review`/`status`-display; never run it in the gate path.
- Determinism: `status --json` output should be stably ordered (sort by path) for diff-friendly CI use.

---

## 12. Design rationale (informative)

- **Blob-keyed, not commit- or path-keyed:** reviewing is signing off on *content*. Blob-keying makes invalidation automatic (new content ⇒ new key ⇒ no note), survives rebase/squash (same content ⇒ same key), is robust to revert-to-identical, and follows renames for free.
- **No authorship:** authorship via hooks can only attribute content created after install, which makes legacy onboarding require a scope-seeding mechanism. Dropping authorship makes "in scope" universal and "not reviewed" the natural default, eliminating that machinery entirely.
- **Notes, not a tracked manifest:** keeps review state off the main tree (no history pollution, no per-review commits, no merge conflicts on a shared file), at the cost of an explicit push/fetch refspec.
