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
    fs::create_dir_all(&change.join("specs/ledger")).unwrap();
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
    assert!(ledger.contains("sh -c exit 3"));
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

