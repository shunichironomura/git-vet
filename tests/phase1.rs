use std::env;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

fn require<T, E: fmt::Display>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => fail(&format!("{context}: {error}")),
    }
}

fn require_some<T>(option: Option<T>, context: &str) -> T {
    option.unwrap_or_else(|| fail(context))
}

fn fail<T>(message: &str) -> T {
    let _ = writeln!(io::stderr().lock(), "{message}");
    std::process::abort()
}

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
        let dir = require(TestTempDir::new("git-vet-test"), "create temp dir");
        run_git(dir.path(), ["init", "-q"]);
        run_git(dir.path(), ["config", "user.email", "reviewer@example.com"]);
        run_git(dir.path(), ["config", "user.name", "Reviewer"]);
        Self { dir }
    }

    fn clone_from(remote: &Path) -> Self {
        let dir = require(TestTempDir::new("git-vet-clone"), "create temp dir");
        require(
            fs::remove_dir_all(dir.path()),
            "remove empty clone destination",
        );
        let output = Command::new("git")
            .args(["clone", "-q", path_str(remote), path_str(dir.path())])
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_PREFIX")
            .output()
            .unwrap_or_else(|error| fail(&format!("run git clone: {error}")));
        assert_git_success(output);
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
            require(fs::create_dir_all(parent), "create parent dir");
        }
        require(fs::write(path, contents), "write file");
    }

    fn commit_all(&self, message: &str) {
        run_git(self.path(), ["add", "."]);
        run_git(self.path(), ["commit", "-q", "-m", message]);
    }

    fn add_remote(&self, name: &str, remote: &Path) {
        run_git(self.path(), ["remote", "add", name, path_str(remote)]);
    }

    fn current_branch(&self) -> String {
        let output = assert_git_success(git_output(self.path(), ["branch", "--show-current"]));
        stdout(&output).trim().to_owned()
    }

    fn push_head_to(&self, remote: &str) {
        let refspec = format!("HEAD:{}", self.current_branch());
        run_git(self.path(), ["push", remote, refspec.as_str()]);
    }

    fn push_head_to_and_set_upstream(&self, remote: &str) {
        let refspec = format!("HEAD:{}", self.current_branch());
        run_git(self.path(), ["push", "-u", remote, refspec.as_str()]);
    }

    fn run_vet(&self, args: &[&str]) -> Output {
        Self::run_vet_in(self.path(), args)
    }

    fn run_vet_without_user_config(&self, args: &[&str]) -> Output {
        let empty_global_config = self.path().join(".empty-global-config");
        require(
            fs::write(&empty_global_config, ""),
            "write empty global config",
        );
        let mut command = Self::vet_command(self.path(), args);
        command
            .env("GIT_CONFIG_GLOBAL", empty_global_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap_or_else(|error| fail(&format!("run git-vet: {error}")))
    }

    fn run_vet_in(cwd: &Path, args: &[&str]) -> Output {
        Self::vet_command(cwd, args)
            .output()
            .unwrap_or_else(|error| fail(&format!("run git-vet: {error}")))
    }

    fn vet_command(cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_git-vet"));
        command
            .current_dir(cwd)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_PREFIX");
        command
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
        .unwrap_or_else(|error| fail(&format!("run git: {error}")))
}

fn bare_repo(prefix: &str) -> TestTempDir {
    let dir = require(TestTempDir::new(prefix), "create bare temp dir");
    run_git(dir.path(), ["init", "--bare", "-q"]);
    dir
}

fn path_str(path: &Path) -> &str {
    path.to_str()
        .unwrap_or_else(|| fail(&format!("path is not UTF-8: {}", path.display())))
}

fn ref_exists(cwd: &Path, ref_name: &str) -> bool {
    git_output(cwd, ["show-ref", "--verify", "--quiet", ref_name])
        .status
        .success()
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
    let document: Value = require(serde_json::from_slice(&output.stdout), "status JSON");
    require_some(
        document["files"].as_array(),
        &format!("missing files array in status JSON: {document:#?}"),
    )
    .clone()
}

fn status_json_document(repo: &TestRepo, args: &[&str]) -> Value {
    let output = repo.run_vet(args);
    assert!(
        output.status.success(),
        "status --json failed: {}",
        stderr(&output)
    );
    require(serde_json::from_slice(&output.stdout), "status JSON")
}

fn record_for<'a>(records: &'a [Value], path: &str) -> &'a Value {
    records
        .iter()
        .find(|record| record["path"] == path)
        .unwrap_or_else(|| fail(&format!("missing record for {path}: {records:#?}")))
}

