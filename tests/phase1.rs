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

fn ref_target(cwd: &Path, ref_name: &str) -> String {
    let output = assert_git_success(git_output(cwd, ["rev-parse", "--verify", ref_name]));
    stdout(&output).trim().to_owned()
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
fn human_status_is_backlog_first_and_hides_vetted_by_default() {
    let repo = TestRepo::new();
    repo.write("a.txt", "reviewed\n");
    repo.write("b.txt", "reviewed then changed\n");
    repo.write("c.txt", "never reviewed\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt", "b.txt"]).status.success());
    repo.write("b.txt", "reviewed then changed\nagain\n");
    repo.commit_all("edit b");

    let status = repo.run_vet(&["status"]);
    assert!(
        status.status.success(),
        "status failed: {}",
        stderr(&status)
    );
    let output = stdout(&status);

    assert!(output.contains("git vet · channel default"), "{output}");
    assert!(output.contains("1/3 vetted"), "{output}");
    assert!(
        output.contains("2 files need review: 1 new, 1 stale"),
        "{output}"
    );
    assert!(output.contains("New — never reviewed:"), "{output}");
    assert!(output.contains("✗ c.txt"), "{output}");
    assert!(
        output.contains("Stale — changed since last review:"),
        "{output}"
    );
    assert!(output.contains("~ b.txt"), "{output}");
    assert!(output.contains("last reviewed"), "{output}");
    assert!(output.contains("Reviewer"), "{output}");
    assert!(output.contains("1 vetted file hidden"), "{output}");
    assert!(!output.contains("✓ a.txt"), "{output}");
    assert!(output.contains("git vet diff <path>"), "{output}");
    assert!(output.contains("git vet mark <path>"), "{output}");
}

#[test]
fn human_status_all_shows_vetted_files_after_backlog() {
    let repo = TestRepo::new();
    repo.write("a.txt", "reviewed\n");
    repo.write("b.txt", "never reviewed\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());

    let status = repo.run_vet(&["status", "--all"]);
    assert!(
        status.status.success(),
        "status failed: {}",
        stderr(&status)
    );
    let output = stdout(&status);

    assert!(
        output.contains("1 file needs review: 1 new, 0 stale"),
        "{output}"
    );
    assert!(output.contains("✗ b.txt"), "{output}");
    assert!(output.contains("Vetted:"), "{output}");
    assert!(output.contains("✓ a.txt"), "{output}");
    assert!(output.contains("reviewed"), "{output}");
    assert!(!output.contains("hidden"), "{output}");
}

#[test]
fn status_check_reports_review_state_with_gate_summary() {
    let repo = TestRepo::new();
    repo.write("a.txt", "reviewed\n");
    repo.write("b.txt", "reviewed then changed\n");
    repo.write("c.txt", "never reviewed\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt", "b.txt"]).status.success());
    repo.write("b.txt", "reviewed then changed\nagain\n");
    repo.commit_all("edit b");

    let check = repo.run_vet(&["status", "--check"]);
    assert_eq!(check.status.code(), Some(1));
    let output = stdout(&check);

    assert!(
        output.contains("Review gate failed for channel default."),
        "{output}"
    );
    assert!(output.contains("2 files need review:"), "{output}");
    assert!(output.contains("stale  b.txt"), "{output}");
    assert!(output.contains("new    c.txt"), "{output}");
    assert!(!output.contains("a.txt"), "{output}");
}

