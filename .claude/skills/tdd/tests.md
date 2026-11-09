# Good and Bad Tests

The snippets are sketches of shape: helper names such as `enter_scm_mode`, `key` or `scm_changes` stand for whatever `tests/common` and `App` offer today.
Read a neighbouring test in `tests/` for the real helpers before writing one.

## Good Tests

**Integration-style**: Test through real interfaces, not mocks of internal parts.

```rust
// GOOD: drives the App with keys over a real repo and reads what the user would see
#[test]
fn staging_a_file_moves_it_to_the_staged_section() {
    let repo = Repo::init();
    repo.write("a.txt", "one\n");
    let mut app = app_on(&repo);
    enter_scm_mode(&mut app);

    handle_key(&mut app, key('s'), AREA, &keymap);

    assert!(render(&app).contains("Staged"));
}
```

Characteristics:

- Tests behavior users/callers care about
- Uses public API only
- Survives internal refactors
- Describes WHAT, not HOW
- One logical assertion per test

## Bad Tests

**Implementation-detail tests**: Coupled to internal structure.

```rust
// BAD: pins which private helper ran, and how many times
#[test]
fn stage_calls_run_git_once_with_add() {
    let runner = RecordingRunner::default();
    stage(&runner, "a.txt");
    assert_eq!(runner.calls(), vec![vec!["add", "--", "a.txt"]]);
}
```

Red flags:

- Mocking internal collaborators
- Testing private functions (`pub(crate)` only so the test can reach them)
- Asserting on call counts/order
- Test breaks when refactoring without behavior change
- Test name describes HOW not WHAT
- Verifying through external means instead of interface

```rust
// BAD: bypasses the interface to verify
#[test]
fn commit_writes_a_commit() {
    let repo = Repo::init();
    commit(repo.path(), "msg").unwrap();
    assert!(repo.git(&["log", "--oneline"]).contains("msg"));
}

// GOOD: verifies through the interface
#[test]
fn a_committed_file_leaves_the_changes_list() {
    let repo = Repo::init();
    repo.write("a.txt", "one\n");
    let mut app = app_on(&repo);
    commit_everything(&mut app, "msg");
    assert!(app.scm_changes().is_empty());
}
```

(When the repository itself is the behavior under test, as in `tests/git_repo.rs`, reading it back with `git` is the interface.)

**Tautological tests**: Expected value restates the implementation, so the test passes by construction.

```rust
// BAD: expected value is recomputed the way the code computes it
#[test]
fn total_height_sums_row_heights() {
    let rows = [3u16, 5];
    let expected: u16 = rows.iter().sum();
    assert_eq!(total_height(&rows), expected);
}

// GOOD: expected value is an independent, known literal
#[test]
fn total_height_sums_row_heights() {
    assert_eq!(total_height(&[3, 5]), 8);
}
```

## Where the test goes

- A pure module's behavior: a `#[cfg(test)] mod tests` at the foot of that module, table-driven where the cases share a shape.
- Anything that crosses `App`, the keymap, the renderer, or a real git repository: a file in `tests/` that does `mod common;` and uses `Repo`.
- Name the test for the behavior, in the glossary's vocabulary: `the_empty_comment_box_shows_a_placeholder`, not `test_render_2`.
