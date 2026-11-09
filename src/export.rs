//! Formatting comments and exporting them to the clipboard.
//!
//! A comment becomes a block of `location`, the
//! diff snippet, then the text. Export is consume-on-success: the caller removes
//! a comment only after `export` returns `Ok`.

use std::io::Write;
use std::process::Stdio;

use anyhow::{Context, Result, bail};

use crate::model::Comment;

/// One comment as its export block: location, snippet, then text.
pub fn format_comment(comment: &Comment) -> String {
    format!("{}\n{}\n{}", comment.location(), comment.lines, normalize_text(&comment.text))
}

/// Comment text for export: drop `\r`, trim trailing space per line, and drop blank
/// lines so a multi-line comment can never introduce the blank-line block separator.
fn normalize_text(text: &str) -> String {
    text.replace('\r', "")
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Many comments, sorted by file then start line, one blank line between blocks.
pub fn format_all(comments: &[&Comment]) -> String {
    let mut sorted = comments.to_vec();
    sorted.sort_by(|a, b| a.file.cmp(&b.file).then(a.start.cmp(&b.start)));
    sorted.iter().map(|c| format_comment(c)).collect::<Vec<_>>().join("\n\n")
}

/// A destination comments can be exported to. Export succeeds or errors as a whole.
pub trait ExportTarget {
    fn export(&self, text: &str) -> Result<()>;
    fn label(&self) -> &'static str;
    /// Destination-specific confirmation shown after a successful export.
    fn success_message(&self, count: usize) -> String;
    /// Destination-specific line shown after a failed one. It is the whole status, so it is one
    /// short sentence a reviewer can read, never the underlying error. The cause goes to the log.
    fn failure_message(&self) -> String;
    /// The failure line for this particular error; a target whose errors say what to do next
    /// overrides it. The default ignores the error.
    fn failure_message_for(&self, err: &anyhow::Error) -> String {
        let _ = err;
        self.failure_message()
    }
}

fn counted_comments(count: usize) -> String {
    let noun = if count == 1 { "comment" } else { "comments" };
    format!("{count} {noun}")
}

/// A clipboard tool and the args that make it read stdin into the system clipboard. Tried in
/// order - the first one present on `PATH` wins (Wayland `wl-copy`, or X11 `xclip`/`xsel`).
const CLIPBOARD_TOOLS: &[(&str, &[&str])] = &[
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
];

/// The reviewer's clipboard. A local display gets a clipboard tool; a session with no display
/// of its own (over ssh, or in a terminal hosted by a display-less server such as hc on a remote
/// machine) gets OSC 52, which the reviewer's terminal turns into a clipboard write wherever it
/// runs. A tool that is missing or fails also falls back to OSC 52.
#[derive(Debug)]
pub struct Clipboard;

/// How a copy reaches the reviewer's clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// A clipboard tool on this machine, falling back to the terminal on failure.
    Tool,
    /// OSC 52 through the terminal only: this machine's clipboard is not the reviewer's.
    Terminal,
}

/// The route for an environment read through `var`. A remote session (`SSH_CONNECTION` or
/// `SSH_TTY`) or one with neither `WAYLAND_DISPLAY` nor `DISPLAY` has no local clipboard that
/// the reviewer could paste from, so a tool there would copy into nowhere or fail.
fn route(var: impl Fn(&str) -> bool) -> Route {
    let remote = var("SSH_CONNECTION") || var("SSH_TTY");
    let display = var("WAYLAND_DISPLAY") || var("DISPLAY");
    if remote || !display { Route::Terminal } else { Route::Tool }
}

/// The OSC 52 sequence that asks the terminal to put `text` on the clipboard.
fn osc52(text: &str) -> String {
    use base64::Engine as _;
    format!("\x1b]52;c;{}\x07", base64::engine::general_purpose::STANDARD.encode(text))
}

