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
    assert_git_success(git_output(cwd, args));
}

fn git_output<const N: usize>(cwd: &Path, args: [&str; N]) -> Output {
    Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_PREFIX")
        .output()
        .expect("run git")
}

fn assert_git_success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "git failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn status_json(repo: &TestRepo) -> Vec<Value> {
    status_json_with_args(repo, &["status", "--json"])
}

fn status_json_with_args(repo: &TestRepo, args: &[&str]) -> Vec<Value> {
    let output = repo.run_vet(args);
    assert!(
        output.status.success(),
        "status --json failed: {}",
        stderr(&output)
    );
    let document: Value = serde_json::from_slice(&output.stdout).expect("status JSON");
    document["files"]
        .as_array()
        .unwrap_or_else(|| panic!("missing files array in status JSON: {document:#?}"))
        .clone()
}

fn status_json_document(repo: &TestRepo, args: &[&str]) -> Value {
    let output = repo.run_vet(args);
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

    let document = status_json_document(&repo, &["status", "--json"]);
    let records = document["files"].as_array().expect("files array");

    assert_eq!(document["channel"], "default");
    let record = record_for(records, "a.txt");
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
fn mark_writes_default_channel_git_notes() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let mark = repo.run_vet(&["mark", "a.txt"]);
    assert!(mark.status.success(), "mark failed: {}", stderr(&mark));

    let note = assert_git_success(git_output(
        repo.path(),
        ["notes", "--ref=vet/default", "show", "HEAD:a.txt"],
    ));
    let note = stdout(&note);
    assert!(note.contains("reviewer=reviewer@example.com"), "{note}");
    assert!(note.contains("path=a.txt"), "{note}");

    let strategy = assert_git_success(git_output(
        repo.path(),
        ["config", "--get", "notes.mergeStrategy"],
    ));
    assert_eq!(stdout(&strategy), "cat_sort_uniq\n");
}

#[test]
fn status_reads_default_channel_git_notes() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    let head = assert_git_success(git_output(repo.path(), ["rev-parse", "HEAD"]));
    let head = stdout(&head).trim().to_owned();
    let note = format!(
        "reviewed-at=2026-06-06T00:00:00Z reviewer=reviewer@example.com commit={head} path=a.txt"
    );
    run_git(
        repo.path(),
        [
            "notes",
            "--ref=vet/default",
            "add",
            "-m",
            &note,
            "HEAD:a.txt",
        ],
    );

    let records = status_json(&repo);
    let record = record_for(&records, "a.txt");
    assert_eq!(record["state"], "vetted");
    assert_eq!(record["reviewer"], "reviewer@example.com");
    assert_eq!(record["last_reviewed_at"], "2026-06-06T00:00:00Z");
}

#[test]
fn review_channels_are_independent() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let mark_default = repo.run_vet(&["mark", "a.txt"]);
    assert!(
        mark_default.status.success(),
        "default mark failed: {}",
        stderr(&mark_default)
    );

    let default_records = status_json(&repo);
    let default_record = record_for(&default_records, "a.txt");
    assert_eq!(default_record["state"], "vetted");

    let security_document =
        status_json_document(&repo, &["status", "--json", "--channel", "security"]);
    assert_eq!(security_document["channel"], "security");
    let security_records = security_document["files"].as_array().expect("files array");
    let security_record = record_for(security_records, "a.txt");
    assert_eq!(security_record["state"], "new");

    let mark_security = repo.run_vet(&["mark", "a.txt", "--channel", "security"]);
    assert!(
        mark_security.status.success(),
        "security mark failed: {}",
        stderr(&mark_security)
    );

    let security_records =
        status_json_with_args(&repo, &["status", "--json", "--channel", "security"]);
    let security_record = record_for(&security_records, "a.txt");
    assert_eq!(security_record["state"], "vetted");

    let security_note = assert_git_success(git_output(
        repo.path(),
        ["notes", "--ref=vet/security", "show", "HEAD:a.txt"],
    ));
    assert!(
        stdout(&security_note).contains("reviewer=reviewer@example.com"),
        "{}",
        stdout(&security_note)
    );
}

#[test]
fn status_check_is_channel_specific() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());

    let default_check = repo.run_vet(&["status", "--check"]);
    assert!(
        default_check.status.success(),
        "default check failed: {}",
        stderr(&default_check)
    );

    let security_check = repo.run_vet(&["--channel", "security", "status", "--check"]);
    assert_eq!(security_check.status.code(), Some(1));
    assert!(stdout(&security_check).contains("a.txt"));
}

#[test]
fn invalid_channel_names_exit_two() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let output = repo.run_vet(&["status", "--channel", "bad..channel"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("invalid review channel"));
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

    let security_records =
        status_json_with_args(&repo, &["status", "--json", "--channel", "security"]);
    let security_record = record_for(&security_records, "a.txt");
    assert_eq!(security_record["state"], "new");
    assert!(security_record["baseline"].is_null());
    assert!(security_record["reviewer"].is_null());
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
