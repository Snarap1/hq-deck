---
name: retro
description: "Conduct a retrospective on a coding session."
disable-model-invocation: true
---

The user has asked for a **retrospective**.
You are suggesting improvements to the coding agent's **environment** to improve future runs.

## Steps

1. Call the Skill tool with `writing-for-agents` for the writing style guide.

2. Read the primary sources for the session the user specifies.
   This may mean searching through session logs on this machine.
   If the user doesn't specify a session, default to the current one.

3. Look for candidates for improvement in these categories.

- **Navigation**: how easy was it for the agent to find the right files?
  Are there hidden dependencies between files?
  Would a **navigation pointer** make it easier?
  _Use when_ the session took a long time to find a piece of information.
- **Automated checks**: are there automated checks that could catch errors the agent made?
  Linting, typing, tests, filesystem linters?
  Read the repo's own check command first (the `justfile`: `just ci` is the whole check here, by design: this repo has no CI pipeline), so a check that already exists but sits unwired or silently broken is the finding, not a reinvention.
  A repo with no **guardrail** is itself a finding, but this repo has one: `just ci`.
  Do not propose a CI pipeline; propose a check inside `just ci`, a `clippy.toml` rule, or a test.
  _Use when_ the agent made a mistake an automated check could have caught.
- **Coding standards**: should the **reviewer agent** be given a new rule to enforce?
  Should an existing rule be removed or clarified?
  Classify the violation first: a **mechanical** one (a fixed syntactic pattern, a banned API, an import shape, a file-location rule) gets a deterministic check, full stop: a clippy lint or `clippy.toml` entry, a test over the source tree, or a step in `just ci`, whichever is cheapest.
  Default to building the check over writing the rule.
  Reserve the standards documents (the `AGENTS.md` Invariants, `policies/`) for genuine **judgement calls** (cross-file consistency, "matches the surrounding style," anything no guardrail could ever substitute for).
  _Use when_ the reviewer agent failed to catch a mistake.
- **Global AGENTS.md**: are there any steering instructions that should be moved to coding standards (or automated checks) instead?
  _Use when_ the AGENTS.md file is particularly large - in the repo OR the user's global scope.
- **Tool economy**: did the agent make expensive tool calls that could be streamlined?
  Is there any custom tooling (CLI's, MCP's) that is particularly token-inefficient?
  A full `cargo build` or a benchmark run while another agent measures is expensive in wall time too.
  _Use when_ the agent made an expensive tool call.
- **No-ops**: look for instructions in steering files that don't modify the agent's behavior.
  _Use when_ the steering files are large and unwieldy.
- **Information access**: look for opportunities to increase the agent's access to information.
  Teeing dev server logs, readonly access to third-party services.
  _Use when_ a crucial piece of information was not available to the agent.

4. Present these candidates to the user, in order of severity.

## Reference

### Implementation vs Review

Remember that all work goes through two stages: implementation and review.
The implementation agent has the most **context pressure**.
They are responsible for exploration, writing code, and debugging failures.

The review agent has the least context pressure - it receives a diff, so no exploration needed.
It often does not need to write code or debug.

This means that the review agent should be responsible for imposing coding standards, not the implementation agent.

### Files

You have access to several files in the repo:

- `CLAUDE.md`/`AGENTS.md`: these files are pushed to the context window of any agent working in this repo.
  They should be used incredibly sparingly, usually only for **navigation pointers** to other files.
- `CODING_STANDARDS.md`: this file is read during review, not implementation.
  This repo has none yet; its standards live in the `AGENTS.md` Invariants and in `policies/`.
  Add **navigation pointers** to docs folders if a standards file gets more than 1,000 lines long.
- Docs: use docs as references files, pointed to by other files.
  Look for existing docs before writing new ones.
- Skills: use skills for docs (since their description goes into the agent's context window), or for user-invoked commands.
  Follow the advice in the `writing-for-agents` skill.
