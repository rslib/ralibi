use ralibi_core::*;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as ProcCommand;
use std::time::Instant;

/// What a proof state is for one task.
#[derive(Debug, Clone, PartialEq)]
enum Proof {
    Proved(Record),
    Stale(Record, StaleReason),
    Failed(Record),
    Missing,
}

#[derive(Debug)]
struct Args {
    cmd: String,
    toon: bool,
    change: Option<String>,
    task: Option<String>,
    command: Vec<String>,
    stale_as_fail: bool,
    uninstall: bool,
    agent: Option<String>,
}

const USAGE: &str = "usage: ralibi <command> [options]

commands:
  run <task> -- <command...>   run the task's verify command, record proof, exit with its status
  status [--change <id>]       per-task proof state table
  gate [--change <id>] [--stale-as-fail]
                              exit nonzero while any task lacks passing proof
  show [--change <id>]        print the ledger trail
  install [--uninstall]       symlink the ralibi skill into harness skill dirs

options:
  --toon                      machine-readable output (agents pass this)
  --change <id>               select the change (default: the single in-flight one)
  --agent <name>              attribute the proof run to an agent (env RALIBI_AGENT as fallback)";

fn parse_args(argv: &[String]) -> std::result::Result<Args, i32> {
    let mut args = Args { cmd: String::new(), toon: false, change: None, task: None, command: Vec::new(), stale_as_fail: false, uninstall: false, agent: None };
    let mut words = argv.iter();
    let Some(sub) = words.next() else {
        eprintln!("{USAGE}");
        return Err(2);
    };
    let rest: Vec<String> = words.cloned().collect();
    match sub.as_str() {
        "run" => {
            args.cmd = "run".into();
            let mut after_dashdash = false;
            let mut it = rest.iter();
            while let Some(w) = it.next() {
                let w = w.as_str();
                if after_dashdash {
                    args.command.push(w.to_string());
                } else if w == "--" {
                    after_dashdash = true;
                } else if args.task.is_none() && !w.starts_with("--") {
                    args.task = Some(w.to_string());
                } else if w == "--toon" {
                    args.toon = true;
                } else if w == "--change" {
                    args.change = it.next().map(|v| v.to_string());
                } else if let Some(id) = w.strip_prefix("--change=") {
                    args.change = Some(id.to_string());
                } else if w == "--agent" {
                    args.agent = it.next().map(|v| v.to_string());
                } else if let Some(name) = w.strip_prefix("--agent=") {
                    args.agent = Some(name.to_string());
                } else {
                    eprintln!("ralibi run: unknown option '{w}'\n{USAGE}");
                    return Err(2);
                }
            }
            if args.task.is_none() || args.command.is_empty() {
                eprintln!("ralibi run <task> -- <command...>\npass a task id and the verify command after --");
                return Err(2);
            }
        }
        "status" | "gate" | "show" => {
            args.cmd = sub.clone();
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--toon" => args.toon = true,
                    "--change" if i + 1 < rest.len() => {
                        args.change = Some(rest[i + 1].clone());
                        i += 1;
                    }
                    w if w.starts_with("--change=") => args.change = Some(w["--change=".len()..].to_string()),
                    "--stale-as-fail" if sub == "gate" => args.stale_as_fail = true,
                    w => {
                        eprintln!("ralibi {sub}: unknown option '{w}'\n{USAGE}");
                        return Err(2);
                    }
                }
                i += 1;
            }
        }
        "install" => {
            args.cmd = "install".into();
            for w in rest {
                match w.as_str() {
                    "--toon" => args.toon = true,
                    "--uninstall" => args.uninstall = true,
                    other => {
                        eprintln!("ralibi install: unknown option '{other}'\n{USAGE}");
                        return Err(2);
                    }
                }
            }
        }
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            return Err(0);
        }
        other => {
            eprintln!("ralibi: unknown command '{other}'\n{USAGE}");
            return Err(2);
        }
    }
    if let Some(v) = args.change.as_deref() {
        if v.is_empty() {
            eprintln!("--change requires a value");
            return Err(2);
        }
    }
    Ok(args)
}

/// TOON-style single-line record. Fields: key=value pairs.
fn kv(pairs: &[(&str, String)]) -> String {
    pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
}

fn toon_error(e: &CoreError) -> String {
    kv(&[("type", "error".into()), ("code", e.code.clone()), ("message", e.message.clone()), ("fix", e.fix.clone())])
}

fn human_error(e: &CoreError) -> String {
    format!("ralibi {}: {}\nfix: {}", e.code, e.message, e.fix)
}

struct Ctx {
    root: PathBuf,
    change: String,
    change_dir: PathBuf,
    tasks: Vec<Task>,
    records: Vec<Record>,
}

