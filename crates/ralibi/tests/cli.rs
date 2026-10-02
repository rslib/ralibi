use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ralibi-cli-{}-{}", std::process::id(), tag));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn setup(tag: &str) -> PathBuf {
    let repo = scratch(tag);
    run(&repo, ["git", "init", "-q"]);
    run(&repo, ["git", "commit", "-q", "-m", "a", "--allow-empty"]);
    let change = repo.join("openspec").join("changes").join("alpha");
    fs::create_dir_all(change.join("specs/ledger")).unwrap();
    fs::write(change.join("tasks.md"),
        "# Tasks\n\n- [ ] 1.1 first; verify true\n- [ ] 1.2 second; verify false\n- [ ] 1.3 third; verify ok\n").unwrap();
    fs::write(change.join("specs/ledger/delta.md"), "## ADDED Requirements\n").unwrap();
    repo
}

fn run<'a>(dir: &Path, args: impl IntoIterator<Item = &'a str>) -> (i32, String, String) {
    let mut it = args.into_iter();
    let out = Command::new(it.next().unwrap())
        .args(it)
        .current_dir(dir)
        .output()
        .unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

fn ralibi(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ralibi"));
    // scrub attribution env so "neither" cases are not poisoned by the outer shell
    c.args(args).current_dir(dir).env_remove("RALIBI_AGENT");
    let out = c.output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

const MIXED_TASKS: &str = "# Tasks\n\n- [ ] 1.1 one; verify a\n- [ ] 1.2 two; verify b\n- [ ] 1.3 three; verify c\n";

fn mixed_repo(tag: &str) -> PathBuf {
    let repo = setup(tag);
    let change = repo.join("openspec/changes/alpha");
    fs::write(change.join("tasks.md"), MIXED_TASKS).unwrap();
    // 1.1 proved now; 1.2 proved at a HEAD that is not an ancestor; 1.3 never proved
    run(&repo, ["git", "checkout", "-q", "-b", "old"]);
    run(&repo, ["git", "commit", "-q", "-m", "divergent", "--allow-empty"]);
    let old = {
        let o = Command::new("git").arg("rev-parse").arg("HEAD").current_dir(&repo).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    run(&repo, ["git", "checkout", "-q", "-"]);
    // write a ledger by hand with a stale 1.2 record and no 1.3
    fs::write(change.join("alibi.md"), format!(
        "# Alibi\n\nchange: alpha\n\n## 1.1 one\n\n- run: {} | exit 0 | 0.1s | head {} | machine test\n  `true`\n\n## 1.2 two\n\n- run: {} | exit 0 | 0.1s | head {} | machine test\n  `true`\n",
        "2030-01-01T00:00:00.000+00:00",
        {
            let o = Command::new("git").arg("rev-parse").arg("HEAD").current_dir(&repo).output().unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        },
        "2030-01-01T00:00:00.000+00:00",
        old,
    )).unwrap();
    repo
}

#[test]
fn run_success_and_failure_passthrough_and_unknown_task() {
    let repo = setup("run");
    // success
    let (code, out, _) = ralibi(&repo, &["run", "1.1", "--", "true"]);
    assert_eq!(code, 0);
    assert!(out.contains("recorded: 1.1"), "out: {out}");
    // failure status passthrough + record kept
    let (code, out, _) = ralibi(&repo, &["run", "1.2", "--", "sh", "-c", "exit 3"]);
    assert_eq!(code, 3);
    assert!(out.contains("exit 3"), "out: {out}");
    let ledger = fs::read_to_string(repo.join("openspec/changes/alpha/alibi.md")).unwrap();
    assert!(ledger.contains("exit 3"));
    assert!(ledger.contains("sh -c 'exit 3'"));
    // unknown task: error record, appends nothing
    let before = fs::read_to_string(repo.join("openspec/changes/alpha/alibi.md")).unwrap();
    let (code, _, err) = ralibi(&repo, &["run", "--toon", "9.9", "--", "true"]);
    assert_ne!(code, 0);
    assert!(err.contains("code=unknown-task"), "err: {err}");
    assert!(err.contains("fix="));
    let after = fs::read_to_string(repo.join("openspec/changes/alpha/alibi.md")).unwrap();
    assert_eq!(before, after, "unknown task must append nothing");
}

#[test]
fn status_mixed_states() {
    let repo = mixed_repo("status");
    let (code, out, _) = ralibi(&repo, &["status", "--toon"]);
    assert_eq!(code, 0);
    assert!(out.contains("task=1.1 state=proved"));
    assert!(out.contains("task=1.2 state=stale reason=head-moved"));
    assert!(out.contains("task=1.3 state=missing"));
    let (_, human, _) = ralibi(&repo, &["status"]);
    assert!(human.contains("proved  1.1") && human.contains("stale   1.2") && human.contains("missing 1.3"));
}

#[test]
fn gate_missing_blocks_all_proved_passes_stale_as_fail() {
    // missing blocks
    let repo = mixed_repo("gate");
    let (code, out, _) = ralibi(&repo, &["gate", "--toon"]);
    assert_eq!(code, 1);
    assert!(out.contains("task=1.3 state=missing"));
    // prove the missing task -> gate passes (stale is only a warning by default)
    let (code, _, _) = ralibi(&repo, &["run", "1.3", "--", "true"]);
    assert_eq!(code, 0);
    let (code, out, _) = ralibi(&repo, &["gate", "--toon"]);
    assert_eq!(code, 0, "stale alone must not fail the gate. out: {out}");
    assert!(out.contains("state=pass"));
    // stale-as-fail refuses
    let (code, out, _) = ralibi(&repo, &["gate", "--toon", "--stale-as-fail"]);
    assert_eq!(code, 1);
    assert!(out.contains("state=stale reason=head-moved"));
    // all fresh: amend nothing, just re-prove the stale task
    let (code, _, _) = ralibi(&repo, &["run", "1.2", "--", "true"]);
    assert_eq!(code, 0);
    let (code, out, _) = ralibi(&repo, &["gate", "--toon", "--stale-as-fail"]);
    assert_eq!(code, 0, "all fresh must pass. out: {out}");
}

#[test]
fn show_records_in_order_and_empty_ledger() {
    let repo = setup("show");
    // empty ledger
    let (code, out, _) = ralibi(&repo, &["show", "--toon"]);
    assert_eq!(code, 0);
    assert!(out.contains("records=0"), "out: {out}");
    // two records, in run order
    ralibi(&repo, &["run", "1.1", "--", "true"]);
    ralibi(&repo, &["run", "1.3", "--", "false"]);
    let (_, out, _) = ralibi(&repo, &["show", "--toon"]);
    let pos_11 = out.find("task=1.1").unwrap();
    let pos_13 = out.find("task=1.3").unwrap();
    assert!(pos_11 < pos_13, "records must print in run order. out: {out}");
    assert!(out.contains("command=true") && out.contains("command=false"));
}

#[test]
fn failed_latest_run_is_not_proof() {
    let repo = setup("failed");
    ralibi(&repo, &["run", "1.1", "--", "true"]);
    ralibi(&repo, &["run", "1.2", "--", "false"]);
    ralibi(&repo, &["run", "1.3", "--", "true"]);
    ralibi(&repo, &["run", "1.3", "--", "false"]);
    let (_, out, _) = ralibi(&repo, &["status", "--toon"]);
    assert!(out.contains("task=1.1 state=proved"), "out: {out}");
    assert!(out.contains("task=1.2 state=failed exit=1"), "out: {out}");
    // a later failing run takes back an earlier pass
    assert!(out.contains("task=1.3 state=failed exit=1"), "out: {out}");
    let (code, out, _) = ralibi(&repo, &["gate", "--toon"]);
    assert_eq!(code, 1, "out: {out}");
    assert!(out.contains("task=1.2 state=failed") && !out.contains("state=pass"), "out: {out}");
    let (code, human, _) = ralibi(&repo, &["gate"]);
    assert_eq!(code, 1);
    assert!(human.contains("gate: failed 1.2 (exit 1)"), "human: {human}");
    // re-proving with a passing run clears it
    ralibi(&repo, &["run", "1.2", "--", "true"]);
    ralibi(&repo, &["run", "1.3", "--", "true"]);
    let (code, out, _) = ralibi(&repo, &["gate", "--toon"]);
    assert_eq!(code, 0, "out: {out}");
}

#[test]
fn recorded_command_keeps_argument_boundaries() {
    let repo = setup("quoting");
    ralibi(&repo, &["run", "1.1", "--", "sh", "-c", "echo \"a b\" | grep -q 'a b'"]);
    let (_, out, _) = ralibi(&repo, &["show", "--toon"]);
    assert!(out.contains(r#"command=sh -c 'echo "a b" | grep -q '\''a b'\''"#), "out: {out}");
    // the recorded line replays as the same command
    let ledger = fs::read_to_string(repo.join("openspec/changes/alpha/alibi.md")).unwrap();
    let line = ledger.lines().find_map(|l| l.trim().strip_prefix('`')?.strip_suffix('`')).unwrap();
    let (code, _, _) = run(&repo, ["sh", "-c", line]);
    assert_eq!(code, 0, "replay of: {line}");
}

#[test]
fn closed_pipe_does_not_panic() {
    let repo = setup("pipe");
    // far more output than a pipe buffer holds, so the writer hits the closed pipe
    let record = "\n## 1.1\n\n- run: 2030-01-01T00:00:00.000+00:00 | exit 0 | 0.1s | head abc | machine test\n  `true`\n";
    fs::write(repo.join("openspec/changes/alpha/alibi.md"), format!("# Alibi\n\nchange: alpha\n{}", record.repeat(5000))).unwrap();
    let bin = env!("CARGO_BIN_EXE_ralibi");
    let (code, _, err) = run(&repo, ["sh", "-c", &format!("{bin} show --toon | head -c 1 >/dev/null; sleep 0.1")]);
    assert_eq!(code, 0);
    assert!(!err.contains("panicked"), "err: {err}");
}

#[test]
fn run_records_output_tail_and_dirty_tree() {
    let repo = setup("context");
    // the setup commit is empty, so the openspec tree is untracked; it must not count as dirty
    let (code, out, _) = ralibi(&repo, &["run", "--toon", "1.1", "--", "sh", "-c", "echo to-out; echo to-err >&2; exit 0"]);
    assert_eq!(code, 0);
    assert!(out.contains("to-out") && out.contains("to-err"), "output is passed through: {out}");
    assert!(!out.contains("dirty="), "clean tree: {out}");
    fs::write(repo.join("src.txt"), "edit\n").unwrap();
    let (_, out, _) = ralibi(&repo, &["run", "--toon", "1.2", "--", "true"]);
    assert!(out.contains("dirty=1"), "out: {out}");
    // ralibi never writes to git: nothing is staged
    let (code, _, _) = run(&repo, ["git", "diff", "--cached", "--quiet"]);
    assert_eq!(code, 0, "the index must stay untouched");
    let (_, out, _) = ralibi(&repo, &["show", "--toon"]);
    assert!(out.contains("task=1.1 state=latest") && out.contains("dirty=0 output=to-err"), "out: {out}");
    assert!(out.contains("task=1.2 state=latest") && out.contains("dirty=1"), "out: {out}");
    let (_, human, _) = ralibi(&repo, &["show"]);
    assert!(human.contains("  > to-out\n  > to-err"), "human: {human}");
    // editing the tested file after the run makes the proof stale
    fs::write(repo.join("src.txt"), "edit again\n").unwrap();
    let (_, out, _) = ralibi(&repo, &["status", "--toon"]);
    assert!(out.contains("task=1.2 state=stale reason=worktree-changed"), "out: {out}");
    assert!(out.contains("task=1.1 state=proved"), "out: {out}");
}

#[test]
fn edited_verify_clause_makes_proof_stale() {
    let repo = setup("task-edit");
    ralibi(&repo, &["run", "1.1", "--", "true"]);
    let tasks = repo.join("openspec/changes/alpha/tasks.md");
    // checking the box is not an edit of the task
    let md = fs::read_to_string(&tasks).unwrap().replace("- [ ] 1.1", "- [x] 1.1");
    fs::write(&tasks, &md).unwrap();
    let (_, out, _) = ralibi(&repo, &["status", "--toon"]);
    assert!(out.contains("task=1.1 state=proved"), "out: {out}");
    fs::write(&tasks, md.replace("verify true", "verify cargo test")).unwrap();
    let (_, out, _) = ralibi(&repo, &["status", "--toon"]);
    assert!(out.contains("task=1.1 state=stale reason=task-changed"), "out: {out}");
}

#[test]
fn unnumbered_task_blocks_gate() {
    let repo = setup("unnumbered");
    let tasks = repo.join("openspec/changes/alpha/tasks.md");
    fs::write(&tasks, "* [ ] 1.1 star bullet; verify true\n1. [ ] write the docs\n").unwrap();
    ralibi(&repo, &["run", "1.1", "--", "true"]);
    let (_, out, _) = ralibi(&repo, &["status", "--toon"]);
    assert!(out.contains("task=1.1 state=proved"), "out: {out}");
    assert!(out.contains("state=unnumbered line=write the docs"), "out: {out}");
    let (code, out, _) = ralibi(&repo, &["gate", "--toon"]);
    assert_eq!(code, 1, "out: {out}");
    assert!(out.contains("state=unnumbered") && !out.contains("state=pass"), "out: {out}");
}

#[test]
fn run_records_even_when_its_reader_closes() {
    let repo = setup("run-pipe");
    let bin = env!("CARGO_BIN_EXE_ralibi");
    let (code, _, err) = run(&repo, ["sh", "-c", &format!("{bin} run 1.1 -- sh -c 'seq 1 200000; exit 3' | head -c 1 >/dev/null; sleep 0.2")]);
    assert_eq!(code, 0);
    assert!(!err.contains("panicked"), "err: {err}");
    let (_, out, _) = ralibi(&repo, &["show", "--toon"]);
    assert!(out.contains("task=1.1") && out.contains("exit=3") && out.contains("output=200000"), "out: {out}");
}

#[test]
fn show_marks_superseded_records() {
    let repo = setup("superseded");
    ralibi(&repo, &[
        "run", "--toon", "1.1", "--", "sh", "-c", "echo wrong-command-proof",
    ]);
    ralibi(&repo, &["run", "--toon", "1.1", "--", "true"]);
    ralibi(&repo, &["run", "--toon", "1.2", "--", "true"]);
    let (_, out, _) = ralibi(&repo, &[
        "show", "--toon",
        "--change", "alpha",
    ]);
    // the first 1.1 run is superseded by the second; 1.2's single run is latest
    let lines: Vec<&str> = out.lines().filter(|l| l.contains("type=record")).collect();
    assert_eq!(lines.len(), 3, "out: {out}");
    assert!(lines[0].contains("task=1.1 state=superseded"), "out: {out}");
    assert!(lines[1].contains("task=1.1 state=latest"), "out: {out}");
    assert!(lines[2].contains("task=1.2 state=latest"), "out: {out}");
    // human output carries the same markers
    let (_, human, _) = ralibi(&repo, &["show"]);
    assert!(human.contains("1.1  superseded"), "human: {human}");
    assert!(human.contains("1.1  latest"), "human: {human}");
    assert!(human.contains("1.2  latest"), "human: {human}");
}

#[test]
fn archived_change_resolves_and_gate_works() {
    let repo = setup("archived");
    ralibi(&repo, &["run", "--toon", "1.1", "--", "true"]);
    ralibi(&repo, &["run", "--toon", "1.2", "--", "true"]);
    ralibi(&repo, &["run", "--toon", "1.3", "--", "true"]);
    // archive the change the way openspec does: move it to archive/YYYY-MM-DD-<name>
    let changes = repo.join("openspec/changes");
    fs::create_dir_all(changes.join("archive")).unwrap();
    fs::rename(changes.join("alpha"), changes.join("archive").join("2026-10-01-alpha")).unwrap();
    // flag resolves into the archive
    let (code, out, _) = ralibi(&repo, &["show", "--toon", "--change", "alpha"]);
    assert_eq!(code, 0, "out: {out}");
    assert!(out.contains("task=1.1"), "out: {out}");
    // auto-resolution still ignores the archive
    let (code, _, err) = ralibi(&repo, &["status", "--toon"]);
    assert_ne!(code, 0);
    assert!(err.contains("code=no-change"), "err: {err}");
    // gate on the archived change: all tasks have records, so it passes
    let (code, out, _) = ralibi(&repo, &["gate", "--toon", "--change", "alpha"]);
    assert_eq!(code, 0, "out: {out}");
    // unknown name errors mentioning the archive
    let (_, _, err) = ralibi(&repo, &["status", "--toon", "--change", "nope"]);
    assert!(err.contains("archived"), "err: {err}");
}

#[test]
fn agent_attribution_flag_env_neither() {
    let repo = setup("agent");
    // flag recorded
    let (code, out, _) = ralibi(&repo, &["run", "--toon", "--agent", "verify", "1.1", "--", "true"]);
    assert_eq!(code, 0);
    assert!(out.contains("agent=verify"), "out: {out}");
    // flag wins over env
    let (code, out, _) = ralibi_env(&repo, &["run", "--toon", "--agent", "pi", "1.2", "--", "true"], Some("reader"));
    assert_eq!(code, 0);
    assert!(out.contains("agent=pi") && !out.contains("agent=reader"), "out: {out}");
    // env fallback
    let (code, out, _) = ralibi_env(&repo, &["run", "--toon", "1.3", "--", "true"], Some("reader"));
    assert_eq!(code, 0);
    assert!(out.contains("agent=reader"), "out: {out}");
    // neither: no agent in run output, and show carries an empty agent field
    let (code, out, _) = ralibi(&repo, &["run", "--toon", "1.1", "--", "true"]);
    assert_eq!(code, 0);
    assert!(!out.contains("agent="), "out: {out}");
    let (_, show, _) = ralibi(&repo, &["show", "--toon"]);
    assert!(show.contains("agent=verify"), "show: {show}");
}

fn ralibi_env(dir: &Path, args: &[&str], agent: Option<&str>) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ralibi"));
    c.args(args).current_dir(dir);
    if let Some(a) = agent {
        c.env("RALIBI_AGENT", a);
    } else {
        c.env_remove("RALIBI_AGENT");
    }
    let out = c.output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn install_writes_skips_and_runs_anywhere() {
    // scratch has no openspec tree above it: install must still work
    let work = scratch("install-cwd");
    let home = scratch("install-home");
    fs::create_dir_all(home.join(".pi/agent/skills")).unwrap();
    // install: pi written, claude refused (different real content), codex skipped (parent absent)
    fs::create_dir_all(home.join(".claude/skills/ralibi")).unwrap();
    fs::write(home.join(".claude/skills/ralibi/SKILL.md"), "user's own notes\n").unwrap();
    let (code, out, _) = ralibi_env2(&work, &["install", "--toon"], &home, None);
    assert_eq!(code, 1, "refused entry makes the run nonzero. out: {out}");
    assert!(out.contains("harness=pi state=written"), "out: {out}");
    assert!(out.contains("harness=claude state=refused reason=different-content"), "out: {out}");
    assert!(out.contains("harness=codex state=skipped reason=missing-parent"), "out: {out}");
    let installed = fs::read_to_string(home.join(".pi/agent/skills/ralibi/SKILL.md")).unwrap();
    assert!(installed.contains("name: ralibi"), "installed content must be the embedded skill");
    // re-install: identical content is unchanged, refused stays refused, still nonzero
    let (code, out, _) = ralibi_env2(&work, &["install", "--toon"], &home, None);
    assert_eq!(code, 1);
    assert!(out.contains("harness=pi state=unchanged"), "out: {out}");
    assert!(out.contains("harness=claude state=refused"), "out: {out}");
}

#[test]
fn install_uninstall_and_symlink_replacement() {
    let work = scratch("install-un");
    let home = scratch("install-un-home");
    fs::create_dir_all(home.join(".pi/agent/skills")).unwrap();
    fs::create_dir_all(home.join(".claude/skills")).unwrap();
    // a dev symlink to identical content is replaced on install, removed on uninstall
    let repo_like = scratch("repo-like");
    fs::create_dir_all(repo_like.join("skills/ralibi")).unwrap();
    let embedded = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/ralibi/SKILL.md")).unwrap();
    fs::write(repo_like.join("skills/ralibi/SKILL.md"), &embedded).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(repo_like.join("skills/ralibi"), home.join(".claude/skills/ralibi")).unwrap();
    let (_, out, _) = ralibi_env2(&work, &["install", "--toon"], &home, None);
    assert!(out.contains("harness=claude state=replaced"), "out: {out}");
    assert!(!home.join(".claude/skills/ralibi").is_symlink());
    // uninstall: identical removed, modified left
    fs::write(home.join(".pi/agent/skills/ralibi/SKILL.md"), "edited by hand\n").unwrap();
    let (code, out, _) = ralibi_env2(&work, &["install", "--toon", "--uninstall"], &home, None);
    assert_eq!(code, 0, "out: {out}");
    assert!(out.contains("harness=pi state=left reason=modified-content"), "out: {out}");
    assert!(out.contains("harness=claude state=removed"), "out: {out}");
    assert!(home.join(".pi/agent/skills/ralibi/SKILL.md").is_file());
    assert!(!home.join(".claude/skills/ralibi").exists());
}



fn ralibi_env2(dir: &Path, args: &[&str], home: &Path, _unused: Option<&Path>) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ralibi"));
    c.args(args).current_dir(dir).env("HOME", home).env_remove("RALIBI_AGENT");
    let out = c.output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn toon_error_records_on_failure_modes() {
    // no openspec tree: error record with code and fix
    let repo = scratch("no-tree");
    let (code, _, err) = ralibi(&repo, &["status", "--toon"]);
    assert_eq!(code, 1);
    assert!(err.contains("type=error code=no-openspec"), "err: {err}");
    assert!(err.contains("fix="));
    // unknown change flag
    let repo = setup("unknown-change");
    let (code, _, err) = ralibi(&repo, &["status", "--toon", "--change", "nope"]);
    assert_eq!(code, 1);
    assert!(err.contains("code=unknown-change"), "err: {err}");
    // ambiguity
    let other = repo.join("openspec/changes/beta");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("tasks.md"), "- [ ] 1.1 x; verify y\n").unwrap();
    let (code, _, err) = ralibi(&repo, &["status", "--toon"]);
    assert_eq!(code, 1);
    assert!(err.contains("code=ambiguous-change"), "err: {err}");
    assert!(err.contains("fix=pass --change <id>"));
    // no in-flight change at all
    let empty = scratch("no-change");
    fs::create_dir_all(empty.join("openspec/changes/archive")).unwrap();
    let (code, _, err) = ralibi(&empty, &["status", "--toon"]);
    assert_eq!(code, 1);
    assert!(err.contains("type=error code=no-change"), "err: {err}");
    assert!(err.contains("fix="));
}