#[test]
fn status_accepts_file_and_directory_pathspecs() {
    let repo = TestRepo::new();
    repo.write("src/a.rs", "reviewed\n");
    repo.write("src/b.rs", "not reviewed\n");
    repo.write("docs/readme.md", "not reviewed\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "src/a.rs"]).status.success());

    let src_records = status_json_with_args(&repo, &["status", "--json", "src"]);
    assert_eq!(src_records.len(), 2);
    assert_eq!(record_for(&src_records, "src/a.rs")["state"], "vetted");
    assert_eq!(record_for(&src_records, "src/b.rs")["state"], "new");
    assert!(
        !src_records
            .iter()
            .any(|record| record["path"] == "docs/readme.md")
    );

    let file_records = status_json_with_args(&repo, &["status", "--json", "src/a.rs"]);
    assert_eq!(file_records.len(), 1);
    assert_eq!(file_records[0]["path"], "src/a.rs");
    assert_eq!(file_records[0]["state"], "vetted");
}

#[test]
fn status_pathspec_check_gates_only_the_selected_scope() {
    let repo = TestRepo::new();
    repo.write("src/a.rs", "reviewed\n");
    repo.write("src/b.rs", "not reviewed\n");
    repo.write("docs/readme.md", "not reviewed\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "src/a.rs"]).status.success());

    let reviewed_file = repo.run_vet(&["status", "--check", "src/a.rs"]);
    assert!(
        reviewed_file.status.success(),
        "scoped check failed: {}",
        stderr(&reviewed_file)
    );
    assert!(stdout(&reviewed_file).contains("Review gate passed"));

    let src_dir = repo.run_vet(&["status", "--check", "src"]);
    assert_eq!(src_dir.status.code(), Some(1));
    let output = stdout(&src_dir);
    assert!(output.contains("new    src/b.rs"), "{output}");
    assert!(!output.contains("docs/readme.md"), "{output}");
}

#[test]
fn status_pathspec_human_output_shows_vetted_file_result() {
    let repo = TestRepo::new();
    repo.write("src/a.rs", "reviewed\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "src/a.rs"]).status.success());

    let status = repo.run_vet(&["status", "src/a.rs"]);
    assert!(
        status.status.success(),
        "status failed: {}",
        stderr(&status)
    );
    let output = stdout(&status);
    assert!(output.contains("All files are vetted."), "{output}");
    assert!(output.contains("Vetted:"), "{output}");
    assert!(output.contains("✓ src/a.rs"), "{output}");
}

#[test]
fn status_unmatched_pathspec_exits_two() {
    let repo = TestRepo::new();
    repo.write("a.txt", "tracked\n");
    repo.commit_all("initial");

    let status = repo.run_vet(&["status", "missing"]);
    assert_eq!(status.status.code(), Some(2));
    assert_eq!(stdout(&status), "");
    assert!(
        stderr(&status).contains("pathspec did not match any tracked files at HEAD: missing"),
        "{}",
        stderr(&status)
    );
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
fn status_workspace_treats_local_edit_to_vetted_head_as_stale() {
    let repo = TestRepo::new();
    repo.write("a.txt", "reviewed\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());

    repo.write("a.txt", "reviewed\nlocal\n");

    let head_records = status_json(&repo);
    let head_record = record_for(&head_records, "a.txt");
    assert_eq!(head_record["state"], "vetted");
    let head_blob = head_record["blob"].clone();

    let workspace_records = status_json_with_args(&repo, &["status", "--workspace", "--json"]);
    let workspace_record = record_for(&workspace_records, "a.txt");
    assert_eq!(workspace_record["state"], "stale");
    assert_eq!(workspace_record["baseline"], head_blob);
    assert_ne!(workspace_record["blob"], head_blob);

    let check = repo.run_vet(&["status", "--workspace", "--check"]);
    assert_eq!(check.status.code(), Some(1));
    assert!(stdout(&check).contains("stale  a.txt"));
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
fn marking_the_same_content_twice_does_not_rewrite_the_note() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let first = repo.run_vet(&["mark", "a.txt"]);
    assert!(
        first.status.success(),
        "first mark failed: {}",
        stderr(&first)
    );
    let notes_ref = "refs/notes/vet/default";
    let first_target = ref_target(repo.path(), notes_ref);

    let second = repo.run_vet(&["mark", "a.txt"]);
    assert!(
        second.status.success(),
        "second mark failed: {}",
        stderr(&second)
    );
    assert_eq!(ref_target(repo.path(), notes_ref), first_target);
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
fn vet_channel_config_selects_default_review_channel() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    run_git(repo.path(), ["config", "vet.channel", "security"]);

    let document = status_json_document(&repo, &["status", "--json"]);
    assert_eq!(document["channel"], "security");

    let mark = repo.run_vet(&["mark", "a.txt"]);
    assert!(mark.status.success(), "mark failed: {}", stderr(&mark));

    let security_note = git_output(
        repo.path(),
        ["notes", "--ref=vet/security", "show", "HEAD:a.txt"],
    );
    assert!(
        security_note.status.success(),
        "security note missing: {}",
        stderr(&security_note)
    );
    assert!(!ref_exists(repo.path(), "refs/notes/vet/default"));
}

#[test]
fn explicit_channel_overrides_vet_channel_config() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    run_git(repo.path(), ["config", "vet.channel", "security"]);

    let document = status_json_document(&repo, &["status", "--json", "--channel", "default"]);
    assert_eq!(document["channel"], "default");

    let mark = repo.run_vet(&["mark", "a.txt", "--channel", "default"]);
    assert!(mark.status.success(), "mark failed: {}", stderr(&mark));

    assert!(ref_exists(repo.path(), "refs/notes/vet/default"));
    assert!(!ref_exists(repo.path(), "refs/notes/vet/security"));
}

#[test]
fn empty_vet_channel_config_is_rejected() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    run_git(repo.path(), ["config", "vet.channel", ""]);

    let output = repo.run_vet(&["status"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr(&output);
    assert!(stderr.contains("invalid review channel"), "{stderr}");
    assert!(stderr.contains("vet.channel"), "{stderr}");
    assert!(
        stderr.contains("channel name must not be empty"),
        "{stderr}"
    );
}

#[test]
fn invalid_vet_channel_config_is_rejected_unless_explicit_channel_is_set() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    run_git(repo.path(), ["config", "vet.channel", "bad..channel"]);

    let configured = repo.run_vet(&["status"]);
    assert_eq!(configured.status.code(), Some(2));
    let configured_stderr = stderr(&configured);
    assert!(
        configured_stderr.contains("invalid review channel"),
        "{configured_stderr}"
    );
    assert!(
        configured_stderr.contains("vet.channel"),
        "{configured_stderr}"
    );
    assert!(
        configured_stderr.contains("valid Git ref name"),
        "{configured_stderr}"
    );

    let explicit = repo.run_vet(&["status", "--json", "--channel", "default"]);
    assert!(
        explicit.status.success(),
        "explicit channel should override invalid config: {}",
        stderr(&explicit)
    );
    let document: Value = require(serde_json::from_slice(&explicit.stdout), "status JSON");
    assert_eq!(document["channel"], "default");
}

#[test]
fn invalid_channel_names_are_rejected_by_git_ref_validation() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    // Channel validation checks exact refname compatibility for
    // `refs/notes/vet/<channel>`.
    for channel in [
        "bad..channel",
        ".hidden",
        "foo.lock",
        "foo bar",
        "foo@{bar",
        "foo:bar",
        "foo*bar",
    ] {
        let output = repo.run_vet(&["status", "--channel", channel]);
        assert_eq!(output.status.code(), Some(2), "channel {channel:?}");
        let stderr = stderr(&output);
        assert!(stderr.contains("invalid review channel"), "{stderr}");
        assert!(stderr.contains("valid Git ref name"), "{stderr}");
    }
}

#[test]
fn nested_channel_names_are_rejected() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let output = repo.run_vet(&["status", "--channel", "team/security"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr(&output);
    assert!(stderr.contains("invalid review channel"), "{stderr}");
    assert!(stderr.contains("must not contain '/'"), "{stderr}");
}

#[test]
fn channel_list_shows_local_channels_in_stable_human_and_json_forms() {
    let repo = TestRepo::new();

    let empty = repo.run_vet(&["channel", "list"]);
    assert!(empty.status.success(), "list failed: {}", stderr(&empty));
    assert_eq!(stdout(&empty), "No local review channels found.\n");

    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());
    let copy = repo.run_vet(&["channel", "copy", "default", "release"]);
    assert!(copy.status.success(), "copy failed: {}", stderr(&copy));

    let human = repo.run_vet(&["channel", "list"]);
    assert!(human.status.success(), "list failed: {}", stderr(&human));
    assert_eq!(stdout(&human), "Review channels:\n  default\n  release\n");

    let json = repo.run_vet(&["channel", "list", "--json"]);
    assert!(
        json.status.success(),
        "list --json failed: {}",
        stderr(&json)
    );
    let document: Value = require(serde_json::from_slice(&json.stdout), "channel list JSON");
    assert_eq!(
        document,
        serde_json::json!({
            "channels": [
                {"name": "default", "ref": "refs/notes/vet/default"},
                {"name": "release", "ref": "refs/notes/vet/release"}
            ]
        })
    );

    let scoped = repo.run_vet(&["channel", "list", "--channel", "default"]);
    assert_eq!(scoped.status.code(), Some(2));
    assert!(stderr(&scoped).contains("--channel cannot be used with `channel list`"));
}

#[test]
fn channel_remove_deletes_only_the_selected_local_channel() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());
    assert!(
        repo.run_vet(&["channel", "copy", "default", "release"])
            .status
            .success()
    );

    let removed = repo.run_vet(&["channel", "remove", "release", "--force"]);
    assert!(
        removed.status.success(),
        "remove failed: {}",
        stderr(&removed)
    );
    assert_eq!(stdout(&removed), "removed review channel \"release\"\n");
    assert!(!ref_exists(repo.path(), "refs/notes/vet/release"));
    assert!(ref_exists(repo.path(), "refs/notes/vet/default"));
    assert_eq!(record_for(&status_json(&repo), "a.txt")["state"], "vetted");

    let missing = repo.run_vet(&["channel", "remove", "release", "--force"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(
        stderr(&missing).contains("review channel \"release\" does not exist locally"),
        "{}",
        stderr(&missing)
    );
}

#[test]
fn channel_remove_requires_force_non_interactively() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());

    let output = repo.run_vet(&["channel", "remove", "default"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("non-interactive channel removal requires --force"),
        "{}",
        stderr(&output)
    );
    assert!(ref_exists(repo.path(), "refs/notes/vet/default"));
}

#[test]
fn channel_remove_rejects_global_channel_option() {
    let repo = TestRepo::new();
    let output = repo.run_vet(&["--channel", "default", "channel", "remove", "default"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("--channel cannot be used with `channel remove`"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn channel_copy_creates_an_exact_independent_review_state_snapshot() {
    let repo = TestRepo::new();
    repo.write("a.txt", "first\n");
    repo.write("b.txt", "second\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt", "b.txt"]).status.success());

    repo.write("a.txt", "changed\n");
    repo.commit_all("change a");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());

    let source_ref = "refs/notes/vet/default";
    let destination_ref = "refs/notes/vet/release";
    let source_target = ref_target(repo.path(), source_ref);
    let source_notes = assert_git_success(git_output(
        repo.path(),
        ["notes", "--ref=vet/default", "list"],
    ));

    let copy = repo.run_vet(&["channel", "copy", "default", "release"]);
    assert!(copy.status.success(), "copy failed: {}", stderr(&copy));
    assert!(
        stdout(&copy).contains("copied review notes from channel \"default\" to \"release\""),
        "{}",
        stdout(&copy)
    );
    assert_eq!(ref_target(repo.path(), destination_ref), source_target);

    let destination_notes = assert_git_success(git_output(
        repo.path(),
        ["notes", "--ref=vet/release", "list"],
    ));
    assert_eq!(destination_notes.stdout, source_notes.stdout);
    let release_records =
        status_json_with_args(&repo, &["status", "--json", "--channel", "release"]);
    assert_eq!(record_for(&release_records, "a.txt")["state"], "vetted");
    assert_eq!(record_for(&release_records, "b.txt")["state"], "vetted");

    let unmark = repo.run_vet(&["unmark", "a.txt", "--channel", "release"]);
    assert!(
        unmark.status.success(),
        "destination unmark failed: {}",
        stderr(&unmark)
    );
    assert_ne!(
        ref_target(repo.path(), source_ref),
        ref_target(repo.path(), destination_ref)
    );
    assert_eq!(record_for(&status_json(&repo), "a.txt")["state"], "vetted");
    let release_records =
        status_json_with_args(&repo, &["status", "--json", "--channel", "release"]);
    assert_ne!(record_for(&release_records, "a.txt")["state"], "vetted");
}

#[test]
fn channel_move_atomically_rehomes_local_review_notes() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());

    let source_target = ref_target(repo.path(), "refs/notes/vet/default");
    let moved = repo.run_vet(&["channel", "move", "default", "user-name"]);
    assert!(moved.status.success(), "move failed: {}", stderr(&moved));
    assert!(
        stdout(&moved).contains("moved review notes from channel \"default\" to \"user-name\""),
        "{}",
        stdout(&moved)
    );
    assert!(
        stderr(&moved).contains("channel selection was not changed"),
        "{}",
        stderr(&moved)
    );
    assert!(!ref_exists(repo.path(), "refs/notes/vet/default"));
    assert_eq!(
        ref_target(repo.path(), "refs/notes/vet/user-name"),
        source_target
    );

    assert_eq!(record_for(&status_json(&repo), "a.txt")["state"], "new");
    let moved_records =
        status_json_with_args(&repo, &["status", "--json", "--channel", "user-name"]);
    assert_eq!(record_for(&moved_records, "a.txt")["state"], "vetted");
}

#[test]
fn channel_transfer_rejects_missing_source_and_identical_endpoints() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let missing = repo.run_vet(&["channel", "copy", "missing", "destination"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(
        stderr(&missing).contains("source channel \"missing\" has no local review notes"),
        "{}",
        stderr(&missing)
    );
    assert!(!ref_exists(repo.path(), "refs/notes/vet/destination"));

    let identical = repo.run_vet(&["channel", "move", "same", "same"]);
    assert_eq!(identical.status.code(), Some(2));
    assert!(
        stderr(&identical).contains("source and destination channels must differ"),
        "{}",
        stderr(&identical)
    );
}

#[test]
fn channel_transfer_never_merges_or_replaces_an_existing_destination() {
    let repo = TestRepo::new();
    repo.write("a.txt", "first\n");
    repo.write("b.txt", "second\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());
    assert!(
        repo.run_vet(&["mark", "b.txt", "--channel", "release"])
            .status
            .success()
    );

    let source_before = ref_target(repo.path(), "refs/notes/vet/default");
    let destination_before = ref_target(repo.path(), "refs/notes/vet/release");

    for operation in ["copy", "move"] {
        let output = repo.run_vet(&["channel", operation, "default", "release"]);
        assert_eq!(output.status.code(), Some(2), "{operation}");
        assert!(
            stderr(&output)
                .contains("destination channel \"release\" already has local review notes"),
            "{}",
            stderr(&output)
        );
        assert_eq!(
            ref_target(repo.path(), "refs/notes/vet/default"),
            source_before
        );
        assert_eq!(
            ref_target(repo.path(), "refs/notes/vet/release"),
            destination_before
        );
    }
}

#[test]
fn channel_transfer_rejects_global_channel_and_ignores_configured_channel() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());

    let selected = repo.run_vet(&[
        "--channel",
        "default",
        "channel",
        "copy",
        "default",
        "release",
    ]);
    assert_eq!(selected.status.code(), Some(2));
    assert!(
        stderr(&selected).contains("--channel cannot be used with `channel copy`"),
        "{}",
        stderr(&selected)
    );
    assert!(!ref_exists(repo.path(), "refs/notes/vet/release"));

    run_git(repo.path(), ["config", "vet.channel", "bad..channel"]);
    let explicit = repo.run_vet(&["channel", "copy", "default", "release"]);
    assert!(
        explicit.status.success(),
        "invalid unrelated config blocked transfer: {}",
        stderr(&explicit)
    );
}

#[test]
fn channel_transfer_preserves_policy_files_and_does_not_require_user_identity() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());
    repo.write(".vetignore.default", "generated/**\n");

    run_git(repo.path(), ["config", "--unset", "user.name"]);
    run_git(repo.path(), ["config", "--unset", "user.email"]);
    run_git(repo.path(), ["config", "user.useConfigOnly", "true"]);

    let moved = repo.run_vet_without_user_config(&["channel", "move", "default", "user-name"]);
    assert!(
        moved.status.success(),
        "move without identity failed: {}",
        stderr(&moved)
    );
    assert!(
        stderr(&moved).contains(".vetignore.default was not moved"),
        "{}",
        stderr(&moved)
    );
    assert_eq!(
        require(
            fs::read_to_string(repo.path().join(".vetignore.default")),
            "read source policy",
        ),
        "generated/**\n"
    );
    assert!(!repo.path().join(".vetignore.user-name").exists());
}