/// Write `text` to the clipboard through the controlling terminal. Between frames the
/// sequence is invisible to the screen, so it cannot disturb the painted UI. The terminal
/// gives no acknowledgement, so this succeeds once the sequence is written.
fn copy_via_terminal(text: &str) -> Result<()> {
    use std::io::IsTerminal as _;
    let mut out = std::io::stdout().lock();
    if !out.is_terminal() {
        bail!("stdout is not a terminal, so OSC 52 has nowhere to go");
    }
    out.write_all(osc52(text).as_bytes()).context("writing OSC 52")?;
    out.flush().context("flushing OSC 52")
}

/// Pipe `text` into the first clipboard tool on `PATH`.
fn copy_via_tool(text: &str) -> Result<()> {
    let (cmd, args) = select_tool(CLIPBOARD_TOOLS, crate::proc::on_path)
        .context("no clipboard tool found (install wl-clipboard, xclip, or xsel)")?;
    let mut child = crate::proc::command(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {cmd}"))?;
    child
        .stdin
        .as_mut()
        .with_context(|| format!("{cmd} stdin unavailable"))?
        .write_all(text.as_bytes())
        .with_context(|| format!("writing to {cmd}"))?;
    if !child.wait().with_context(|| format!("waiting for {cmd}"))?.success() {
        bail!("{cmd} exited non-zero");
    }
    Ok(())
}

impl ExportTarget for Clipboard {
    fn label(&self) -> &'static str {
        "clipboard"
    }

    fn success_message(&self, count: usize) -> String {
        format!("copied {}", counted_comments(count))
    }

    fn failure_message(&self) -> String {
        "clipboard failed".to_string()
    }

    fn export(&self, text: &str) -> Result<()> {
        match route(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty())) {
            Route::Terminal => copy_via_terminal(text),
            Route::Tool => copy_via_tool(text).or_else(|tool_err| {
                crate::logln!("clipboard tool failed, trying OSC 52: {tool_err:#}");
                copy_via_terminal(text).map_err(|e| e.context(format!("{tool_err:#}")))
            }),
        }
    }
}

/// The clipboard tools that read the clipboard back, paired with [`CLIPBOARD_TOOLS`] in the same
/// order, so a paste reads the clipboard a copy wrote.
const PASTE_TOOLS: &[(&str, &[&str])] = &[
    ("wl-paste", &["--no-newline"]),
    ("xclip", &["-selection", "clipboard", "-o"]),
    ("xsel", &["--clipboard", "--output"]),
];

/// The reviewer's clipboard text, for the built-in editor's `ctrl+v`. `None` where this machine's
/// clipboard is not the reviewer's (over ssh, or with no display: OSC 52 cannot be read back), or
/// where no tool answers with text. The first present tool that succeeds wins, so an X11 session
/// with `wl-clipboard` installed still reads through `xclip`.
pub fn read_clipboard() -> Option<String> {
    if route(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty())) == Route::Terminal {
        return None;
    }
    PASTE_TOOLS.iter().filter(|(cmd, _)| crate::proc::on_path(cmd)).find_map(|(cmd, args)| {
        let out = crate::proc::command(cmd)
            .args(*args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout).ok()
    })
}

