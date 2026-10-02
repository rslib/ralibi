//! Ledger model and change resolution for ralibi. No terminal output here.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as ProcCommand;
use std::time::{SystemTime, UNIX_EPOCH};

/// Error record per AXI: code, message, fix. The CLI renders these; the core never prints.
#[derive(Debug, Clone, PartialEq)]
pub struct CoreError {
    pub code: String,
    pub message: String,
    pub fix: String,
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} (fix: {})", self.code, self.message, self.fix)
    }
}

impl std::error::Error for CoreError {}

pub type Result<T> = std::result::Result<T, CoreError>;

fn err(code: &str, message: impl Into<String>, fix: impl Into<String>) -> CoreError {
    CoreError { code: code.into(), message: message.into(), fix: fix.into() }
}

/// A task line from a change's tasks.md, e.g. `- [ ] 1.2 Implement the ledger record model ...`.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: String,
    pub title: String,
}

/// One proof run recorded in a change's alibi.md.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub task: String,
    pub command: String,
    pub exit: i32,
    pub duration_ms: u128,
    pub head: String,
    pub date: String,
    pub machine: String,
    pub agent: Option<String>,
    /// Fingerprint of the task's line (title and verify clause) at run time.
    pub task_hash: Option<String>,
    /// Files outside `openspec/` that differed from HEAD at run time.
    pub dirty: Option<Dirty>,
    /// Last non-empty lines of the command's combined stdout and stderr.
    pub tail: Vec<String>,
}

/// Uncommitted files at run time: paths relative to the openspec root and a content fingerprint.
#[derive(Debug, Clone, PartialEq)]
pub struct Dirty {
    pub paths: Vec<String>,
    pub digest: String,
}

impl Record {
    pub fn now_date() -> String {
        chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    pub fn machine_name() -> String {
        hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_else(|_| "unknown".into())
    }

    /// Milliseconds since the epoch, for duration measurement.
    pub fn now_ms() -> u128 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
    }
}

/// Extract `1.1`-style task ids and titles from tasks.md content.
pub fn parse_tasks(md: &str) -> Vec<Task> {
    md.lines()
        .filter_map(task_description)
        .filter_map(numbered)
        .map(|(id, title)| Task { id: id.to_string(), title: title.to_string() })
        .collect()
}

/// Task lines that carry no `1.1`-style id. ralibi cannot prove them, so the gate refuses them.
pub fn unnumbered_tasks(md: &str) -> Vec<String> {
    md.lines()
        .filter_map(task_description)
        .filter(|desc| numbered(desc).is_none())
        .map(str::to_string)
        .collect()
}

fn numbered(desc: &str) -> Option<(&str, &str)> {
    let (id, title) = desc.split_once(char::is_whitespace).unwrap_or((desc, ""));
    let id = id.trim_end_matches('.');
    let ok = id.split('.').count() >= 2 && id.split('.').all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()));
    ok.then(|| (id, title.trim()))
}

/// The description of a checkbox line, accepting the bullets OpenSpec accepts:
/// `-`, `*`, `+`, `1.` or `1)`, then a box holding at most one non-space mark.
fn task_description(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let rest = if (1..=9).contains(&digits) && matches!(line.as_bytes().get(digits), Some(b'.' | b')')) {
        &line[digits + 1..]
    } else {
        line.strip_prefix(['-', '*', '+'])?
    };
    let (mark, after) = rest.trim_start().strip_prefix('[')?.split_once(']')?;
    if mark.trim().chars().count() > 1 || after.starts_with(['(', '[']) {
        return None;
    }
    Some(after.trim())
}

/// Read the task list of a change directory (holds tasks.md): numbered tasks and unnumbered lines.
pub fn read_tasks(change_dir: &Path) -> Result<(Vec<Task>, Vec<String>)> {
    let path = change_dir.join("tasks.md");
    let md = fs::read_to_string(&path)
        .map_err(|_| err("no-tasks", format!("no tasks.md at {}", path.display()), "run from inside the change's repository"))?;
    Ok((parse_tasks(&md), unnumbered_tasks(&md)))
}

