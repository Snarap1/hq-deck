//! The ✧ commit-message suggestion: ask the configured `claude` CLI to summarize the pending
//! diff, falling back to a filename-based heuristic when the CLI is missing, slow, or fails.
//! Runs on a background thread so the frame loop stays responsive; the event loop polls the
//! returned channel from its tick.
//!
//! The model, extra args, diff cap, timeout, and prompt are read from [`CommitMessageConfig`].
//!
//! With a branch template configured, the line produced (the model's or the fallback's) is
//! only the summary: the template around it comes from the branch name, never from the model.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use crate::config::CommitMessageConfig;

/// Spawn generation for `repo`'s pending diff; the result arrives on the channel. Always yields
/// exactly one message (the fallback is used on any failure). The diff itself is read on this
/// same background thread, not the caller's — a slow `git diff` on a huge change must not add to
/// the frame loop's cost any more than the `claude` call already doesn't.
pub fn spawn(repo: PathBuf, config: CommitMessageConfig) -> Receiver<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        // Read now, not from the SCM panel's last status: the message is for the branch the
        // commit will land on. Only a configured template needs it.
        let branch =
            config.branch_template().and_then(|_| crate::git::current_branch(&repo).ok().flatten());
        let prompt = prompt(&config, branch.as_deref());
        let summary = match crate::git::porcelain::diff_for_message(&repo) {
            Ok((diff, files)) => generate(&diff, &files, &config, &prompt),
            Err(_) => fallback(&[]),
        };
        let _ = tx.send(compose(summary, branch.as_deref(), &config));
    });
    rx
}

/// Appended to the instructions when the template that will wrap the reply renders `{type}`.
const TYPE_INSTRUCTION: &str = "Start the line with its Conventional Commits type (feat, fix, docs, refactor, perf, test, build, ci, style, or chore) and \": \", then the description. No scope, no ticket key.";

/// The configured instructions, plus [`TYPE_INSTRUCTION`] when a template that renders `{type}`
/// matches `branch`: the model classifies, the branch supplies everything else.
fn prompt(config: &CommitMessageConfig, branch: Option<&str>) -> String {
    let wants_type = config
        .branch_template()
        .zip(branch)
        .is_some_and(|(template, branch)| template.uses_type() && template.matches(branch));
    if wants_type {
        format!("{}\n\n{TYPE_INSTRUCTION}", config.instructions())
    } else {
        config.instructions().to_owned()
    }
}

fn generate(diff: &str, files: &[String], config: &CommitMessageConfig, prompt: &str) -> String {
    match ask_claude(diff, config, prompt) {
        Some(message) => message,
        None => fallback(files),
    }
}

/// The final message: `summary` inside the branch template when one is configured and matches
/// `branch`, else `summary` alone.
fn compose(summary: String, branch: Option<&str>, config: &CommitMessageConfig) -> String {
    config
        .branch_template()
        .zip(branch)
        .and_then(|(template, branch)| template.render(branch, &summary))
        .unwrap_or(summary)
}

/// The diff text truncated to `max_bytes`, on a UTF-8 boundary, with a trailing marker when it
/// was cut — the file list already names everything a huge diff would otherwise describe.
fn truncate_diff(diff: &str, max_bytes: usize) -> String {
    let mut input = String::with_capacity(diff.len().min(max_bytes));
    for c in diff.chars() {
        if input.len() + c.len_utf8() > max_bytes {
            input.push_str("\n[diff truncated]");
            break;
        }
        input.push(c);
    }
    input
}

/// `claude -p --model <model> <extra_args…> <prompt>`, the grammar `ask_claude` spawns —
/// pulled out so argv construction is testable without touching a process.
fn argv<'a>(config: &'a CommitMessageConfig, prompt: &'a str) -> Vec<&'a str> {
    let mut args = vec!["-p", "--model", config.model()];
    args.extend(config.extra_args().iter().map(String::as_str));
    args.push(prompt);
    args
}

/// One subject line from the `claude` CLI, or `None` on any failure.
fn ask_claude(diff: &str, config: &CommitMessageConfig, prompt: &str) -> Option<String> {
    let input = truncate_diff(diff, config.max_diff_bytes());
    let spawned = Command::new("claude")
        .args(argv(config, prompt))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = spawned else { return None };
    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        if stdin.write_all(input.as_bytes()).is_err() {
            let _ = child.kill();
            return None;
        }
        // Dropping stdin closes it so claude sees EOF.
    }
    wait_with_timeout(child, config.timeout())
}