/// The first clipboard tool the `present` predicate accepts, preserving list order.
fn select_tool(
    tools: &'static [(&'static str, &'static [&'static str])],
    present: impl Fn(&str) -> bool,
) -> Option<(&'static str, &'static [&'static str])> {
    tools.iter().copied().find(|(cmd, _)| present(cmd))
}

#[cfg(test)]
mod tests {
    use super::{
        CLIPBOARD_TOOLS, Clipboard, ExportTarget, Route, format_all, format_comment, osc52, route,
        select_tool,
    };
    use crate::model::{Comment, Side};

    #[test]
    fn route_uses_a_tool_only_on_a_local_display() {
        let env = |set: &'static [&'static str]| move |name: &str| set.contains(&name);
        assert_eq!(route(env(&["WAYLAND_DISPLAY"])), Route::Tool);
        assert_eq!(route(env(&["DISPLAY"])), Route::Tool);
        // No display: a server-hosted terminal (hc) or a console - the terminal is the way out.
        assert_eq!(route(env(&[])), Route::Terminal);
        // Over ssh a forwarded display would still be the remote clipboard, not the reviewer's.
        assert_eq!(route(env(&["DISPLAY", "SSH_CONNECTION"])), Route::Terminal);
        assert_eq!(route(env(&["WAYLAND_DISPLAY", "SSH_TTY"])), Route::Terminal);
    }

    #[test]
    fn osc52_is_the_clipboard_selection_with_base64_payload() {
        assert_eq!(osc52("hello"), "\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(osc52("src/путь.rs"), "\x1b]52;c;c3JjL9C/0YPRgtGMLnJz\x07");
    }

    #[test]
    fn clipboard_tool_selection_prefers_list_order_and_can_be_empty() {
        // None present -> no tool (the caller surfaces the "install one" error).
        assert!(select_tool(CLIPBOARD_TOOLS, |_| false).is_none());
        // Only an X11 tool present -> it's chosen, with its selection args.
        assert_eq!(
            select_tool(CLIPBOARD_TOOLS, |c| c == "xclip"),
            Some(("xclip", &["-selection", "clipboard"][..]))
        );
        // When several are present, earlier in the list wins (wl-copy over xclip).
        assert_eq!(
            select_tool(CLIPBOARD_TOOLS, |c| c == "wl-copy" || c == "xclip").map(|(cmd, _)| cmd),
            Some("wl-copy")
        );
    }

    #[test]
    fn export_confirmations_name_the_actual_result_and_pluralize_comments() {
        assert_eq!(Clipboard.success_message(1), "copied 1 comment");
        assert_eq!(Clipboard.success_message(2), "copied 2 comments");
    }

    fn comment(file: &str, side: Side, start: u32, end: u32, lines: &str, text: &str) -> Comment {
        Comment {
            file: file.into(),
            side,
            start,
            end,
            lines: lines.into(),
            text: text.into(),
            diff_anchored: true,
            rev: crate::model::Rev::Worktree,
            basis: None,
            orphaned: false,
        }
    }

    #[test]
    fn block_is_location_snippet_text() {
        let c = comment(
            "extruct/core/llm_registry.py",
            Side::New,
            40,
            41,
            "-from .z import w\n+from .x import y",
            "this import path looks wrong",
        );
        assert_eq!(
            format_comment(&c),
            "extruct/core/llm_registry.py:40-41\n-from .z import w\n+from .x import y\nthis import path looks wrong"
        );
    }

    #[test]
    fn removed_side_marks_the_header() {
        let c = comment("a.rs", Side::Old, 38, 38, "-    cleanup()", "still needed");
        assert_eq!(format_comment(&c), "a.rs:38 (removed)\n-    cleanup()\nstill needed");
    }

    #[test]
    fn multiline_text_keeps_breaks_but_drops_blank_lines() {
        let c = comment("a.rs", Side::New, 1, 1, "+x", "first line\n\n  \nsecond line\n");
        assert_eq!(format_comment(&c), "a.rs:1\n+x\nfirst line\nsecond line");
    }

    #[test]
    fn all_sorts_by_file_then_start_with_blank_separator() {
        let b = comment("b.rs", Side::New, 5, 5, "+x", "two");
        let a2 = comment("a.rs", Side::New, 20, 20, "+y", "later");
        let a1 = comment("a.rs", Side::New, 3, 3, "+z", "earlier");
        let out = format_all(&[&b, &a2, &a1]);
        assert_eq!(out, "a.rs:3\n+z\nearlier\n\na.rs:20\n+y\nlater\n\nb.rs:5\n+x\ntwo");
    }
}
