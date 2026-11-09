---
name: implement
description: "Implement a piece of work based on a spec or set of tickets."
disable-model-invocation: true
---

Implement the work described by the user in the spec or tickets.

If the user passes a ticket reference, fetch it from the issue tracker and state its title before starting.
If the reference is ambiguous, ask.

Call the Skill tool with "tdd" where possible, at pre-agreed seams.

Check the build regularly with `cargo check --all-targets`.
Run single tests regularly: `cargo test <name>`, or `cargo test --test <file> <name>` for a file in `tests/`.
Run `just ci` once at the end: fmt-check, clippy with warnings as errors, the full suite, and the release build.
When the change touches `run_editor`, the terminal mode stack, or the editor dialects, run `just smoke-edit` too, detached, as `AGENTS.md` describes.
When it touches the reload, render, git, or highlight paths, run the benchmark before and after, as `AGENTS.md` describes.

Once done, call the Skill tool with "review" to review the work.

Commit your work to the current branch.
The commit message carries no `Co-Authored-By` trailer for the agent.
