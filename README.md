# git-vet

Git-based vetting gate for tracked file contents.

`git-vet` records that the exact committed contents of a tracked file have been vetted (i.e., reviewed and signed off). Vetting state is keyed by Git blob ID, so editing a file creates new content that must be vetted again.

![Screenshot of `git vet status` showing new, stale, and vetted files](images/git-vet-status.png)

Run `git vet status` to get a review backlog for the repository. It shows files whose current committed contents are `new`, files that are `stale` because they changed since they were last vetted, and files that are already `vetted`. Add `--workspace` to classify the current local working-tree contents instead of committed `HEAD` contents.

## Install

```sh
cargo install git-vet --locked
```

With `git-vet` on `PATH`, you can run it as either:

```sh
git vet status
git-vet status
```

## Basic workflow

```sh
# See the vetting backlog for the default channel
git vet status

# Include uncommitted local edits in status classification
git vet status --workspace

# Show status for one file or a directory subtree
git vet status src/lib.rs
git vet status src/

# Inspect what needs vetting for a file at HEAD
git vet diff src/lib.rs

# Include uncommitted working-tree edits in the diff
git vet diff --workspace src/lib.rs

# Mark the file's current HEAD content as vetted
git vet mark src/lib.rs

# If the working tree is dirty, intentionally mark only the committed HEAD content
git vet mark --allow-dirty src/lib.rs

# Force the current HEAD content to be reviewed again
git vet unmark src/lib.rs

# Gate a release/CI job: exits 1 if any in-scope file is not vetted
git vet status --check

# Gate only a selected file or directory subtree
git vet status --check src/

# Force colored output when piping to another command
git vet --color=always status

# Disable colors explicitly
git vet status --color=never
```

By default, `git-vet` looks at committed `HEAD` contents only. Untracked files are not vetted or gated. `git vet status [PATHSPEC...]` limits status to tracked files matching the given file or directory pathspecs, and `--check`, `--json`, and `--workspace` compose with that same scope. Use `git vet status --workspace` to include uncommitted working-tree edits in local status/check output; for example, a local edit to a file whose `HEAD` blob is vetted appears as `stale`. `git vet mark` still records only committed `HEAD:<path>` bytes and warns before marking a path with uncommitted working-tree changes; use `--allow-dirty` to proceed intentionally without the prompt.

## States

`git vet status` groups tracked files into:

- `new`: this file content has not been vetted in the selected channel.
- `stale`: an earlier version was vetted, but the current `HEAD` content has changed.
- `vetted`: the current `HEAD` content is vetted.

With `git vet status --workspace`, these states are computed from the current working-tree content for tracked files that still exist locally. A local edit to a vetted `HEAD` blob is therefore shown as `stale` until that edited content is committed and marked.

For `new` files, `git vet diff <path>` shows the whole file as a new-file diff. For `stale` files, it shows the cumulative diff since the last vetted version. Add `--workspace` to compare the latest vetted content with the local working-tree file instead of committed `HEAD`.

## Commands

- `git vet status [PATHSPEC...]` — show vetting state for tracked files, optionally limited to files or directory subtrees.
- `git vet status --json [PATHSPEC...]` — emit stable JSON.
- `git vet status --workspace [PATHSPEC...]` — classify local working-tree contents instead of committed `HEAD` contents.
- `git vet status --check [PATHSPEC...]` — print files that are not vetted and exit non-zero if any exist.
- `git vet diff [--workspace] <path>` — show the diff that still needs vetting; `--workspace` includes local working-tree edits.
- `git vet --color=<auto|always|never> ...` — control ANSI color output globally; `--color=always` may be placed after the subcommand as well.
- `git vet mark [--allow-dirty] <paths...>` — mark current `HEAD` contents as vetted.
- `git vet unmark <paths...>` — remove vetting from current `HEAD` contents.
- `git vet channel list [--json]` — list local review-note channels.
- `git vet channel copy <SOURCE> <DESTINATION>` — copy all local review notes into a new channel.
- `git vet channel move <SOURCE> <DESTINATION>` — move all local review notes into a new channel.
- `git vet channel remove <CHANNEL> [--force]` — remove a local review-note channel.
- `git vet prune` — prune stale Git-note entries.

Commands that operate on one review channel accept `--channel <name>` to use an independent vetting channel instead of the configured default:

```sh
git vet --channel security status
git vet --channel security mark src/lib.rs
```

Without `--channel`, `git-vet` uses `git config vet.channel` when set, otherwise `default`:

```sh
git config vet.channel security
git vet status # uses security
```

## Color output

Human-readable output uses colors automatically when written to a terminal. Use `--color=always` or `--color=never` to override detection. `--json` output is never colored.

The `FORCE_COLOR` environment variable enables colors even when output is redirected. A non-empty `NO_COLOR` disables colors; `NO_COLOR=` is treated as unset. Explicit `--color` takes precedence over both variables, and `NO_COLOR` takes precedence over `FORCE_COLOR`.

```sh
FORCE_COLOR=1 git vet status
git vet --color=never status
```

## Copying and moving channel state

Copy all local review notes into a new independent channel with:

```sh
git vet channel copy default release
```

Immediately after copying, both notes refs contain exactly the same review state. Later marks and unmarks remain independent between the channels.

Move local review notes to a new channel with:

```sh
git vet channel move default user-name
git config vet.channel user-name # optional: select the destination by default
```

Both channel names are always explicit; `--channel` cannot be combined with these commands. The source must have a local notes ref and the destination must not already have one. The commands never merge or replace destination review state.

Channel transfers are local-only and move review notes, not other channel-associated state. They do not change `vet.channel`, `.vetignore.<channel>`, the working tree, or remote refs. Publish the destination separately when needed:

```sh
git vet --channel user-name sync
```

Moving a local channel does not delete an existing remote source ref.

Remove a local channel with confirmation:

```sh
git vet channel remove security
```

Use `--force` for non-interactive removal. This deletes only the local notes ref; it does not modify remote refs or `vet.channel`:

```sh
git vet channel remove security --force
```

## Ignoring files

`git-vet` only considers tracked files at `HEAD`, so `.gitignore` is naturally respected. `.vetignore` is an additional filter for tracked files that should not be gated in any channel. It uses gitignore syntax:

```gitignore
Cargo.lock
*.generated.rs
```

You can add channel-specific rules in `.vetignore.<channel>`. Channel names are flat names without `/`, so `--channel security` uses `.vetignore.security`.

Channel-specific rules are loaded after `.vetignore`, so they can re-include globally ignored paths:

```gitignore
# .vetignore
generated/**
```

```gitignore
# .vetignore.security
!generated/security-critical.rs
```

## Notes

Vetting state is stored in Git notes under `refs/notes/vet/<channel>`. Because it is blob-keyed, identical file contents share vetting state within a channel, and renaming an unchanged file keeps it vetted. Unmarking a blob also affects all paths with identical content in that channel.
