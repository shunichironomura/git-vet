use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

struct TestRepo {
    dir: TestTempDir,
}

struct TestTempDir {
    path: PathBuf,
}

impl TestTempDir {
    fn new(prefix: &str) -> std::io::Result<Self> {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();

        for attempt in 0..1000 {
            let path = base.join(format!("{prefix}-{pid}-{nanos}-{attempt}"));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not create a unique temporary directory",
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

impl TestRepo {
    fn new() -> Self {
        let dir = TestTempDir::new("git-vet-test").expect("create temp dir");
        run_git(dir.path(), ["init", "-q"]);
        run_git(dir.path(), ["config", "user.email", "reviewer@example.com"]);
        run_git(dir.path(), ["config", "user.name", "Reviewer"]);
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.path().join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(path, contents).expect("write file");
    }

    fn commit_all(&self, message: &str) {
        run_git(self.path(), ["add", "."]);
        run_git(self.path(), ["commit", "-q", "-m", message]);
    }

    fn run_vet(&self, args: &[&str]) -> Output {
        self.run_vet_in(self.path(), args)
    }

    fn run_vet_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_git-vet"))
            .current_dir(cwd)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_PREFIX")
            .output()
            .expect("run git-vet")
    }
}

fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_PREFIX")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn status_json(repo: &TestRepo) -> Vec<Value> {
    let output = repo.run_vet(&["status", "--json"]);
    assert!(
        output.status.success(),
        "status --json failed: {}",
        stderr(&output)
    );
    serde_json::from_slice(&output.stdout).expect("status JSON")
}

fn record_for<'a>(records: &'a [Value], path: &str) -> &'a Value {
    records
        .iter()
        .find(|record| record["path"] == path)
        .unwrap_or_else(|| panic!("missing record for {path}: {records:#?}"))
}

#[test]
fn empty_notes_ref_means_tracked_files_are_new() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let records = status_json(&repo);

    let record = record_for(&records, "a.txt");
    assert_eq!(record["state"], "new");
    assert!(record["baseline"].is_null());
    assert!(record["last_reviewed_at"].is_null());
    assert!(record["reviewer"].is_null());
}

#[test]
fn mark_makes_a_file_vetted() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let mark = repo.run_vet(&["mark", "a.txt"]);
    assert!(mark.status.success(), "mark failed: {}", stderr(&mark));

    let records = status_json(&repo);
    let record = record_for(&records, "a.txt");
    assert_eq!(record["state"], "vetted");
    assert_eq!(record["reviewer"], "reviewer@example.com");
    assert!(!record["last_reviewed_at"].is_null());

    let diff = repo.run_vet(&["diff", "a.txt"]);
    assert!(diff.status.success(), "diff failed: {}", stderr(&diff));
    assert!(stdout(&diff).contains("a.txt is up to date"));
}

#[test]
fn editing_and_committing_a_marked_file_makes_it_stale() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());

    repo.write("a.txt", "hello\nworld\n");
    repo.commit_all("edit");

    let records = status_json(&repo);
    let record = record_for(&records, "a.txt");
    assert_eq!(record["state"], "stale");
    assert!(!record["baseline"].is_null());
    assert_eq!(record["reviewer"], "reviewer@example.com");
}

#[test]
fn diff_for_new_and_stale_files_shows_git_diffs() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let new_diff = repo.run_vet(&["diff", "a.txt"]);
    assert!(
        new_diff.status.success(),
        "new diff failed: {}",
        stderr(&new_diff)
    );
    let new_diff = stdout(&new_diff);
    assert!(
        new_diff.contains("diff --git a/a.txt b/a.txt"),
        "{new_diff}"
    );
    assert!(new_diff.contains("+hello"), "{new_diff}");

    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());
    repo.write("a.txt", "hello\nworld\n");
    repo.commit_all("edit");

    let stale_diff = repo.run_vet(&["diff", "a.txt"]);
    assert!(
        stale_diff.status.success(),
        "stale diff failed: {}",
        stderr(&stale_diff)
    );
    let stale_diff = stdout(&stale_diff);
    assert!(
        stale_diff.contains("diff --git a/a.txt b/a.txt"),
        "{stale_diff}"
    );
    assert!(stale_diff.contains("--- a/a.txt"), "{stale_diff}");
    assert!(stale_diff.contains("+++ b/a.txt"), "{stale_diff}");
    assert!(stale_diff.contains("+world"), "{stale_diff}");
}