#[test]
fn channel_transfer_reports_invalid_endpoint_roles() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");

    let source = repo.run_vet(&["channel", "copy", "bad/source", "valid"]);
    assert_eq!(source.status.code(), Some(2));
    assert!(
        stderr(&source).contains("SOURCE argument"),
        "{}",
        stderr(&source)
    );

    let destination = repo.run_vet(&["channel", "copy", "valid", "bad/destination"]);
    assert_eq!(destination.status.code(), Some(2));
    assert!(
        stderr(&destination).contains("DESTINATION argument"),
        "{}",
        stderr(&destination)
    );
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
fn diff_workspace_compares_latest_vetted_content_to_dirty_worktree() {
    let repo = TestRepo::new();
    repo.write("dir/a.txt", "hello\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "dir/a.txt"]).status.success());

    repo.write("dir/a.txt", "hello\nlocal\n");

    let head_diff = repo.run_vet(&["diff", "dir/a.txt"]);
    assert!(
        head_diff.status.success(),
        "HEAD diff failed: {}",
        stderr(&head_diff)
    );
    assert!(stdout(&head_diff).contains("dir/a.txt is up to date"));

    let workspace_diff = repo.run_vet(&["diff", "--workspace", "dir/a.txt"]);
    assert!(
        workspace_diff.status.success(),
        "workspace diff failed: {}",
        stderr(&workspace_diff)
    );
    let workspace_diff = stdout(&workspace_diff);
    assert!(workspace_diff.contains("diff --git "), "{workspace_diff}");
    assert!(workspace_diff.contains("+local"), "{workspace_diff}");
}

#[test]
fn diff_workspace_for_stale_file_includes_committed_and_uncommitted_changes() {
    let repo = TestRepo::new();
    repo.write("a.txt", "base\n");
    repo.commit_all("initial");
    assert!(repo.run_vet(&["mark", "a.txt"]).status.success());

    repo.write("a.txt", "base\nhead\n");
    repo.commit_all("edit");
    repo.write("a.txt", "base\nhead\nlocal\n");

    let head_diff = repo.run_vet(&["diff", "a.txt"]);
    assert!(
        head_diff.status.success(),
        "diff failed: {}",
        stderr(&head_diff)
    );
    let head_diff = stdout(&head_diff);
    assert!(head_diff.contains("+head"), "{head_diff}");
    assert!(!head_diff.contains("+local"), "{head_diff}");

    let workspace_diff = repo.run_vet(&["diff", "--workspace", "a.txt"]);
    assert!(
        workspace_diff.status.success(),
        "workspace diff failed: {}",
        stderr(&workspace_diff)
    );
    let workspace_diff = stdout(&workspace_diff);
    assert!(workspace_diff.contains("+head"), "{workspace_diff}");
    assert!(workspace_diff.contains("+local"), "{workspace_diff}");
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
fn per_channel_vetignore_applies_only_to_selected_channel() {
    let repo = TestRepo::new();
    repo.write("kept.txt", "kept\n");
    repo.write("security-ignored.txt", "ignored in security\n");
    repo.write(".vetignore.security", "security-ignored.txt\n");
    repo.commit_all("initial");

    let default_records = status_json(&repo);
    assert!(
        default_records
            .iter()
            .any(|record| record["path"] == "security-ignored.txt")
    );

    let security_records =
        status_json_with_args(&repo, &["status", "--json", "--channel", "security"]);
    assert!(
        security_records
            .iter()
            .all(|record| record["path"] != "security-ignored.txt")
    );
}

#[test]
fn per_channel_vetignore_can_reinclude_global_ignored_paths() {
    let repo = TestRepo::new();
    repo.write("generated/security-critical.rs", "critical\n");
    repo.write("generated/other.rs", "other\n");
    repo.write(".vetignore", "generated/**\n");
    repo.write(".vetignore.security", "!generated/security-critical.rs\n");
    repo.commit_all("initial");

    let default_records = status_json(&repo);
    assert!(default_records.iter().all(|record| {
        !record["path"]
            .as_str()
            .is_some_and(|path| path.starts_with("generated/"))
    }));

    let security_records =
        status_json_with_args(&repo, &["status", "--json", "--channel", "security"]);
    assert!(
        security_records
            .iter()
            .any(|record| record["path"] == "generated/security-critical.rs")
    );
    assert!(
        security_records
            .iter()
            .all(|record| record["path"] != "generated/other.rs")
    );
}

#[test]
fn per_channel_vetignore_uses_dot_suffixed_channel_file() {
    let repo = TestRepo::new();
    repo.write("team-only.txt", "ignored in team channel\n");
    repo.write(".vetignore.team", "team-only.txt\n");
    repo.commit_all("initial");

    let team_records = status_json_with_args(&repo, &["status", "--json", "--channel", "team"]);
    assert!(
        team_records
            .iter()
            .all(|record| record["path"] != "team-only.txt")
    );

    let security_records =
        status_json_with_args(&repo, &["status", "--json", "--channel", "security"]);
    assert!(
        security_records
            .iter()
            .any(|record| record["path"] == "team-only.txt")
    );
}

#[test]
fn explicit_path_commands_can_target_ignored_paths() {
    let repo = TestRepo::new();
    repo.write("ignored.txt", "ignored\n");
    repo.write(".vetignore.security", "ignored.txt\n");
    repo.commit_all("initial");

    let mark = repo.run_vet(&["--channel", "security", "mark", "ignored.txt"]);
    assert!(mark.status.success(), "mark failed: {}", stderr(&mark));

    let diff = repo.run_vet(&["--channel", "security", "diff", "ignored.txt"]);
    assert!(diff.status.success(), "diff failed: {}", stderr(&diff));
    assert!(stdout(&diff).contains("ignored.txt is up to date"));
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
    let sync_stderr = stderr(&sync);
    assert!(
        sync_stderr.contains("✓ Pushed review notes for channel security via origin"),
        "{sync_stderr}"
    );
    assert!(
        sync_stderr.contains("result: pushed local notes; remote ref did not exist"),
        "{sync_stderr}"
    );

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
    let sync_stderr = stderr(&sync);
    assert!(
        sync_stderr.contains("✓ Synced review notes for channel default via origin"),
        "{sync_stderr}"
    );
    assert!(
        sync_stderr.contains("result: fetched, merged, and pushed"),
        "{sync_stderr}"
    );

    let after_sync = status_json(&second);
    assert_eq!(record_for(&after_sync, "a.txt")["state"], "vetted");
    assert!(!ref_exists(second.path(), "refs/notes/vet-sync/default"));
}

#[test]
fn sync_prints_noop_summary_when_no_notes_exist() {
    let remote = bare_repo("git-vet-remote");
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    repo.add_remote("origin", remote.path());
    repo.push_head_to("origin");

    let sync = repo.run_vet(&["sync"]);
    assert!(sync.status.success(), "sync failed: {}", stderr(&sync));
    let sync_stderr = stderr(&sync);
    assert!(
        sync_stderr.contains("✓ No review notes to sync for channel default via origin"),
        "{sync_stderr}"
    );
    assert!(
        sync_stderr.contains("result: nothing to sync"),
        "{sync_stderr}"
    );
    assert!(!ref_exists(remote.path(), "refs/notes/vet/default"));
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
fn sync_rejects_a_configured_remote_without_a_fetch_url() {
    let repo = TestRepo::new();
    repo.write("a.txt", "hello\n");
    repo.commit_all("initial");
    run_git(repo.path(), ["config", "remote.upstream.prune", "true"]);

    let sync = repo.run_vet(&["sync", "--remote", "upstream"]);
    assert_eq!(sync.status.code(), Some(2));
    let stderr = stderr(&sync);
    assert!(stderr.contains("upstream"), "{stderr}");
    assert!(
        stderr.contains("does not exist or has no fetch URL"),
        "{stderr}"
    );
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