/// Nearest openspec root at or above `start`: the first ancestor dir holding `openspec/changes`.
pub fn find_openspec_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("openspec").join("changes").is_dir() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Changes under `root/openspec/changes` that are in flight (not archived, hold tasks.md).
pub fn in_flight_changes(root: &Path) -> Vec<String> {
    let mut ids: Vec<String> = fs::read_dir(root.join("openspec").join("changes"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter(|e| e.file_name() != "archive")
        .filter(|e| e.path().join("tasks.md").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    ids.sort();
    ids
}

/// The resolved change directory: `changes/<id>` in flight, or its archived location.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedChange {
    pub id: String,
    pub dir: PathBuf,
    pub archived: bool,
}

/// Resolve the change to act on: the `--change` flag wins, otherwise exactly one in-flight change.
/// A flagged id that names no in-flight change falls back to the archive, where OpenSpec stores it
/// as `archive/<id>` or `archive/YYYY-MM-DD-<id>`; in-flight beats archived so a reopened change
/// shadows its archived self. Auto-resolution without the flag considers in-flight changes only.
pub fn resolve_change(root: &Path, flag: Option<&str>) -> Result<ResolvedChange> {
    let changes = root.join("openspec").join("changes");
    if let Some(id) = flag {
        let in_flight = changes.join(id);
        if in_flight.join("tasks.md").is_file() {
            return Ok(ResolvedChange { id: id.to_string(), dir: in_flight, archived: false });
        }
        if let Some(dir) = find_archived_change(&changes.join("archive"), id) {
            return Ok(ResolvedChange { id: id.to_string(), dir, archived: true });
        }
        return Err(err(
            "unknown-change",
            format!("no in-flight or archived change named '{id}'"),
            format!("pick one of: {}", in_flight_changes(root).join(", ")),
        ));
    }
    match in_flight_changes(root).as_slice() {
        [] => Err(err("no-change", "no in-flight changes under the nearest openspec root", "cd into the repository holding the change, or create one")),
        [one] => Ok(ResolvedChange { id: one.clone(), dir: changes.join(one), archived: false }),
        many => Err(err(
            "ambiguous-change",
            format!("{} in-flight changes: {}", many.len(), many.join(", ")),
            "pass --change <id>",
        )),
    }
}

/// `archive/<id>` when it exists, else the newest `archive/YYYY-MM-DD-<id>`.
fn find_archived_change(archive: &Path, id: &str) -> Option<PathBuf> {
    let exact = archive.join(id);
    if exact.join("tasks.md").is_file() {
        return Some(exact);
    }
    let mut dated: Vec<PathBuf> = fs::read_dir(archive)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_str().and_then(strip_date_prefix) == Some(id))
        .map(|e| e.path())
        .filter(|p| p.join("tasks.md").is_file())
        .collect();
    dated.sort();
    dated.pop()
}

/// The name after a `YYYY-MM-DD-` prefix, the prefix OpenSpec adds when it archives a change.
fn strip_date_prefix(name: &str) -> Option<&str> {
    let (date, rest) = (name.get(..11)?, name.get(11..)?);
    let shape = date.bytes().enumerate().all(|(i, c)| if matches!(i, 4 | 7 | 10) { c == b'-' } else { c.is_ascii_digit() });
    (shape && !rest.is_empty()).then_some(rest)
}

/// Join argv into one line that a POSIX shell (sh, bash, zsh) parses back into the same argv.
/// Arguments with control characters use `$'...'` quoting so the ledger line stays one line.
pub fn shell_join(argv: &[String]) -> String {
    argv.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")
}

fn shell_quote(arg: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c);
    if !arg.is_empty() && arg.chars().all(plain) {
        return arg.to_string();
    }
    if !arg.chars().any(|c| c.is_ascii_control()) {
        return format!("'{}'", arg.replace('\'', "'\\''"));
    }
    let mut out = String::from("$'");
    for c in arg.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_ascii_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// The ledger file of a change: openspec/changes/<id>/alibi.md.
pub fn ledger_path(change_dir: &Path) -> PathBuf {
    change_dir.join("alibi.md")
}

/// Current git HEAD of `repo`, or an empty string outside a repository. Read-only.
pub fn read_head(repo: &Path) -> String {
    ProcCommand::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(repo)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// One appended section per run keeps the file strictly append-only (inserting a run under a
/// task's earlier section mid-file would rewrite existing bytes). Optional fields render only
/// when present, so a record without them keeps the exact shape older ralibi versions wrote.
fn render_section(rec: &Record) -> String {
    let mut run = format!(
        "- run: {} | exit {} | {:.1}s | head {} | machine {}",
        rec.date,
        rec.exit,
        rec.duration_ms as f64 / 1000.0,
        rec.head,
        rec.machine
    );
    if let Some(a) = &rec.agent {
        run += &format!(" | agent {a}");
    }
    if let Some(t) = &rec.task_hash {
        run += &format!(" | task {t}");
    }
    if let Some(d) = &rec.dirty {
        run += &format!(" | dirty {} {}", d.paths.len(), d.digest);
    }
    let mut out = format!("\n## {}\n\n{run}\n  `{}`\n", rec.task, rec.command);
    for path in rec.dirty.iter().flat_map(|d| &d.paths) {
        out += &format!("  dirty: {}\n", percent_encode(path));
    }
    for line in &rec.tail {
        out += &format!("  > {line}\n");
    }
    out
}

/// Escape `%` and control characters so a path fits on one ledger line.
fn percent_encode(s: &str) -> String {
    s.chars().map(|c| if c == '%' || c.is_control() { format!("%{:02X}", c as u32) } else { c.to_string() }).collect()
}

fn percent_decode(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out += &rest[..i];
        match rest.get(i + 1..i + 3).and_then(|h| u32::from_str_radix(h, 16).ok()).and_then(char::from_u32) {
            Some(c) => {
                out.push(c);
                rest = &rest[i + 3..];
            }
            None => {
                out.push('%');
                rest = &rest[i + 1..];
            }
        }
    }
    out + rest
}

/// FNV-1a 64-bit (public domain, http://www.isthe.com/chongo/tech/comp/fnv/), as 16 hex digits.
/// It detects careless edits, not tampering.
pub fn fingerprint(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    format!("{hash:016x}")
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = ProcCommand::new("git").args(args).current_dir(dir).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Files under `root`, outside `openspec/`, whose content differs from HEAD, untracked ones included.
/// None outside a git repository or before the first commit. Read-only.
pub fn dirty_paths(root: &Path) -> Option<Vec<String>> {
    let scope = ["--", ".", ":(exclude)openspec"];
    let changed = git(root, &[&["diff", "--name-only", "--relative", "-z", "HEAD"][..], &scope].concat())?;
    let untracked = git(root, &[&["ls-files", "-z", "--others", "--exclude-standard"][..], &scope].concat())?;
    let mut paths: Vec<String> = changed.split('\0').chain(untracked.split('\0')).filter(|p| !p.is_empty()).map(String::from).collect();
    paths.sort();
    paths.dedup();
    Some(paths)
}

/// Fingerprint of the current content of `paths` under `root`; a missing file counts as content.
pub fn content_digest(root: &Path, paths: &[String]) -> String {
    let files: Vec<&str> = paths.iter().filter(|p| root.join(p).is_file()).map(String::as_str).collect();
    let blobs = if files.is_empty() { String::new() } else { git(root, &[&["hash-object", "--"][..], &files].concat()).unwrap_or_default() };
    let mut blobs = blobs.lines();
    let mut listing = String::new();
    for path in paths {
        let blob = if root.join(path).is_file() { blobs.next().unwrap_or("unreadable") } else { "missing" };
        listing += &format!("{path}\0{blob}\n");
    }
    fingerprint(&listing)
}

/// The uncommitted state to record with a run, or None when the tree is clean or not in git.
pub fn capture_dirty(root: &Path) -> Option<Dirty> {
    let paths = dirty_paths(root)?;
    if paths.is_empty() {
        return None;
    }
    let digest = content_digest(root, &paths);
    Some(Dirty { paths, digest })
}

/// Append one record, creating the ledger with its header on first write. Never rewrites existing bytes.
pub fn append_record(ledger: &Path, change: &str, rec: &Record) -> Result<()> {
    if let Some(parent) = ledger.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| err("io", format!("cannot create {}: {e}", parent.display()), "check permissions"))?;
    }
    let fresh = fs::metadata(ledger).map(|m| m.len()).unwrap_or(0) == 0;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(ledger)
        .map_err(|e| err("io", format!("cannot open {}: {e}", ledger.display()), "check permissions"))?;
    if fresh {
        writeln!(file, "# Alibi\n\nchange: {change}\n").map_err(io_err)?;
    }
    write!(file, "{}", render_section(rec)).map_err(io_err)?;
    Ok(())
}