/// Wait for the child up to `timeout`; kill it and give up on overrun.
/// Reading stdout AFTER exit is safe here because `-p` output is one short
/// line, far below any pipe buffer.
fn wait_with_timeout(mut child: Child, timeout: Duration) -> Option<String> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Ok(Some(_)) | Err(_) => return None,
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    clean_reply(&out)
}

/// The reply line, stripped of the quoting/fencing chat models sometimes add
/// despite instructions; `None` when nothing usable came back. Startup log
/// noise (MCP warnings and the like) can precede the reply on stdout, so this
/// takes the LAST usable line and drops warning-looking lines outright.
fn clean_reply(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).rfind(|l| {
        let lower = l.to_lowercase();
        !l.is_empty()
            && !l.starts_with("```")
            && !lower.contains("warn")
            && !lower.contains("error")
    })?;
    let line = line.trim_matches(['"', '\'', '`']).trim_end_matches('.').trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Filename-based fallback: good enough to save retyping, honest about scope.
fn fallback(files: &[String]) -> String {
    let name = |path: &String| path.rsplit('/').next().unwrap_or(path).to_string();
    match files {
        [] => "Update".to_string(),
        [only] => format!("Update {}", name(only)),
        [first, rest @ ..] => format!("Update {} and {} more", name(first), rest.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CommitMessageConfig {
        CommitMessageConfig::default()
    }

    #[test]
    fn argv_uses_the_configured_model_and_extra_args() {
        let config = config();
        let args = argv(&config, config.instructions());
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "--model");
        assert_eq!(args[2], "haiku");
        assert_eq!(args[3], "--strict-mcp-config");
        assert_eq!(args.last(), Some(&config.instructions()));
    }

    #[test]
    fn argv_reflects_a_custom_model() {
        let config = CommitMessageConfig::for_test("sonnet", &[], 16 * 1024, 60, "prompt");
        assert_eq!(argv(&config, config.instructions()), vec!["-p", "--model", "sonnet", "prompt"]);
    }

    #[test]
    fn argv_with_empty_extra_args_carries_only_the_prompt_after_the_model() {
        let config = CommitMessageConfig::for_test("haiku", &[], 16 * 1024, 60, "prompt");
        assert_eq!(argv(&config, config.instructions()), vec!["-p", "--model", "haiku", "prompt"]);
    }

    #[test]
    fn diff_is_truncated_exactly_at_the_byte_cap_on_a_utf8_boundary() {
        // Each "é" is 2 bytes; a cap that would split the last character keeps only whole ones.
        let diff = "é".repeat(10); // 20 bytes
        let truncated = truncate_diff(&diff, 7);
        assert!(truncated.starts_with("ééé")); // 6 bytes kept, the cap (7) never split mid-char
        assert!(truncated.ends_with("\n[diff truncated]"));
        assert!(truncated.is_char_boundary(truncated.len()));

        // A diff at or under the cap is untouched.
        assert_eq!(truncate_diff("short", 100), "short");
    }

    #[test]
    fn reply_cleanup_strips_quotes_fences_and_periods() {
        assert_eq!(clean_reply("Add sidebar merge\n"), Some("Add sidebar merge".into()));
        assert_eq!(clean_reply("\"Fix the thing.\""), Some("Fix the thing".into()));
        assert_eq!(
            clean_reply("```\nRefactor launch flow\n```"),
            Some("Refactor launch flow".into())
        );
        assert_eq!(clean_reply("   \n\n"), None);
        assert_eq!(
            clean_reply("RendererWarning resource UID duplicate\nAdd auth docs\n"),
            Some("Add auth docs".into())
        );
        assert_eq!(clean_reply("[WARN] something\nERROR: nope\n"), None);
    }

    #[test]
    fn compose_wraps_the_summary_only_on_a_matching_branch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[commit_message]
branch_pattern = '(?P<ticket>IDC-\d+)-(?P<scope>.+)'
template = "feat({scope|lower}): {ticket} {summary}\n\nTicket: {ticket}"
"#,
        )
        .unwrap();
        let templated =
            crate::config::plugin_config_in(dir.path()).unwrap().commit_message().clone();

        // TPL-MATCH, and TPL-FALLBACK: the filename fallback is a summary like any other.
        assert_eq!(
            compose("Add retry".into(), Some("PN-IDC-29308-AI"), &templated),
            "feat(ai): IDC-29308 Add retry\n\nTicket: IDC-29308"
        );
        assert_eq!(
            compose(fallback(&["src/app.rs".into()]), Some("IDC-1-X"), &templated),
            "feat(x): IDC-1 Update app.rs\n\nTicket: IDC-1"
        );
        // TPL-PLAIN: no match, no branch, no template.
        assert_eq!(compose("Add retry".into(), Some("main"), &templated), "Add retry");
        assert_eq!(compose("Add retry".into(), None, &templated), "Add retry");
        assert_eq!(compose("Add retry".into(), Some("IDC-1-X"), &config()), "Add retry");
    }

    #[test]
    fn the_type_instruction_is_added_only_for_a_matching_type_template() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[commit_message]
