# When to Mock

Mock at **system boundaries** only:

- External processes and services: `hc`/`herdr` (agent send), `gh`/`glab` (the PR tab), `claude -p` (the commit-message generator), clipboard tools
- Time/randomness
- The terminal (the renderer draws into a `TestBackend`; no real tty)
- File system (sometimes: prefer a real temporary directory)

Don't mock:

- Your own structs/modules
- Internal collaborators
- Anything you control
- git: use a real temporary repository (`tests/common::Repo`), it is fast and it is the thing under test

## Designing for Mockability

At system boundaries, design interfaces that are easy to fake.
In Rust the fake is a small hand-written type behind a trait or a function argument; no mocking framework.

**1. Pass the dependency in**

Take the boundary as a parameter instead of reaching for it inside:

```rust
// Easy to fake: the target is an argument
fn export(comments: &[Comment], target: &dyn ExportTarget) -> Result<()> {
    target.export(&format_all(comments))
}

// Hard to fake: the clipboard tool is chosen inside
fn export(comments: &[Comment]) -> Result<()> {
    Command::new("wl-copy").arg(format_all(comments)).status()?;
    Ok(())
}
```

`ExportTarget` is the prior art: `tests/comment_anchors.rs` has a `FakeTarget` that records what it was given and can be told to fail.

**2. Prefer one narrow operation per boundary call over a generic runner**

Create specific functions for each external operation instead of one generic function with conditional logic:

```rust
// Illustrative shape, not an existing trait.
// GOOD: each operation is independently fakeable
trait Agents {
    fn list(&self) -> Result<Vec<AgentCandidate>>;
    fn send(&self, agent: &AgentCandidate, text: &str) -> Result<()>;
}

// BAD: the fake needs a match on argv inside it
trait Agents {
    fn run(&self, argv: &[&str]) -> Result<String>;
}
```

The narrow shape means:

- Each fake returns one specific shape
- No conditional logic in test setup
- It is easy to see which operation a test exercises
- Types per operation

Where a boundary is only argv in, text out (`src/agent.rs` reaches `hc` and `herdr` only through their CLI), keep the parse and the pick pure and test them on captured output; leave the spawn itself to the PTY scripts.

**3. One fake means a hypothetical seam**

Don't add a trait for a boundary until a second implementation (production plus the test fake) is justified.
Until then a plain function taking the value it needs is enough.