fn ctx_for(change_flag: Option<&str>) -> std::result::Result<Ctx, CoreError> {
    let cwd = std::env::current_dir().map_err(|e| CoreError {
        code: "io".into(),
        message: format!("cannot read cwd: {e}"),
        fix: "check the working directory".into(),
    })?;
    let Some(root) = find_openspec_root(&cwd) else {
        return Err(CoreError {
            code: "no-openspec".into(),
            message: "no openspec/changes at or above the working directory".into(),
            fix: "cd into a repository with an openspec tree".into(),
        });
    };
    let resolved = resolve_change(&root, change_flag)?;
    let change = resolved.id.clone();
    let change_dir = resolved.dir.clone();
    let tasks = read_tasks(&change_dir)?;
    let records = read_records(&ledger_path(&change_dir));
    Ok(Ctx { root, change, change_dir, tasks, records })
}

/// Latest record per task, in ledger order.
fn latest_per_task(records: &[Record]) -> std::collections::HashMap<String, Record> {
    let mut latest = std::collections::HashMap::new();
    for rec in records {
        latest.insert(rec.task.clone(), rec.clone());
    }
    latest
}

/// True when a later record for the same task id exists after `index` in the ledger.
fn is_superseded(records: &[Record], index: usize) -> bool {
    records[index + 1..].iter().any(|later| later.task == records[index].task)
}

fn proof_state(ctx: &Ctx, task_id: &str) -> Proof {
    let latest = latest_per_task(&ctx.records);
    let Some(rec) = latest.get(task_id) else { return Proof::Missing };
    let rec = rec.clone();
    if rec.exit != 0 {
        return Proof::Failed(rec);
    }
    match staleness(&ctx.root, &ctx.change_dir, &rec) {
        Some(reason) => Proof::Stale(rec, reason),
        None => Proof::Proved(rec),
    }
}

fn cmd_run(ctx: &Ctx, task_id: &str, command: &[String], toon: bool, agent: Option<String>) -> std::result::Result<i32, CoreError> {
    if !ctx.tasks.iter().any(|t| t.id == task_id) {
        return Err(CoreError {
            code: "unknown-task".into(),
            message: format!("no task '{task_id}' in change '{}'", ctx.change),
            fix: format!("use one of: {}", ctx.tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(", ")),
        });
    }
    let start = Instant::now();
    let status = ProcCommand::new(&command[0])
        .args(&command[1..])
        .current_dir(&ctx.root)
        .status();
    let duration_ms = start.elapsed().as_millis();
    match status {
        Ok(st) => {
            let rec = Record {
                task: task_id.to_string(),
                command: command.join(" "),
                exit: st.code().unwrap_or(-1),
                duration_ms,
                head: read_head(&ctx.root),
                date: Record::now_date(),
                machine: Record::machine_name(),
                agent,
            };
            append_record(&ledger_path(&ctx.change_dir), &ctx.change, &rec)?;
            let mut pairs = vec![
                ("type", "run".to_string()),
                ("task", rec.task.clone()),
                ("exit", rec.exit.to_string()),
                ("duration_ms", rec.duration_ms.to_string()),
                ("head", rec.head.clone()),
                ("machine", rec.machine.clone()),
                ("ledger", ledger_path(&ctx.change_dir).display().to_string()),
            ];
            if let Some(a) = &rec.agent {
                pairs.push(("agent", a.clone()));
            }
            if toon {
                println!("{}", kv(&pairs));
            } else {
                println!("recorded: {} exit {} ({:.1}s) -> {}", rec.task, rec.exit, rec.duration_ms as f64 / 1000.0, ledger_path(&ctx.change_dir).display());
            }
            Ok(rec.exit)
        }
        Err(e) => Err(CoreError {
            code: "spawn".into(),
            message: format!("cannot run '{}': {e}", command[0]),
            fix: "check the command name and PATH".into(),
        }),
    }
}

