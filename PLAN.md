# ralibi implementation plan

Status: draft for review

ralibi is a Rust command-line tool that records proof for OpenSpec change tasks. A task is done when its verify command has run; ralibi runs it, records the evidence, and refuses to bless a change whose tasks lack fresh proof. It is the same idea as rkb, applied to plans instead of errors: a durable memory with a feedback loop for a thing agents currently lose.

## Problem

OpenSpec changes carry a `tasks.md` where every task ends with a verify clause. The clause runs, the box gets checked, and the evidence (command, exit status, duration, commit) evaporates with the session. Nothing can later answer "what actually proved task 2.1?" or "is this proof still fresh after HEAD moved?" Completion is a claim; nothing records it.

## Constraints

- Build one executable named `ralibi`.
- Keep code and data apart. This repository holds the code and the agent skill. Ledgers stay inside the project's `openspec/` tree, which is the project's local memory; ralibi never writes outside a change's own folder.
- Files are the source of truth. Each change keeps its ledger in `openspec/changes/<change>/alibi.md`. No daemon, no database, no network.
- Read-only toward OpenSpec: ralibi parses `tasks.md` and `config.yaml` but never modifies them, and never rewrites the ledger; it appends.
- Output follows AXI: human text by default, TOON with `--toon` (agents always pass it), errors as records with a code and a fix.
- Work offline by default.
- Correctness of the record beats compactness. The ledger keeps every field needed to trust and reproduce a proof run.
- Minimal and correct go together. No speculative verbs; a command earns its place when a workflow names it.

## Non-goals

- No MCP, no daemon, no plugin system, no TUI.
- No git operations beyond reading HEAD (no commits, no hooks installed by ralibi).
- No verification semantics of its own: ralibi runs and records commands; it does not judge whether a command was the *right* proof. The task's verify clause is authoritative.
- No cross-project aggregation, no dashboards.

## Product surface (milestone 1)

- `ralibi run <task> -- <command...>` — run the command, record `{task, command, exit, duration, HEAD, date, machine}` in the change's `alibi.md`. Exit with the command's status so wrappers chain.
- `ralibi status [--change <id>]` — TOON table: per task, `proved` / `missing` / `stale` (HEAD moved or spec synced after the proof) with the proving run.
- `ralibi gate [--change <id>]` — nonzero exit while any task lacks fresh proof. The `opsx-archive` flow calls this before archiving.
- `ralibi show [--change <id>]` — print the ledger trail, human form or TOON.

## Staleness

A proof is stale when the repository HEAD recorded at run time is no longer an ancestor of the current HEAD, or when the change's spec deltas were synced after the proof ran. Stale is a warning state, not a failure: the gate refuses on `missing` and reports `stale` with the reason.

## Design notes

- Cargo workspace, two crates: `ralibi` (CLI, output) and `ralibi-core` (library, no terminal output). Same shape as rkb, so the house stays uniform.
- Ledger format: YAML frontmatter-free Markdown, one section per task, runs appended as list items. Human-readable, git-friendly, diff-friendly.
- Rust edition 2024. Blocking I/O only; no Tokio.
- The `alibi.md` name avoids `proof.md` collisions with anything and reads as the house family.

## Milestones

1. **ledger-core** — `run`, `status`, `gate`, `show` over a single change; staleness via HEAD ancestry. Dogfood: record proof for this change's own tasks.
2. **flow hooks** — document the `gate` precondition in the opsx-archive guidance (`operations.archive.guidance` in `openspec/config.yaml`); optionally a `--stale-as-fail` flag once real flows want it.
3. **agent records** — record which agent (if any) ran a proof, once the verify/reader agents exist; report per-agent proof trails.