#[test]
fn status_check_is_open_only_when_all_in_scope_files_are_reviewed() {
    let repo = TestRepo::new();
    repo.write("a.txt", "a\n");
    repo.write("b.txt", "b\n");
    repo.commit_all("initial");

    let check = repo.run_vet(&["status", "--check"]);
    assert_eq!(check.status.code(), Some(1));
    assert!(stdout(&check).contains("a.txt"));
    assert!(stdout(&check).contains("b.txt"));

    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());
    let check = repo.run_vet(&["status", "--check"]);
    assert_eq!(check.status.code(), Some(1));
    assert!(!stdout(&check).contains("a.txt"));
    assert!(stdout(&check).contains("b.txt"));

    assert!(repo.run_vet(&["mark", "b.txt"]).status.success());
    let check = repo.run_vet(&["status", "--check"]);
    assert!(check.status.success(), "check failed: {}", stderr(&check));
}

#[test]
fn vetignore_excludes_files_from_status_and_check() {
    let repo = TestRepo::new();
    repo.write("kept.txt", "kept\n");
    repo.write("ignored.txt", "ignored\n");
    repo.write(".vetignore", "ignored.txt\n");
    repo.commit_all("initial");

    assert!(repo.run_vet(&["mark", "kept.txt"]).status.success());
    assert!(repo.run_vet(&["mark", ".vetignore"]).status.success());

    let records = status_json(&repo);
    assert!(records.iter().all(|record| record["path"] != "ignored.txt"));

    let check = repo.run_vet(&["status", "--check"]);
    assert!(check.status.success(), "check failed: {}", stderr(&check));
}

#[test]
fn untracked_or_missing_paths_exit_two() {
    let repo = TestRepo::new();
    repo.write("tracked.txt", "tracked\n");
    repo.commit_all("initial");
    repo.write("untracked.txt", "untracked\n");

    let untracked = repo.run_vet(&["mark", "untracked.txt"]);
    assert_eq!(untracked.status.code(), Some(2));
    assert!(stderr(&untracked).contains("not tracked"));

    let missing = repo.run_vet(&["diff", "missing.txt"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(stderr(&missing).contains("not tracked"));
}

#[test]
fn prune_wraps_git_notes_prune() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let prune = repo.run_vet(&["prune"]);
    assert!(prune.status.success(), "prune failed: {}", stderr(&prune));
}

#[test]
fn git_dispatch_finds_git_vet_on_path() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_git-vet"));
    let binary_dir = binary.parent().expect("binary parent");
    let path = env::join_paths(
        std::iter::once(binary_dir.to_path_buf())
            .chain(env::split_paths(&env::var_os("PATH").expect("PATH is set"))),
    )
    .expect("join PATH");

    let output = Command::new("git")
        .current_dir(repo.path())
        .args(["vet", "status", "--check"])
        .env("PATH", path)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_PREFIX")
        .output()
        .expect("run git vet");

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("a.txt"));
}

#[test]
fn relative_paths_are_resolved_from_subdirectories() {
    let repo = TestRepo::new();
    repo.write("nested/file.txt", "hello\n");
    repo.commit_all("initial");

    let mark = repo.run_vet_in(&repo.path().join("nested"), &["mark", "file.txt"]);
    assert!(mark.status.success(), "mark failed: {}", stderr(&mark));

    let records = status_json(&repo);
    let record = record_for(&records, "nested/file.txt");
    assert_eq!(record["state"], "vetted");
}