fn io_err(e: std::io::Error) -> CoreError {
    err("io", format!("ledger write failed: {e}"), "check permissions")
}

/// Staleness reason for a proof run.
#[derive(Debug, Clone, PartialEq)]
pub enum StaleReason {
    HeadMoved,
    TaskChanged,
    SpecsSynced,
    WorktreeChanged,
}

impl StaleReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            StaleReason::HeadMoved => "head-moved",
            StaleReason::TaskChanged => "task-changed",
            StaleReason::SpecsSynced => "specs-synced",
            StaleReason::WorktreeChanged => "worktree-changed",
        }
    }
}

/// True when `recorded` is the current HEAD or an ancestor of it. Read-only.
pub fn head_is_ancestor(repo: &Path, recorded: &str) -> bool {
    if recorded.is_empty() {
        return false;
    }
    ProcCommand::new("git")
        .arg("merge-base")
        .arg("--is-ancestor")
        .arg(recorded)
        .arg("HEAD")
        .current_dir(repo)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Latest mtime under the change's `specs/` tree (recursively), or None when absent or unreadable.
pub fn latest_spec_mtime(change_dir: &Path) -> Option<SystemTime> {
    let specs = change_dir.join("specs");
    let mut latest: Option<SystemTime> = None;
    let mut stack = vec![specs];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(md) = fs::metadata(&path) {
                let mtime = md.modified().ok()?;
                latest = Some(latest.map_or(mtime, |l| l.max(mtime)));
            }
        }
    }
    latest
}

