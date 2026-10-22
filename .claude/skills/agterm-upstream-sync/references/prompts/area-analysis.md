# Feature analysis subagent

One subagent per feature cluster (a command group, or one changelog entry with its commits). Spawn them in parallel,
`subagent_type: "general-purpose"`, read-only work. Replace every `<...>` before sending; the subagent sees nothing
of this skill. Paste the hard-rules block from `references/safety.md` where it says so.

```
You analyse one upstream agterm change for a port into ngterm (Rust, GPUI on Linux). You do NOT implement
anything and you do NOT edit, create or commit files: read, grep and run read-only commands only. Do not run cargo
(build, test, clippy): the gates share target/ and are running or about to. Your answer is a draft plan task.

<HARD RULES block from references/safety.md, verbatim>

Repository: <ROOT> (ngterm). Upstream clone (read-only): <UPSTREAM>, range <OLD>..<NEW>.
Read <ROOT>/CLAUDE.md first (crate map, reference rule, porting order).
The cluster: <cluster title>
Upstream commits: <hash subject, one per line>
Changelog text: <the matching changelog bullets, verbatim>
Files by area (from the classify report):
<the cluster's lines of the report: path, +/-, tests, strings>

Do this:
1. Read the upstream change: `git -C <UPSTREAM> show <hash>` per commit, or
   `git -C <UPSTREAM> diff <OLD> <NEW> -- <path>` per file. Read the Swift tests in full for the cases and exact
   strings, `site/commands.html` and `.claude/rules/*.md` hunks for the contract. Note the validation order, every
   refusal string, defaults, read-back fields and field order, on-disk format changes, and what changed in the UI.
   Code that only moved (the report marks such strings `(moved)`) is not a behavior change: say so and skip it.
2. Find the Rust side: the files that hold this behavior today (`grep -rn` in <ROOT>/crates for the command's wire
   name, its fields, the strings without Swift interpolation, the old behavior). Decide per behavior: already
   ported (cite file:line and the unit test), partly ported (what is missing), or absent.
3. Find the conformance coverage: run
   `cd <ROOT> && .venv/bin/python <SKILL_DIR>/scripts/coverage-map.py <catalog keys of this cluster>` (or
   `--grep <regex>`), and `grep -rn` in <ROOT>/conformance/tests for the behavior. List the ids that cover it, which
   are in `conformance/rust-passing.txt`, and the tests still missing (propose file, function name, `covers` keys,
   and what each asserts). A UI-only behavior has no suite test on Linux: propose its UI item
   (the GPUI form) instead.
4. Linux specifics: anything macOS-only (AppKit, Finder, NSEvent, keyboard-layout APIs) gets its GPUI/Linux
   equivalent, or "no Linux counterpart" with the reason. Consult the fork <ROOT>/../agterm-linux only for Linux
   glue upstream lacks, never as a port source.
5. Decide STATUS per behavior: `absent` (no Rust code); `partial` (part of it missing, or the code is there but no
   Rust unit test asserts what the Swift tests assert, or no conformance id covers a suite-testable behavior: then the
   task ports the tests); `already-ported` (code and tests both there: cite them); `no-behavior-change` (a move,
   rename or comment). Mark any claim you inferred from reading code without finding the deciding line as
   "unconfirmed" in RISKS.

Answer in this exact shape, nothing before or after. When the cluster holds independent changes, give one block
per change, each starting with STATUS:

STATUS: absent | partial | already-ported | no-behavior-change
TITLE: <task title: group or feature, with PR numbers>
UPSTREAM_FILES: <implementation paths, comma separated>
SWIFT_TESTS: <test file paths with the relevant function names>
CONTRACT: <commands.html section / rules files>
RUST_FILES: <files to create or modify, each with create|modify>
EVIDENCE: <for already-ported or partial: file:line and test ids that show it>
CONFORMANCE_EXISTING: <ids, each with listed|not-listed in rust-passing.txt>
CONFORMANCE_NEW: <proposed new tests, or none>
UI_ITEMS: <proposed UI-only items with their GPUI form, or none>
ITEMS:
- [ ] <one behavior per line, with the exact upstream string where there is one>
- [ ] write tests: <Rust unit tests: the cases from the Swift tests>
- [ ] write tests: <conformance tests, if any>
RISKS: <ordering dependencies on other clusters, conflicts between docs and code, or none>
```