#[test]
fn empty_notes_ref_means_tracked_files_are_new() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let document = status_json_document(&repo, &["status", "--json"]);
    let records = require_some(document["files"].as_array(), "files array");

    assert_eq!(document["channel"], "default");
    let record = record_for(records, "a.txt");
    assert_eq!(record["state"], "new");
    assert!(record["baseline"].is_null());
    assert!(record["last_vetted_at"].is_null());
    assert!(record["vetted_by"].is_null());
}

#[test]
fn mark_makes_a_file_vetted() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let mark = repo.run_vet(&["mark", "a.txt"]);
    assert!(mark.status.success(), "mark failed: {}", stderr(&mark));
    assert_eq!(stderr(&mark), "");

    let records = status_json(&repo);
    let record = record_for(&records, "a.txt");
    assert_eq!(record["state"], "vetted");
    assert_eq!(record["vetted_by"]["name"], "Reviewer");
    assert_eq!(record["vetted_by"]["email"], "reviewer@example.com");
    assert!(!record["last_vetted_at"].is_null());

    let diff = repo.run_vet(&["diff", "a.txt"]);
    assert!(diff.status.success(), "diff failed: {}", stderr(&diff));
    assert!(stdout(&diff).contains("a.txt is up to date"));
}

#[test]
fn mark_dirty_path_fails_noninteractive_without_allow_dirty() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    repo.write("a.txt", "hello from the dirty working tree\n");

    let mark = repo.run_vet(&["mark", "a.txt"]);
    assert_eq!(mark.status.code(), Some(2));
    assert_eq!(stdout(&mark), "");
    let stderr = stderr(&mark);
    assert!(
        stderr.contains("uncommitted changes relative to HEAD"),
        "{stderr}"
    );
    assert!(stderr.contains("a.txt"), "{stderr}");
    assert!(stderr.contains("--allow-dirty"), "{stderr}");
    assert!(stderr.contains("HEAD:<path> bytes"), "{stderr}");

    let records = status_json(&repo);
    let record = record_for(&records, "a.txt");
    assert_eq!(record["state"], "new");
}

#[test]
fn mark_allow_dirty_marks_head_blob_and_warns_without_prompting() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    repo.write("a.txt", "hello from the dirty working tree\n");

    let mark = repo.run_vet(&["mark", "--allow-dirty", "a.txt"]);
    assert!(mark.status.success(), "mark failed: {}", stderr(&mark));
    assert!(stdout(&mark).contains("marked a.txt"));
    let stderr = stderr(&mark);
    assert!(
        stderr.contains("uncommitted changes relative to HEAD"),
        "{stderr}"
    );
    assert!(stderr.contains("a.txt"), "{stderr}");
    assert!(!stderr.contains("Proceed with the committed HEAD version"));

    let records = status_json(&repo);
    let record = record_for(&records, "a.txt");
    assert_eq!(record["state"], "vetted");

    repo.commit_all("commit dirty working-tree content");

    let records = status_json(&repo);
    let record = record_for(&records, "a.txt");
    assert_eq!(record["state"], "stale");
    assert!(!record["baseline"].is_null());
}

