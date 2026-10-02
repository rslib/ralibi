---
name: ralibi
description: Proof ledger for OpenSpec change tasks, run through the ralibi CLI. Use it whenever you finish a task that ends with a verify clause, when you want to check which tasks of a change still lack proof, and before archiving a change (run the gate first).
---

# ralibi

ralibi records proof for OpenSpec change tasks. A task is done when its verify command has run; ralibi runs it, records the evidence in the change's ledger, and refuses to bless a change whose tasks lack fresh proof. Pass `--toon` on every ralibi command you run: TOON is the compact, structured format meant for you. Without it ralibi prints human text, for people at a terminal.

- The ledger lives at `openspec/changes/<change>/alibi.md` inside the project's openspec tree. It is append-only Markdown; ralibi never rewrites existing records, never modifies tasks.md or spec files, and never touches git beyond reading HEAD.
- Setup, once per machine: `ralibi install` writes this skill (embedded in the binary) into every harness skill dir that exists (pi, Claude Code, omp, codex, opencode). Re-run it after upgrading ralibi. `ralibi install --uninstall` removes only files matching the embedded skill; files you edited by hand are left alone.
- Run `ralibi run <task> -- <command...>` when you complete a task: it runs the command, appends one record (task, command, exit, duration, HEAD, date, machine) and exits with the command's status. Chain it: `ralibi run 2.1 -- cargo test -p ralibi-core`.
- Run the command exactly as the task's verify clause states. ralibi records; it never judges whether the command was the right proof.
- Run `ralibi status [--change <id>]` for the per-task table: `proved`, `missing`, or `stale` with a reason (`head-moved` when the proof's recorded HEAD is no longer an ancestor of the current HEAD, `specs-synced` when the change's spec deltas changed after the run). Stale is a warning, not a failure.
- Run `ralibi gate [--change <id>]` before archiving a change. It exits nonzero while any task is `missing` and lists them; `--stale-as-fail` also refuses stale proofs. Re-prove a stale task by running `ralibi run` for it again.
- Run `ralibi show [--change <id>]` to print the ledger trail, newest tasks with their commands, exits, heads and machines. Each record is marked `latest` or `superseded`: a superseded record lost to a later run of the same task, which is the run that `status` and `gate` trust. Read superseded records to see how a task was proved before.
- The `--change <id>` flag selects the change when more than one is in flight; otherwise the single in-flight change is used. When the flag names no in-flight change, ralibi also looks in the change archive, under `archive/<id>` or the dated `archive/YYYY-MM-DD-<id>` that OpenSpec creates, so `show`, `status` and `gate` still work after archiving (auto-selection without the flag never picks an archived change). Every error is a record with a `code` and a `fix`; run the fix or show it to the user.
