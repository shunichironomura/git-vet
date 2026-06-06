# git-vet

Git-based vetting gate for tracked file contents.

`git-vet` records that the exact committed contents of a tracked file have been vetted (i.e., reviewed and signed off). Vetting state is keyed by Git blob ID, so editing a file creates new content that must be vetted again.

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

# Inspect what needs vetting for a file
git vet diff src/lib.rs

# Mark the file's current HEAD content as vetted
git vet mark src/lib.rs

# Force the current HEAD content to be reviewed again
git vet unmark src/lib.rs

# Gate a release/CI job: exits 1 if any in-scope file is not vetted
git vet status --check
```

`git-vet` looks at committed `HEAD` contents only. Untracked files and uncommitted working-tree edits are not vetted or gated.

## States

`git vet status` groups tracked files into:

- `new`: this file content has not been vetted in the selected channel.
- `stale`: an earlier version was vetted, but the current `HEAD` content has changed.
- `vetted`: the current `HEAD` content is vetted.

For `new` files, `git vet diff <path>` shows the whole file as a new-file diff. For `stale` files, it shows the cumulative diff since the last vetted version.

## Commands

- `git vet status` — show vetting state for tracked files.
- `git vet status --json` — emit stable JSON.
- `git vet status --check` — print files that are not vetted and exit non-zero if any exist.
- `git vet diff <path>` — show the diff that still needs vetting.
- `git vet mark <paths...>` — mark current `HEAD` contents as vetted.
- `git vet unmark <paths...>` — remove vetting from current `HEAD` contents.
- `git vet prune` — prune stale Git-note entries.

All commands accept `--channel <name>` to use an independent vetting channel instead of `default`:

```sh
git vet --channel security status
git vet --channel security mark src/lib.rs
```

## Ignoring files

`git-vet` only considers tracked files at `HEAD`, so `.gitignore` is naturally respected. `.vetignore` is an additional filter for tracked files that should not be gated. It uses gitignore syntax:

```gitignore
Cargo.lock
*.generated.rs
```

## Notes

Vetting state is stored in Git notes under `refs/notes/vet/<channel>`. Because it is blob-keyed, identical file contents share vetting state within a channel, and renaming an unchanged file keeps it vetted. Unmarking a blob also affects all paths with identical content in that channel.