#[test]
fn unmark_makes_a_vetted_file_new() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());

    let unmark = repo.run_vet(&["unmark", "a.txt"]);
    assert!(
        unmark.status.success(),
        "unmark failed: {}",
        stderr(&unmark)
    );
    assert!(stdout(&unmark).contains("unmarked a.txt"));
    assert!(stderr(&unmark).contains("blob-keyed"));

    let records = status_json(&repo);
    let record = record_for(&records, "a.txt");
    assert_eq!(record["state"], "new");
    assert!(record["baseline"].is_null());
    assert!(record["last_vetted_at"].is_null());
    assert!(record["vetted_by"].is_null());

    let note = git_output(
        repo.path(),
        ["notes", "--ref=vet/default", "show", "HEAD:a.txt"],
    );
    assert!(!note.status.success());
}

#[test]
fn unmark_makes_a_vetted_file_stale_when_an_older_blob_was_reviewed() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());
    repo.write("a.txt", "hello\nworld\n");
    repo.commit_all("edit");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());

    let unmark = repo.run_vet(&["unmark", "a.txt"]);
    assert!(
        unmark.status.success(),
        "unmark failed: {}",
        stderr(&unmark)
    );

    let records = status_json(&repo);
    let record = record_for(&records, "a.txt");
    assert_eq!(record["state"], "stale");
    assert!(!record["baseline"].is_null());
    assert_eq!(record["vetted_by"]["name"], "Reviewer");
    assert_eq!(record["vetted_by"]["email"], "reviewer@example.com");
}

#[test]
fn mark_requires_git_config_user_name() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    run_git(repo.path(), ["config", "unset", "user.name"]);

    let mark = repo.run_vet_without_user_config(&["mark", "a.txt"]);
    assert_eq!(mark.status.code(), Some(2));
    assert!(stderr(&mark).contains("missing git config user.name"));
}

#[test]
fn mark_requires_git_config_user_email() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    run_git(repo.path(), ["config", "unset", "user.email"]);

    let mark = repo.run_vet_without_user_config(&["mark", "a.txt"]);
    assert_eq!(mark.status.code(), Some(2));
    assert!(stderr(&mark).contains("missing git config user.email"));
}

#[test]
fn mark_rejects_empty_git_config_user_name() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    run_git(repo.path(), ["config", "user.name", ""]);

    let mark = repo.run_vet(&["mark", "a.txt"]);
    assert_eq!(mark.status.code(), Some(2));
    assert!(stderr(&mark).contains("missing git config user.name"));
}

#[test]
fn mark_rejects_empty_git_config_user_email() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    run_git(repo.path(), ["config", "user.email", ""]);

    let mark = repo.run_vet(&["mark", "a.txt"]);
    assert_eq!(mark.status.code(), Some(2));
    assert!(stderr(&mark).contains("missing git config user.email"));
}

#[test]
fn mark_writes_default_channel_git_notes_without_mutating_git_config() {
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
    assert!(note.contains("\"vetted_by\":{"), "{note}");
    assert!(note.contains("\"name\":\"Reviewer\""), "{note}");
    assert!(
        note.contains("\"email\":\"reviewer@example.com\""),
        "{note}"
    );
    assert!(note.contains("\"path\":\"a.txt\""), "{note}");

    let strategy = git_output(
        repo.path(),
        ["config", "--local", "--get", "notes.mergeStrategy"],
    );
    assert_eq!(strategy.status.code(), Some(1));
    assert_eq!(stdout(&strategy), "");
}

#[test]
fn status_reads_default_channel_git_notes() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    let head = assert_git_success(git_output(repo.path(), ["rev-parse", "HEAD"]));
    let head = stdout(&head).trim().to_owned();
    let note = format!(
        "{{\"vetted_at\":\"2026-06-06T00:00:00Z\",\"vetted_by\":{{\"name\":\"Reviewer\",\"email\":\"reviewer@example.com\"}},\"commit\":\"{head}\",\"path\":\"a.txt\"}}"
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
    assert_eq!(record["vetted_by"]["name"], "Reviewer");
    assert_eq!(record["vetted_by"]["email"], "reviewer@example.com");
    assert_eq!(record["last_vetted_at"], "2026-06-06T00:00:00Z");
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
    let security_records = require_some(security_document["files"].as_array(), "files array");
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
        stdout(&security_note).contains("\"email\":\"reviewer@example.com\""),
        "{}",
        stdout(&security_note)
    );
}