/// Staleness of one run, computed live (never cached) from the repo, the change dir and the
/// task as tasks.md states it now. Returns None when fresh, Some(reason) when stale.
pub fn staleness(repo: &Path, change_dir: &Path, rec: &Record, task: &Task) -> Option<StaleReason> {
    if !head_is_ancestor(repo, &rec.head) {
        return Some(StaleReason::HeadMoved);
    }
    if rec.task_hash.as_ref().is_some_and(|h| *h != fingerprint(&task.title)) {
        return Some(StaleReason::TaskChanged);
    }
    if let (Some(spec_mtime), Ok(run_at)) = (
        latest_spec_mtime(change_dir),
        chrono::DateTime::parse_from_rfc3339(&rec.date),
    ) {
        let run_sys = run_at.with_timezone(&chrono::Local).into();
        if spec_mtime > run_sys {
            return Some(StaleReason::SpecsSynced);
        }
    }
    if let Some(d) = &rec.dirty
        && content_digest(repo, &d.paths) != d.digest
    {
        return Some(StaleReason::WorktreeChanged);
    }
    None
}

/// Last `n` non-empty lines of command output, with escape sequences and control characters
/// removed and each line cut to 200 characters, ready for the ledger.
pub fn output_tail(output: &[u8], n: usize) -> Vec<String> {
    let text = String::from_utf8_lossy(output);
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|l| l.rsplit('\r').find(|seg| !seg.trim().is_empty()).unwrap_or(""))
        .map(strip_escapes)
        .map(|l| l.trim_end().chars().take(200).collect::<String>())
        .filter(|l| !l.trim().is_empty())
        .collect();
    lines.drain(..lines.len().saturating_sub(n));
    lines
}

fn strip_escapes(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI sequences end at the first byte in @..~
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
        } else if c == '\t' {
            out.push(' ');
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}

/// Index of the latest record per task: the last one in file order.
pub fn latest_per_task(records: &[Record]) -> std::collections::HashMap<String, usize> {
    records.iter().enumerate().map(|(i, r)| (r.task.clone(), i)).collect()
}

