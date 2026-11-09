---
name: tdd
description: Test-driven development. Use when the user wants to build features or fix bugs test-first, mentions "red-green-refactor", or wants integration tests.
---

# Test-Driven Development

TDD is the red → green loop.
This skill is the reference that makes that loop produce tests worth keeping: what a good test is, where tests go, the anti-patterns, and the rules of the loop.
Every section applies on every cycle: consult them before and during the loop, not after.

When exploring the codebase, read `GLOSSARY.md` (if it exists) so test names and interface vocabulary match the project's domain language, and respect ADRs in the area you're touching.
Read the **Invariants** section of `AGENTS.md` too: a test that touches comments, writes, or place state pins one of them, and cites it by name.

## What a good test is

Tests verify behavior through public interfaces, not implementation details.
Code can change entirely; tests shouldn't.
A good test reads like a specification: "a comment whose lines were all deleted stays orphaned on the line above the gap" tells you exactly what capability exists, and it survives refactors because it doesn't care about internal structure.

See [tests.md](tests.md) for examples and [mocking.md](mocking.md) for mocking guidelines.

## Seams: where tests go

A **seam** is the public boundary you test at: the interface where you observe behavior without reaching inside.
Tests live at seams, never against internals.

**Test only at pre-agreed seams.**
Before writing any test, write down the seams under test and confirm them with the user.
No test is written at an unconfirmed seam.
You can't test everything, so agreeing the seams up front is how testing effort lands on the critical paths and complex logic instead of every edge case.

Ask: "What's the public interface, and which seams should we test?"
Give each proposed seam a one-line note on what it catches and what it misses.

The seams this repo already has, highest first (prefer the highest one that can see the behavior):

- **The `App` through its keys and its render.**
  Integration tests in `tests/` build an `App` over a real temporary git repo (`common::Repo`, `common::app_on`), drive it with `handle_key`, and read the painted frame (`tests/render.rs`).
  This is the seam for anything a user can see or do.
- **A pure module's own interface.**
  Unit tests in a `#[cfg(test)] mod tests` at the foot of the module, for the pure ones: `editor.rs` (argv resolution), `edit_buffer.rs`, `commit_template.rs`, `model.rs`, the parse and pick functions of `agent.rs`.
- **The real binary under a PTY.**
  `just smoke-edit` and `scripts/bench_tui.py`; reserved for the terminal mode stack and latency, because they are slow and not part of `just ci`.

When the shape of that interface is itself in question (how deep the module is, where the seam belongs, what the interface should expose), call the Skill tool with "codebase-design" for the vocabulary.
It is the shared source of the module, interface, depth, seam, adapter, leverage and locality terms, and it is a reference to consult, not a session to run.

## Anti-patterns

- **Implementation-coupled**: mocks internal collaborators, tests private methods, or verifies through a side channel (reading `git` state with a raw subprocess instead of using the interface).
  The tell: the test breaks when you refactor but behavior hasn't changed.
- **Tautological**: the assertion recomputes the expected value the way the code does (`assert_eq!(add(a, b), a + b)`, a snapshot derived by hand the same way, a constant asserted equal to itself), so it passes by construction and can never disagree with the code.
  Expected values must come from an independent source of truth: a known-good literal, a worked example, the spec.
- **Horizontal slicing**: writing all tests first, then all implementation.
  Bulk tests verify _imagined_ behavior: you test the _shape_ of things rather than user-facing behavior, the tests go insensitive to real changes, and you commit to test structure before understanding the implementation.
  Work in **vertical slices** instead: one test → one implementation → repeat, each test a **tracer bullet** that responds to what the last cycle taught you.

## Rules of the loop

- **Red before green.**
  Write the failing test first, then only enough code to pass it.
  Don't anticipate future tests or add speculative features.
- **One slice at a time.**
  One seam, one test, one minimal implementation per cycle.
- **Refactoring is not part of the loop.**
  It belongs to the review stage (see the `review` skill), not the red → green implementation cycle.

## Running the loop

- One test: `cargo test <name>`; one integration file: `cargo test --test app_flow <name>`.
- The whole suite: `just test`.
- Before calling the change done: `just ci` (fmt-check, clippy with warnings as errors, tests, release build).
- A test that fails or flakes while you work on something else still gets fixed.