#[test]
fn unmark_channels_are_independent() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());
    assert!(
        repo.run_vet(&["--channel", "security", "mark", "a.txt"])
            .status
            .success()
    );

    let unmark_default = repo.run_vet(&["unmark", "a.txt"]);
    assert!(
        unmark_default.status.success(),
        "default unmark failed: {}",
        stderr(&unmark_default)
    );

    let default_records = status_json(&repo);
    let default_record = record_for(&default_records, "a.txt");
    assert_eq!(default_record["state"], "new");

    let security_records =
        status_json_with_args(&repo, &["status", "--json", "--channel", "security"]);
    let security_record = record_for(&security_records, "a.txt");
    assert_eq!(security_record["state"], "vetted");
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
fn invalid_channel_names_are_rejected_by_git_check_ref_format() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    // Channel validation intentionally delegates exact refname compatibility to
    // `git check-ref-format refs/notes/vet/<channel>`.
    for channel in [
        "bad..channel",
        ".hidden",
        "foo.lock",
        "foo//bar",
        "foo bar",
        "foo@{bar",
        "foo:bar",
        "foo*bar",
    ] {
        let output = repo.run_vet(&["status", "--channel", channel]);
        assert_eq!(output.status.code(), Some(2), "channel {channel:?}");
        let stderr = stderr(&output);
        assert!(stderr.contains("invalid review channel"), "{stderr}");
        assert!(stderr.contains("git check-ref-format"), "{stderr}");
    }
}

#[test]
fn valid_channel_names_can_contain_slashes() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let document = status_json_document(&repo, &["status", "--json", "--channel", "team/security"]);
    assert_eq!(document["channel"], "team/security");
}

#[test]
fn unmark_affects_all_identical_content_paths_in_the_channel() {
    let repo = TestRepo::new();
    repo.write("a.txt", "same\n");
    repo.write("b.txt", "same\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());

    let records = status_json(&repo);
    assert_eq!(record_for(&records, "a.txt")["state"], "vetted");
    assert_eq!(record_for(&records, "b.txt")["state"], "vetted");

    let unmark = repo.run_vet(&["unmark", "b.txt"]);
    assert!(
        unmark.status.success(),
        "unmark failed: {}",
        stderr(&unmark)
    );
    assert!(stderr(&unmark).contains("sharing the same current content"));

    let records = status_json(&repo);
    assert_eq!(record_for(&records, "a.txt")["state"], "new");
    assert_eq!(record_for(&records, "b.txt")["state"], "new");
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
    assert_eq!(record["vetted_by"]["name"], "Reviewer");
    assert_eq!(record["vetted_by"]["email"], "reviewer@example.com");

    let security_records =
        status_json_with_args(&repo, &["status", "--json", "--channel", "security"]);
    let security_record = record_for(&security_records, "a.txt");
    assert_eq!(security_record["state"], "new");
    assert!(security_record["baseline"].is_null());
    assert!(security_record["vetted_by"].is_null());
}

#[test]
fn rename_and_edit_uses_git_follow_history_for_stale_baseline() {
    let repo = TestRepo::new();
    repo.write(
        "old.txt",
        "line-1\nline-2\nline-3\nline-4\nline-5\nline-6\nline-7\nline-8\nline-9\nline-10\n",
    );
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "old.txt"]).status.success());

    run_git(repo.path(), ["mv", "old.txt", "new.txt"]);
    repo.write(
        "new.txt",
        "line-1\nline-2\nline-3\nline-4\nline-5 changed\nline-6\nline-7\nline-8\nline-9\nline-10\n",
    );
    repo.commit_all("rename and edit");

    let records = status_json(&repo);
    assert!(records.iter().all(|record| record["path"] != "old.txt"));
    let record = record_for(&records, "new.txt");
    assert_eq!(record["state"], "stale");
    assert!(!record["baseline"].is_null());
    assert_eq!(record["vetted_by"]["name"], "Reviewer");

    let diff = repo.run_vet(&["diff", "new.txt"]);
    assert!(diff.status.success(), "diff failed: {}", stderr(&diff));
    let diff = stdout(&diff);
    assert!(diff.contains("-line-5"), "{diff}");
    assert!(diff.contains("+line-5 changed"), "{diff}");
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
    assert!(stale_diff.contains("diff --git "), "{stale_diff}");
    assert!(stale_diff.contains("+world"), "{stale_diff}");
}