/// Parse all records from a ledger, in append order.
pub fn read_records(ledger: &Path) -> Vec<Record> {
    let Ok(md) = fs::read_to_string(ledger) else { return Vec::new() };
    let mut records: Vec<Record> = Vec::new();
    let mut task = String::new();
    let mut pending: Option<Record> = None;
    // lines after a record's command belong to it until the next section
    let mut open = false;
    for line in md.lines() {
        let trimmed = line.trim();
        if let Some(header) = trimmed.strip_prefix("## ") {
            task = header.split_whitespace().next().unwrap_or("").to_string();
            open = false;
        } else if let Some(path) = trimmed.strip_prefix("dirty: ").filter(|_| open) {
            if let Some(d) = records.last_mut().and_then(|r| r.dirty.as_mut()) {
                d.paths.push(percent_decode(path));
            }
        } else if let Some(out) = line.strip_prefix("  > ").filter(|_| open) {
            if let Some(r) = records.last_mut() {
                r.tail.push(out.to_string());
            }
        } else if let Some(run) = trimmed.strip_prefix("- run: ") {
            open = false;
            // <date> | exit <n> | <x.x>s | head <h> | machine <m>
            let fields: Vec<&str> = run.split(" | ").collect();
            if fields.len() < 5 {
                continue;
            }
            pending = Some(Record {
                task: task.clone(),
                command: String::new(),
                exit: fields[1].strip_prefix("exit ").unwrap_or("").trim().parse().unwrap_or(-1),
                duration_ms: (fields[2].trim_end_matches('s').parse::<f64>().unwrap_or(0.0) * 1000.0) as u128,
                head: fields[3].strip_prefix("head ").unwrap_or("").trim().to_string(),
                date: fields[0].trim().to_string(),
                machine: fields[4].strip_prefix("machine ").unwrap_or("").trim().to_string(),
                agent: optional_field(&fields, "agent ").map(str::to_string),
                task_hash: optional_field(&fields, "task ").map(str::to_string),
                dirty: optional_field(&fields, "dirty ")
                    .and_then(|d| d.split_once(' '))
                    .map(|(_, digest)| Dirty { paths: Vec::new(), digest: digest.to_string() }),
                tail: Vec::new(),
            });
        } else if let Some(cmd) = trimmed.strip_prefix('`').and_then(|s| s.strip_suffix('`'))
            && let Some(mut rec) = pending.take()
        {
            rec.command = cmd.to_string();
            records.push(rec);
            open = true;
        }
    }
    records
}