fn cmd_status(ctx: &Ctx, toon: bool) -> std::result::Result<(), CoreError> {
    if toon {
        for task in &ctx.tasks {
            match proof_state(ctx, &task.id) {
                Proof::Proved(rec) => println!("{}", kv(&[("type", "task".into()), ("change", ctx.change.clone()), ("task", task.id.clone()), ("state", "proved".into()), ("command", rec.command.clone())])),
                Proof::Stale(rec, reason) => println!("{}", kv(&[("type", "task".into()), ("change", ctx.change.clone()), ("task", task.id.clone()), ("state", "stale".into()), ("reason", reason.as_str().into()), ("command", rec.command.clone())])),
                Proof::Failed(rec) => println!("{}", kv(&[("type", "task".into()), ("change", ctx.change.clone()), ("task", task.id.clone()), ("state", "failed".into()), ("exit", rec.exit.to_string()), ("command", rec.command.clone())])),
                Proof::Missing => println!("{}", kv(&[("type", "task".into()), ("change", ctx.change.clone()), ("task", task.id.clone()), ("state", "missing".into())])),
            }
        }
    } else {
        println!("change: {}", ctx.change);
        for task in &ctx.tasks {
            match proof_state(ctx, &task.id) {
                Proof::Proved(rec) => println!("  proved  {}  {}", task.id, rec.command),
                Proof::Stale(rec, reason) => println!("  stale   {}  {} ({})", task.id, rec.command, reason.as_str()),
                Proof::Failed(rec) => println!("  failed  {}  {} (exit {})", task.id, rec.command, rec.exit),
                Proof::Missing => println!("  missing {}", task.id),
            }
        }
    }
    Ok(())
}

fn cmd_gate(ctx: &Ctx, toon: bool, stale_as_fail: bool) -> std::result::Result<i32, CoreError> {
    let mut missing: Vec<String> = Vec::new();
    let mut failed: Vec<(String, i32)> = Vec::new();
    let mut stale: Vec<(String, StaleReason)> = Vec::new();
    for task in &ctx.tasks {
        match proof_state(ctx, &task.id) {
            Proof::Proved(_) => {}
            Proof::Stale(_, reason) => stale.push((task.id.clone(), reason)),
            Proof::Failed(rec) => failed.push((task.id.clone(), rec.exit)),
            Proof::Missing => missing.push(task.id.clone()),
        }
    }
    let pass = missing.is_empty() && failed.is_empty() && (stale.is_empty() || !stale_as_fail);
    if toon {
        for id in &missing {
            println!("{}", kv(&[("type", "gate".into()), ("task", id.clone()), ("state", "missing".into())]));
        }
        for (id, exit) in &failed {
            println!("{}", kv(&[("type", "gate".into()), ("task", id.clone()), ("state", "failed".into()), ("exit", exit.to_string())]));
        }
        for (id, r) in &stale {
            println!("{}", kv(&[("type", "gate".into()), ("task", id.clone()), ("state", "stale".into()), ("reason", r.as_str().into())]));
        }
        if pass {
            println!("{}", kv(&[("type", "gate".into()), ("state", "pass".into())]));
        }
    } else if pass {
        println!("gate: pass ({} tasks proved)", ctx.tasks.len());
    } else {
        if !missing.is_empty() {
            println!("gate: missing proof for {}", missing.join(", "));
        }
        for (id, exit) in &failed {
            println!("gate: failed {} (exit {})", id, exit);
        }
        for (id, r) in &stale {
            println!("gate: stale {} ({})", id, r.as_str());
        }
    }
    Ok(if pass { 0 } else { 1 })
}

fn cmd_show(ctx: &Ctx, toon: bool) -> std::result::Result<(), CoreError> {
    if ctx.records.is_empty() {
        if toon {
            println!("{}", kv(&[("type", "show".into()), ("change", ctx.change.clone()), ("records", "0".into())]));
        } else {
            println!("no records for change '{}'", ctx.change);
        }
        return Ok(());
    }
    if toon {
        for (i, rec) in ctx.records.iter().enumerate() {
            let state = if is_superseded(&ctx.records, i) { "superseded" } else { "latest" };
            println!("{}", kv(&[
                ("type", "record".into()),
                ("change", ctx.change.clone()),
                ("task", rec.task.clone()),
                ("state", state.into()),
                ("command", rec.command.clone()),
                ("exit", rec.exit.to_string()),
                ("duration_ms", rec.duration_ms.to_string()),
                ("head", rec.head.clone()),
                ("date", rec.date.clone()),
                ("machine", rec.machine.clone()),
                ("agent", rec.agent.clone().unwrap_or_default()),
            ]));
        }
    } else {
        for (i, rec) in ctx.records.iter().enumerate() {
            let state = if is_superseded(&ctx.records, i) { "superseded" } else { "latest" };
            let short_head = rec.head.get(..7).unwrap_or(&rec.head);
            println!("{}  {}  {}  exit {}  {:.1}s  head {}  {}", rec.date, rec.task, state, rec.exit, rec.duration_ms as f64 / 1000.0, short_head, rec.machine);
            println!("  `{}`", rec.command);
        }
    }
    Ok(())
}

/// Harness skill dirs, same target list as the skills-repo install.sh.
const HARNESSES: &[(&str, &str)] = &[
    ("pi", ".pi/agent/skills"),
    ("claude", ".claude/skills"),
    ("omp", ".omp/agent/skills"),
    ("codex", ".codex/skills"),
    ("opencode", ".config/opencode/skills"),
];