instructions = "base"
branch_pattern = '(?P<ticket>IDC-\d+)-(?P<scope>.+)'
template = "{type}({scope|lower}): {ticket} {summary}"
"#,
        )
        .unwrap();
        let typed = crate::config::plugin_config_in(dir.path()).unwrap().commit_message().clone();

        assert_eq!(prompt(&typed, Some("IDC-1-X")), format!("base\n\n{TYPE_INSTRUCTION}"));
        assert_eq!(prompt(&typed, Some("main")), "base");
        assert_eq!(prompt(&typed, None), "base");
        assert_eq!(prompt(&config(), Some("IDC-1-X")), config().instructions());
        assert_eq!(
            compose("docs: log session".into(), Some("IDC-7-DCO"), &typed),
            "docs(dco): IDC-7 log session"
        );
    }

    #[test]
    fn fallback_names_the_files_when_the_cli_is_unavailable() {
        assert_eq!(fallback(&[]), "Update");
        assert_eq!(fallback(&["src/app.rs".into()]), "Update app.rs");
        assert_eq!(
            fallback(&["src/app.rs".into(), "b".into(), "c".into()]),
            "Update app.rs and 2 more"
        );
    }

    #[test]
    fn a_repo_hq_deck_toml_takes_priority_over_the_global_commit_message_config() {
        use crate::config::commit_message_config;
        let config_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            config_dir.path().join("config.toml"),
            "[commit_message]\nmodel = \"global-model\"\n",
        )
        .unwrap();
        let plugin = crate::config::plugin_config_in(config_dir.path()).unwrap();
        assert_eq!(plugin.commit_message().model(), "global-model");

        // No override file: the repo resolves to the global section unchanged.
        let repo = tempfile::tempdir().unwrap();
        let resolved = commit_message_config(&plugin, repo.path()).unwrap();
        assert_eq!(resolved.model(), "global-model");

        // A repo-root override replaces only the fields it names; the rest keeps the global
        // section's values (`extra_args` here is untouched).
        std::fs::write(
            repo.path().join(".hq-deck.toml"),
            "[commit_message]\nmodel = \"repo-model\"\n",
        )
        .unwrap();
        let resolved = commit_message_config(&plugin, repo.path()).unwrap();
        assert_eq!(resolved.model(), "repo-model");
        assert_eq!(resolved.extra_args(), plugin.commit_message().extra_args());

        // An invalid override is reported, not silently ignored or falling back to the global
        // section.
        std::fs::write(repo.path().join(".hq-deck.toml"), "[commit_message]\nmodel = 7\n").unwrap();
        let error = commit_message_config(&plugin, repo.path()).unwrap_err().to_string();
        assert!(error.contains("commit_message.model"), "{error}");
    }

    #[test]
    fn instructions_file_and_inline_instructions_are_mutually_exclusive() {
        use crate::config::plugin_config_in;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("prompt.txt"), "from the shared file").unwrap();

        // `instructions_file` alone resolves to the file's (trimmed) content.
        std::fs::write(
            dir.path().join("config.toml"),
            "[commit_message]\ninstructions_file = \"prompt.txt\"\n",
        )
        .unwrap();
        let config = plugin_config_in(dir.path()).unwrap();
        assert_eq!(config.commit_message().instructions(), "from the shared file");

        // Both together is an invalid file, not an unspecified precedence.
        std::fs::write(
            dir.path().join("config.toml"),
            "[commit_message]\ninstructions = \"inline\"\ninstructions_file = \"prompt.txt\"\n",
        )
        .unwrap();
        let error = plugin_config_in(dir.path()).unwrap_err().to_string();
        assert!(error.contains("not both"), "{error}");
    }
}