fn optional_field<'a>(fields: &[&'a str], key: &str) -> Option<&'a str> {
    fields.iter().skip(5).find_map(|f| f.strip_prefix(key)).map(str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ralibi-test-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample() -> Record {
        Record {
            task: "2.1".into(),
            command: "cargo test -p ralibi".into(),
            exit: 0,
            duration_ms: 4100,
            head: "1a2b3c4".into(),
            date: "2026-09-29T21:40:12+07:00".into(),
            machine: "goby".into(),
            agent: None,
            task_hash: None,
            dirty: None,
            tail: Vec::new(),
        }
    }

    #[test]
    fn round_trip() {
        let dir = scratch("round-trip");
        let ledger = ledger_path(&dir);
        append_record(&ledger, "ledger-core", &sample()).unwrap();
        let records = read_records(&ledger);
        assert_eq!(records, vec![sample()]);
    }

    #[test]
    fn first_append_creates_file() {
        let dir = scratch("create");
        let ledger = ledger_path(&dir);
        assert!(!ledger.exists());
        append_record(&ledger, "ledger-core", &sample()).unwrap();
        let md = fs::read_to_string(&ledger).unwrap();
        assert!(md.starts_with("# Alibi"));
        assert!(md.contains("change: ledger-core"));
    }

    #[test]
    fn append_preserves_earlier_records() {
        let dir = scratch("preserve");
        let ledger = ledger_path(&dir);
        append_record(&ledger, "ledger-core", &sample()).unwrap();
        let before = fs::read_to_string(&ledger).unwrap();
        let mut second = sample();
        second.task = "2.2".into();
        second.exit = 1;
        append_record(&ledger, "ledger-core", &second).unwrap();
        let after = fs::read_to_string(&ledger).unwrap();
        assert!(after.starts_with(&before), "append must not touch earlier bytes");
        assert_eq!(read_records(&ledger), vec![sample(), second]);
    }

    fn make_change(root: &Path, id: &str) {
        let dir = root.join("openspec").join("changes").join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("tasks.md"), "# Tasks\n\n- [ ] 1.1 Do a thing; verify it ran\n").unwrap();
    }

    #[test]
    fn nearest_root_walks_up() {
        let root = scratch("nearest");
        make_change(&root, "alpha");
        let nested = root.join("crates").join("ralibi");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(find_openspec_root(&nested).as_deref(), Some(root.as_path()));
        assert_eq!(find_openspec_root(&root).as_deref(), Some(root.as_path()));
        assert_eq!(find_openspec_root(&scratch("empty")), None);
    }

    #[test]
    fn flag_wins_and_unknown_flag_errors() {
        let root = scratch("flag");
        make_change(&root, "alpha");
        make_change(&root, "beta");
        assert_eq!(resolve_change(&root, Some("beta")).unwrap().id, "beta");
        let e = resolve_change(&root, Some("nope")).unwrap_err();
        assert_eq!(e.code, "unknown-change");
        assert!(e.fix.contains("alpha") && e.fix.contains("beta"));
    }

    #[test]
    fn single_change_resolves_without_flag() {
        let root = scratch("single");
        make_change(&root, "only-one");
        assert_eq!(resolve_change(&root, None).unwrap().id, "only-one");
    }

    #[test]
    fn ambiguity_error_names_fix() {
        let root = scratch("ambiguous");
        make_change(&root, "alpha");
        make_change(&root, "beta");
        let e = resolve_change(&root, None).unwrap_err();
        assert_eq!(e.code, "ambiguous-change");
        assert_eq!(e.fix, "pass --change <id>");
        assert!(e.message.contains("alpha") && e.message.contains("beta"));
    }

    #[test]
    fn archive_dir_is_not_in_flight() {
        let root = scratch("archive");
        make_change(&root, "live-one");
        let arch = root.join("openspec").join("changes").join("archive");
        fs::create_dir_all(&arch).unwrap();
        fs::write(arch.join("tasks.md"), "- [ ] 9.9 old\n").unwrap();
        assert_eq!(in_flight_changes(&root), vec!["live-one".to_string()]);
    }

    #[test]
    fn archived_change_resolves_by_flag() {
        let root = scratch("archived-flag");
        make_change(&root, "live-one");
        let arch = root.join("openspec").join("changes").join("archive");
        fs::create_dir_all(arch.join("old-one")).unwrap();
        fs::write(arch.join("old-one").join("tasks.md"), "- [x] 1.1 done; verify x\n").unwrap();
        assert_eq!(resolve_change(&root, Some("old-one")).unwrap().id, "old-one");
        // auto-resolution still ignores the archive
        assert_eq!(resolve_change(&root, None).unwrap().id, "live-one");
    }

    #[test]
    fn date_prefixed_archive_resolves_by_bare_name() {
        let root = scratch("dated-archive");
        let arch = root.join("openspec").join("changes").join("archive");
        for dir in ["2026-01-01-older", "2026-02-01-older", "2026-03-01-older-x", "2026-04-01-older"] {
            fs::create_dir_all(arch.join(dir)).unwrap();
        }
        for dir in ["2026-01-01-older", "2026-02-01-older", "2026-03-01-older-x"] {
            fs::write(arch.join(dir).join("tasks.md"), "- [x] 1.1 done; verify x\n").unwrap();
        }
        // newest dated match with a tasks.md wins; `older-x` is another change
        let resolved = resolve_change(&root, Some("older")).unwrap();
        assert_eq!(resolved.id, "older");
        assert!(resolved.dir.ends_with("archive/2026-02-01-older"));
        // the full archived name still resolves
        assert!(resolve_change(&root, Some("2026-01-01-older")).unwrap().dir.ends_with("archive/2026-01-01-older"));
        assert!(resolve_change(&root, Some("01-older")).is_err());
    }

    #[test]
    fn shell_join_round_trips_through_sh() {
        let argv: Vec<String> = ["sh", "-c", "printf '%s|' \"$@\"", "x", "plain", "a b", "it's", "", "$HOME", "*", "l1\nl2", "tab\tq'\\"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let line = shell_join(&argv);
        assert!(!line.contains('\n'));
        assert!(line.starts_with("sh -c "));
        for shell in ["sh", "bash", "zsh"] {
            let Ok(out) = ProcCommand::new(shell).arg("-c").arg(&line).output() else { continue };
            assert_eq!(String::from_utf8_lossy(&out.stdout), "plain|a b|it's||$HOME|*|l1\nl2|tab\tq'\\|", "{shell}: {line}");
        }
    }

    #[test]
    fn in_flight_shadows_archived_namesake() {
        let root = scratch("shadow");
        make_change(&root, "alpha");
        let arch = root.join("openspec").join("changes").join("archive").join("alpha");
        fs::create_dir_all(&arch).unwrap();
        fs::write(arch.join("tasks.md"), "- [x] 1.1 old; verify x\n").unwrap();
        let resolved = resolve_change(&root, Some("alpha")).unwrap();
        assert_eq!(resolved.id, "alpha");
        assert!(!resolved.archived);
        assert!(resolved.dir.ends_with("changes/alpha"));
        assert!(resolved.dir.join("tasks.md").is_file());
    }

    #[test]
    fn unknown_flag_error_mentions_archive() {
        let root = scratch("unknown-archived");
        make_change(&root, "alpha");
        let e = resolve_change(&root, Some("nope")).unwrap_err();
        assert_eq!(e.code, "unknown-change");
        assert!(e.message.contains("archived"));
    }

    #[test]
    fn round_trip_with_agent() {
        let dir = scratch("agent-rt");
        let ledger = ledger_path(&dir);
        let mut rec = sample();
        rec.agent = Some("verify".into());
        append_record(&ledger, "agent-records", &rec).unwrap();
        let md = fs::read_to_string(&ledger).unwrap();
        assert!(md.contains("| agent verify"), "ledger: {md}");
        assert_eq!(read_records(&ledger), vec![rec]);
    }

    #[test]
    fn pre_change_record_parses_to_no_agent() {
        let dir = scratch("pre-change");
        let ledger = ledger_path(&dir);
        // the exact run-line shape written before agent attribution existed
        fs::write(&ledger,
            "# Alibi\n\nchange: old\n\n## 2.1\n\n- run: 2026-09-29T21:40:12.000+07:00 | exit 0 | 4.1s | head 1a2b3c4 | machine goby\n  `cargo test`\n").unwrap();
        let records = read_records(&ledger);
        assert_eq!(records.len(), 1);
        let rec = &records[0];
        assert_eq!(rec.agent, None);
        assert_eq!(rec.task, "2.1");
        assert_eq!(rec.exit, 0);
        assert_eq!(rec.command, "cargo test");
    }

    #[test]
    fn round_trip_without_agent_keeps_shape() {
        let dir = scratch("no-agent");
        let ledger = ledger_path(&dir);
        append_record(&ledger, "agent-records", &sample()).unwrap();
        let md = fs::read_to_string(&ledger).unwrap();
        assert!(!md.lines().any(|l| l.contains("| agent ")), "no agent field may render: {md}");
        assert_eq!(read_records(&ledger), vec![sample()]);
    }

    #[test]
    fn staleness_head_ancestor_cases_in_temp_repo() {
        let repo = scratch("stale");
        // real git repo: commit twice, prove at first HEAD
        run_git(&repo, &["init", "-q"]);
        fs::write(repo.join("a.txt"), "1\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "a", "--allow-empty"]);
        let first = read_head(&repo);
        let mut rec = sample();
        rec.head = first.clone();
        // fresh: recorded head is the current HEAD
        assert_eq!(staleness(&repo, &repo, &rec, &task()), None);
        // fresh: recorded head becomes an ancestor
        fs::write(repo.join("a.txt"), "2\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "b"]);
        assert_eq!(staleness(&repo, &repo, &rec, &task()), None);
        // stale: recorded head not an ancestor (HEAD is back on main, other branch head is not its ancestor)
        run_git(&repo, &["checkout", "-q", "-b", "other", first.as_str()]);
        run_git(&repo, &["commit", "-q", "-m", "c", "--allow-empty"]);
        rec.head = read_head(&repo);
        run_git(&repo, &["checkout", "-q", "-"]);
        assert_eq!(staleness(&repo, &repo, &rec, &task()), Some(StaleReason::HeadMoved));
    }

    #[test]
    fn staleness_specs_synced_case() {
        let repo = scratch("specs-synced");
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["commit", "-q", "-m", "a", "--allow-empty"]);
        let mut rec = sample();
        rec.head = read_head(&repo);
        // spec file written before the run date: fresh
        let specs = repo.join("specs");
        fs::create_dir_all(&specs).unwrap();
        fs::write(specs.join("delta.md"), "## ADDED\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        rec.date = Record::now_date();
        assert_eq!(staleness(&repo, &repo, &rec, &task()), None);
        // spec rewritten after the run: stale
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(specs.join("delta.md"), "## MODIFIED\n").unwrap();
        assert_eq!(staleness(&repo, &repo, &rec, &task()), Some(StaleReason::SpecsSynced));
    }

    fn task() -> Task {
        Task { id: "2.1".into(), title: "do it; verify cargo test".into() }
    }

    #[test]
    fn staleness_task_changed_and_worktree_changed() {
        let repo = scratch("stale-new");
        run_git(&repo, &["init", "-q"]);
        fs::write(repo.join("a.txt"), "1\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "a"]);
        fs::create_dir_all(repo.join("openspec")).unwrap();
        fs::write(repo.join("openspec/alibi.md"), "ledger\n").unwrap();
        assert_eq!(capture_dirty(&repo), None, "openspec/ is not part of the tested tree");
        let mut rec = sample();
        rec.head = read_head(&repo);
        rec.date = Record::now_date();
        rec.task_hash = Some(fingerprint(&task().title));
        assert_eq!(staleness(&repo, &repo, &rec, &task()), None);
        let edited = Task { title: "do it; verify cargo test -p x".into(), ..task() };
        assert_eq!(staleness(&repo, &repo, &rec, &edited), Some(StaleReason::TaskChanged));
        // proof taken on an uncommitted edit and a new file
        fs::write(repo.join("a.txt"), "2\n").unwrap();
        fs::write(repo.join("b.txt"), "new\n").unwrap();
        rec.dirty = capture_dirty(&repo);
        assert_eq!(rec.dirty.as_ref().unwrap().paths, vec!["a.txt".to_string(), "b.txt".to_string()]);
        assert_eq!(staleness(&repo, &repo, &rec, &task()), None);
        // committing exactly what was tested keeps the proof fresh
        run_git(&repo, &["add", "a.txt", "b.txt"]);
        run_git(&repo, &["commit", "-q", "-m", "b"]);
        assert_eq!(staleness(&repo, &repo, &rec, &task()), None);
        // editing a tested file after the run makes it stale
        fs::write(repo.join("a.txt"), "3\n").unwrap();
        assert_eq!(staleness(&repo, &repo, &rec, &task()), Some(StaleReason::WorktreeChanged));
        fs::write(repo.join("a.txt"), "2\n").unwrap();
        fs::remove_file(repo.join("b.txt")).unwrap();
        assert_eq!(staleness(&repo, &repo, &rec, &task()), Some(StaleReason::WorktreeChanged));
    }

    #[test]
    fn round_trip_with_all_optional_fields() {
        let dir = scratch("all-fields");
        let ledger = ledger_path(&dir);
        let mut rec = sample();
        rec.agent = Some("verify".into());
        rec.task_hash = Some(fingerprint("x"));
        rec.dirty = Some(Dirty { paths: vec!["src/a b.rs".into(), "100%\nodd".into()], digest: "0123456789abcdef".into() });
        rec.tail = vec!["test result: ok. 3 passed".into(), "## not a header".into(), "  dirty: not a path".into()];
        append_record(&ledger, "c", &rec).unwrap();
        append_record(&ledger, "c", &sample()).unwrap();
        assert_eq!(read_records(&ledger), vec![rec, sample()]);
    }

    #[test]
    fn task_lines_accept_openspec_bullets() {
        let md = "* [ ] 1.1 star\n+ [x] 1.2 plus\n1. [ ] 1.3 ordered\n2) [X] 1.4 paren\n  - [ ] 1.5 indented\n- [ ] write docs\n- [ ](link) 9.9 not a task\n- [ab] 9.8 not a task\n";
        let ids: Vec<String> = parse_tasks(md).into_iter().map(|t| t.id).collect();
        assert_eq!(ids, ["1.1", "1.2", "1.3", "1.4", "1.5"]);
        assert_eq!(unnumbered_tasks(md), vec!["write docs".to_string()]);
    }

    #[test]
    fn output_tail_keeps_last_clean_lines() {
        let out = b"a\n\x1b[32mok\x1b[0m\n\nprogress 1\rprogress 2\r\n\tdone\n";
        assert_eq!(output_tail(out, 3), vec!["ok", "progress 2", " done"]);
        assert_eq!(output_tail(b"", 5), Vec::<String>::new());
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let out = ProcCommand::new("git").args(args).current_dir(repo).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn parse_tasks_ids_and_titles() {
        let md = "# Tasks\n\n## 1. Group\n\n- [ ] 1.1 Create workspace; verify cargo\n- [x] 1.2 Done thing; verify tests\n- [ ] not-a-task id here\n  plain line\n";
        let tasks = parse_tasks(md);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "1.1");
        assert!(tasks[0].title.starts_with("Create workspace"));
        assert_eq!(tasks[1].id, "1.2");
    }

    #[test]
    fn parse_tasks_allows_missing_title() {
        let md = "# Tasks\n\n- [x] 1.1\n- [ ] 2.10\n- [ ] 3\n- [ ] x.1\n";
        let tasks = parse_tasks(md);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "1.1");
        assert_eq!(tasks[1].id, "2.10");
        assert!(tasks.iter().all(|t| t.title.is_empty()));
    }
}