/// The skill ships inside the binary (the rkb pattern): install works from any
/// location, including a cargo-installed copy with no source repo on the machine.
const EMBEDDED_SKILL: &str = include_str!("../../../skills/ralibi/SKILL.md");

/// Read the SKILL.md content through `dir`, whether dir is real or a symlink. None when absent.
fn skill_content(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join("SKILL.md")).ok()
}

/// `ralibi install`: write the embedded skill into every harness whose parent exists.
/// Refuses to clobber real files that differ; --uninstall removes only what matches.
fn cmd_install(uninstall: bool, toon: bool) -> i32 {
    let home = match std::env::var("HOME") {
        Ok(h) => PathBuf::from(h),
        Err(_) => {
            report(&CoreError { code: "no-home".into(), message: "HOME is not set".into(), fix: "set HOME to the user's directory".into() }, toon);
            return 1;
        }
    };
    let mut failures = 0;
    for (name, rel) in HARNESSES {
        let dir = home.join(rel);
        let parent = dir.parent().expect("skill dir has a parent");
        let dst = dir.join("ralibi");
        if !parent.is_dir() {
            println!("{}", kv(&[("harness", name.to_string()), ("state", "skipped".into()), ("reason", "missing-parent".into())]));
            continue;
        }
        if uninstall {
            match skill_content(&dst) {
                Some(content) if content == EMBEDDED_SKILL => {
                    let _ = fs::remove_file(dst.join("SKILL.md"));
                    if !dst.is_symlink() {
                        let _ = fs::remove_dir(&dst); // only succeeds when empty
                    } else {
                        let _ = fs::remove_dir_all(&dst);
                    }
                    println!("{}", kv(&[("harness", name.to_string()), ("state", "removed".into())]));
                }
                Some(_) => println!("{}", kv(&[("harness", name.to_string()), ("state", "left".into()), ("reason", "modified-content".into())])),
                None => println!("{}", kv(&[("harness", name.to_string()), ("state", "absent".into())])),
            }
            continue;
        }
        match skill_content(&dst) {
            // identical real content, already ours: nothing to do. A symlink with identical
            // content still falls through to replacement, so installs track the binary.
            Some(content) if content == EMBEDDED_SKILL && !dst.is_symlink() => {
                println!("{}", kv(&[("harness", name.to_string()), ("state", "unchanged".into())]));
            }
            Some(_) if !dst.is_symlink() => {
                println!("{}", kv(&[("harness", name.to_string()), ("state", "refused".into()), ("reason", "different-content".into())]));
                failures += 1;
            }
            _ => {
                // missing, or a symlink (dev checkout link): replace with the real file
                let was_symlink = dst.is_symlink();
                if let Err(e) = (|| -> std::io::Result<()> {
                    if was_symlink {
                        fs::remove_dir_all(&dst)?;
                    }
                    fs::create_dir_all(&dst)?;
                    fs::write(dst.join("SKILL.md"), EMBEDDED_SKILL)
                })() {
                    println!("{}", kv(&[("harness", name.to_string()), ("state", "error".into()), ("reason", e.to_string())]));
                    failures += 1;
                    continue;
                }
                let state = if was_symlink { "replaced" } else { "written" };
                println!("{}", kv(&[("harness", name.to_string()), ("state", state.into()), ("path", dst.display().to_string())]));
            }
        }
    }
    if failures == 0 { 0 } else { 1 }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(code) => std::process::exit(code),
    };
    // install runs before openspec resolution: it works from any directory
    if args.cmd == "install" {
        std::process::exit(cmd_install(args.uninstall, args.toon));
    }
    match &ctx_for(args.change.as_deref()) {
        Ok(ctx) => {
            // flag wins over the env; absent both, no attribution is recorded
            let agent = args
                .agent
                .clone()
                .or_else(|| std::env::var("RALIBI_AGENT").ok().filter(|v| !v.is_empty()));
            let code = match args.cmd.as_str() {
                "run" => cmd_run(ctx, args.task.as_deref().unwrap(), &args.command, args.toon, agent),
                "status" => cmd_status(ctx, args.toon).map(|_| 0),
                "gate" => cmd_gate(ctx, args.toon, args.stale_as_fail),
                "show" => cmd_show(ctx, args.toon).map(|_| 0),
                _ => unreachable!("parse_args validates the command"),
            };
            match code {
                Ok(c) => std::process::exit(c),
                Err(e) => {
                    report(&e, args.toon);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            report(e, args.toon);
            std::process::exit(1);
        }
    }
}

fn report(e: &CoreError, toon: bool) {
    let _ = std::io::stdout().flush();
    if toon {
        eprintln!("{}", toon_error(e));
    } else {
        eprintln!("{}", human_error(e));
    }
}