#[test]
fn diff_uses_git_diff_machinery() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());

    repo.write("a.txt", "hello\nworld\n");
    repo.commit_all("edit");
    run_git(
        repo.path(),
        ["config", "diff.external", "echo GIT-VET-EXTERNAL-DIFF"],
    );

    let diff = repo.run_vet(&["diff", "a.txt"]);
    assert!(diff.status.success(), "diff failed: {}", stderr(&diff));
    assert!(stdout(&diff).contains("GIT-VET-EXTERNAL-DIFF"));
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

    let unmark_untracked = repo.run_vet(&["unmark", "untracked.txt"]);
    assert_eq!(unmark_untracked.status.code(), Some(2));
    assert!(stderr(&unmark_untracked).contains("not tracked"));

    let missing = repo.run_vet(&["diff", "missing.txt"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(stderr(&missing).contains("not tracked"));

    let unmark_missing = repo.run_vet(&["unmark", "missing.txt"]);
    assert_eq!(unmark_missing.status.code(), Some(2));
    assert!(stderr(&unmark_missing).contains("not tracked"));
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
fn sync_pushes_selected_channel_notes_to_origin_fallback() {
    let remote = bare_repo("git-vet-remote");
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    repo.add_remote("origin", remote.path());
    repo.push_head_to("origin");

    let mark = repo.run_vet(&["--channel", "security", "mark", "a.txt"]);
    assert!(mark.status.success(), "mark failed: {}", stderr(&mark));

    let sync = repo.run_vet(&["--channel", "security", "sync"]);
    assert!(sync.status.success(), "sync failed: {}", stderr(&sync));

    assert!(ref_exists(remote.path(), "refs/notes/vet/security"));
    assert!(!ref_exists(remote.path(), "refs/notes/vet/default"));
}

#[test]
fn sync_fetches_remote_notes_into_local_channel() {
    let remote = bare_repo("git-vet-remote");
    let first = TestRepo::new();
    first.write("a.txt", "hello\n");
    first.commit_all("initial");
    first.add_remote("origin", remote.path());
    first.push_head_to("origin");
    assert!(first.run_vet(&["mark", "a.txt"]).status.success());
    assert!(first.run_vet(&["sync"]).status.success());

    let second = TestRepo::clone_from(remote.path());
    let before_sync = status_json(&second);
    assert_eq!(record_for(&before_sync, "a.txt")["state"], "new");

    let sync = second.run_vet(&["sync"]);
    assert!(sync.status.success(), "sync failed: {}", stderr(&sync));

    let after_sync = status_json(&second);
    assert_eq!(record_for(&after_sync, "a.txt")["state"], "vetted");
}

#[test]
fn sync_unions_concurrent_note_records_without_persistent_merge_config() {
    let remote = bare_repo("git-vet-remote");
    let base = TestRepo::new();
    base.write("a.txt", "hello\n");
    base.commit_all("initial");
    base.add_remote("origin", remote.path());
    base.push_head_to("origin");

    let first = TestRepo::clone_from(remote.path());
    let second = TestRepo::clone_from(remote.path());
    run_git(
        second.path(),
        ["config", "user.email", "second@example.com"],
    );
    run_git(second.path(), ["config", "user.name", "Second Reviewer"]);

    assert!(first.run_vet(&["mark", "a.txt"]).status.success());
    assert!(second.run_vet(&["mark", "a.txt"]).status.success());
    assert!(first.run_vet(&["sync"]).status.success());
    let second_sync = second.run_vet(&["sync"]);
    assert!(
        second_sync.status.success(),
        "second sync failed: {}",
        stderr(&second_sync)
    );
    let first_sync_again = first.run_vet(&["sync"]);
    assert!(
        first_sync_again.status.success(),
        "first sync failed: {}",
        stderr(&first_sync_again)
    );

    let note = assert_git_success(git_output(
        first.path(),
        ["notes", "--ref=vet/default", "show", "HEAD:a.txt"],
    ));
    let note = stdout(&note);
    assert!(note.contains("reviewer@example.com"), "{note}");
    assert!(note.contains("second@example.com"), "{note}");

    let strategy = git_output(
        first.path(),
        ["config", "--local", "--get", "notes.mergeStrategy"],
    );
    assert_eq!(strategy.status.code(), Some(1));
}

#[test]
fn sync_remote_selection_uses_explicit_then_config_then_origin_without_branch_upstream() {
    let origin = bare_repo("git-vet-origin");
    let upstream = bare_repo("git-vet-upstream");
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    repo.add_remote("origin", origin.path());
    repo.add_remote("upstream", upstream.path());
    repo.push_head_to("origin");
    repo.push_head_to_and_set_upstream("upstream");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());

    let sync = repo.run_vet(&["sync"]);
    assert!(sync.status.success(), "sync failed: {}", stderr(&sync));
    assert!(ref_exists(origin.path(), "refs/notes/vet/default"));
    assert!(!ref_exists(upstream.path(), "refs/notes/vet/default"));

    run_git(repo.path(), ["config", "vet.syncRemote", "upstream"]);
    let sync = repo.run_vet(&["sync"]);
    assert!(sync.status.success(), "sync failed: {}", stderr(&sync));
    assert!(ref_exists(upstream.path(), "refs/notes/vet/default"));

    assert!(
        repo.run_vet(&["--channel", "explicit", "mark", "a.txt"])
            .status
            .success()
    );
    let explicit = repo.run_vet(&["--channel", "explicit", "sync", "--remote", "origin"]);
    assert!(
        explicit.status.success(),
        "explicit sync failed: {}",
        stderr(&explicit)
    );
    assert!(ref_exists(origin.path(), "refs/notes/vet/explicit"));
    assert!(!ref_exists(upstream.path(), "refs/notes/vet/explicit"));
}

#[test]
fn sync_without_selected_remote_exits_two_with_diagnostic() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let sync = repo.run_vet(&["sync"]);
    assert_eq!(sync.status.code(), Some(2));
    let stderr = stderr(&sync);
    assert!(stderr.contains("no remote selected"), "{stderr}");
    assert!(stderr.contains("--remote"), "{stderr}");
    assert!(stderr.contains("vet.syncRemote"), "{stderr}");
    assert!(stderr.contains("origin"), "{stderr}");
}

#[test]
fn git_dispatch_finds_git_vet_on_path() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_git-vet"));
    let binary_dir = require_some(binary.parent(), "binary parent");
    let path = env::join_paths(
        std::iter::once(binary_dir.to_path_buf()).chain(env::split_paths(&require_some(
            env::var_os("PATH"),
            "PATH is set",
        ))),
    )
    .unwrap_or_else(|error| fail(&format!("join PATH: {error}")));

    let output = Command::new("git")
        .current_dir(repo.path())
        .args(["vet", "status", "--check"])
        .env("PATH", path)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_PREFIX")
        .output()
        .unwrap_or_else(|error| fail(&format!("run git vet: {error}")));

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("a.txt"));
}

#[test]
fn relative_paths_are_resolved_from_subdirectories() {
    let repo = TestRepo::new();
    repo.write("nested/file.txt", "hello\n");
    repo.commit_all("initial");

    let mark = TestRepo::run_vet_in(&repo.path().join("nested"), &["mark", "file.txt"]);
    assert!(mark.status.success(), "mark failed: {}", stderr(&mark));

    let records = status_json(&repo);
    let record = record_for(&records, "nested/file.txt");
    assert_eq!(record["state"], "vetted");
}
