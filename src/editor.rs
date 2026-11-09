//! Resolving the external editor command for `edit` on a file.
//!
//! The one source is the `editor` config key. Without it, `edit` opens the built-in editor
//! instead (`src/edit_buffer.rs`), so `$VISUAL`/`$EDITOR` are never read: a key that behaved
//! differently depending on which shell started hq-deck would be a key nobody could predict.
//! A value that names `{file}` or `{line}` is a template the user owns outright. A bare command
//! (`micro`, `code --wait`) gets the line in the argument dialect its binary name selects.
//! This module is process-free: it builds an argv, and `src/lib.rs` spawns it.

use std::path::Path;

/// How an editor spells "open this file at this line".
///
/// Four shapes cover every editor in [`DIALECTS`]. Sources: lazygit's editor presets, Julia's
/// `InteractiveUtils` editor table, and each vendor's own CLI documentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineArg {
    /// `+42 <path>` — the vi family, nano, micro, kakoune, emacs, `BBEdit`, gedit.
    Plus,
    /// `<path>:42` — helix, Zed, Sublime Text.
    Suffix,
    /// `-g <path>:42` — the VS Code family.
    Goto,
    /// `--line 42 <path>` — the `JetBrains` family, Xcode, Kate, `TextMate`.
    Flag,
}

/// One editor family: the binary names that select it, how it takes a line, and where it draws.
///
/// A window editor hands the file to an instance of its own and returns, so reviewr keeps the
/// pane. A terminal editor draws in the pane and is given it.
struct Dialect {
    names: &'static [&'static str],
    line: LineArg,
    window: bool,
}

const DIALECTS: &[Dialect] = &[
    // Terminal editors. Each holds the pane until it exits.
    Dialect {
        names: &["vi", "vim", "nvim", "lvim", "vis", "joe"],
        line: LineArg::Plus,
        window: false,
    },
    Dialect { names: &["nano", "micro", "kak"], line: LineArg::Plus, window: false },
    // Emacs is whichever its build and `DISPLAY` make it, and neither is readable from here.
    // The pane is the survivable guess: a windowed Emacs handed the pane leaves the pane blank
    // until it quits, where a terminal Emacs denied it is invisible.
    Dialect { names: &["emacs", "emacsclient"], line: LineArg::Plus, window: false },
    Dialect { names: &["hx", "helix"], line: LineArg::Suffix, window: false },
    // MacVim and gVim open a window and return, unlike every other vi-family binary.
    Dialect { names: &["mvim", "gvim"], line: LineArg::Plus, window: true },
    // Graphical editors.
    Dialect {
        names: &["code", "code-insiders", "codium", "vscodium", "cursor", "windsurf", "positron"],
        line: LineArg::Goto,
        window: true,
    },
    Dialect { names: &["subl", "sublime_text"], line: LineArg::Suffix, window: true },
    // Plain `zed` collides with the OpenZFS event daemon, so Linux packages ship the CLI
    // under a name of their own.
    Dialect { names: &["zed", "zeditor", "zedit"], line: LineArg::Suffix, window: true },
    Dialect { names: &["bbedit", "gedit"], line: LineArg::Plus, window: true },
    Dialect { names: &["mate"], line: LineArg::Flag, window: true },
    // On Linux `xed` is Mint's X-Apps editor, a gedit fork that takes `+LINE` and rejects
    // `--line` outright.
    Dialect { names: &["xed"], line: LineArg::Plus, window: true },
    Dialect { names: &["kate"], line: LineArg::Flag, window: true },
    Dialect {
        names: &[
            "idea",
            "pycharm",
            "webstorm",
            "goland",
            "clion",
            "phpstorm",
            "rubymine",
            "rider",
            "datagrip",
            "rustrover",
            "dataspell",
            "fleet",
        ],
        line: LineArg::Flag,
        window: true,
    },
];

/// A resolved editor invocation: the program to run, its full argument list, and whether it
/// wants the terminal.
///
/// A terminal editor paints in the pane and must be handed it outright. A window one draws in an
/// instance of its own and never reads the terminal, so reviewr keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorCommand {
    pub program: String,
    pub args: Vec<String>,
    pub wants_terminal: bool,
}

/// The configured value names no program: its first word is empty.
#[derive(Debug, PartialEq, Eq)]
pub struct NoProgram;

/// The command that opens `path` at `line`, from the `editor` config key's `value`.
///
/// A value naming `{file}` or `{line}` is the whole command, and both substitute wherever they
/// appear. A template that does not name `{file}` gets the path appended.
///
/// A value naming neither is a bare command, and its binary name selects a dialect for the
/// line, so `editor = "micro"` opens the file at the line. An unrecognized binary opens the
/// file without a line rather than guessing a flag it may not accept. The binary name also
/// decides who owns the pane, in every path ([`wants_terminal`]).
///
/// `path` must be absolute. That is what keeps the dialects that append it bare from handing an
/// editor a file name it reads as a flag, so no dialect needs a `--` guard.
pub fn resolve(value: &str, path: &Path, line: u32) -> Result<EditorCommand, NoProgram> {
    let file = path.to_string_lossy().into_owned();
    if value.contains("{file}") || value.contains("{line}") {
        return from_template(value, &file, line);
    }
    let mut words = split_command(value).into_iter();
    // The same guard the template gets: a value of two quote characters is not empty and
    // splits to one empty word, and handing that to the pane would flip the screen for a
    // spawn that cannot succeed.
    let Some(program) = words.next().filter(|p| !p.is_empty()) else {
        return Err(NoProgram);
    };
    let mut args: Vec<String> = words.collect();
    match dialect_for(&program).map(|d| d.line) {
        None => args.push(file),
        Some(LineArg::Plus) => {
            args.push(format!("+{line}"));
            args.push(file);
        }
        Some(LineArg::Suffix) => args.push(format!("{file}:{line}")),
        Some(LineArg::Goto) => {
            args.push("-g".to_owned());
            args.push(format!("{file}:{line}"));
        }
        Some(LineArg::Flag) => {
            args.push("--line".to_owned());
            args.push(line.to_string());
            args.push(file);
        }
    }
    let wants_terminal = wants_terminal(&program, &args);
    Ok(EditorCommand { program, args, wants_terminal })
}

/// Whether the command draws in the pane it was launched from.
///
/// Two signals, asked in the same order in every path. A command already carrying a wait flag
/// names a window editor whatever its binary is called: no terminal editor has one, because
/// blocking is what a terminal editor inherently does. Long forms only, since `-w` and `-b` are
/// ordinary short flags elsewhere (helix spells `--working-dir` as `-w`).
///
/// Failing that, the binary's own name. Unknown means the pane, which is the outcome a terminal
/// editor cannot survive being denied.
fn wants_terminal(program: &str, args: &[String]) -> bool {
    if args.iter().any(|a| a == "--wait" || a == "--block") {
        return false;
    }
    dialect_for(program).is_none_or(|d| !d.window)
}

/// Split a command into words, honouring quotes.
///
/// A plain whitespace split cannot express `/Applications/Sublime Text.app/.../subl`, which is
/// how macOS spells most editor paths. Quoting is the only escape, since no shell runs the
/// command. A quote closes at its match or at the end of the string.
fn split_command(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    for ch in value.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => word.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                started = true;
            }
            None if ch.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            None => {
                word.push(ch);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// The dialect for a binary, matched on its file name so an absolute path resolves too.
fn dialect_for(program: &str) -> Option<&'static Dialect> {
    let name = Path::new(program).file_name()?.to_string_lossy().to_lowercase();
    DIALECTS.iter().find(|d| d.names.contains(&name.as_str()))
}

/// Build the command from a user template, substituting every `{file}` and `{line}`.
///
/// `{line}` goes first, and that ordering is the whole trick: a path is only ever substituted
/// into text nothing looks at again, so a file named `{line}.cshtml` stays a file name rather
/// than becoming a second placeholder.
///
/// The config layer rejects an empty value, but a value of two quote characters is not empty and
/// splits to one empty word, which names no program.
fn from_template(template: &str, file: &str, line: u32) -> Result<EditorCommand, NoProgram> {
    let named_file = template.contains("{file}");
    let line = line.to_string();
    let substitute = |w: String| w.replace("{line}", &line).replace("{file}", file);
    let mut words = split_command(template).into_iter().map(substitute);
    let Some(program) = words.next().filter(|p| !p.is_empty()) else {
        return Err(NoProgram);
    };
    let mut args: Vec<String> = words.collect();
    if !named_file {
        args.push(file.to_owned());
    }
    let wants_terminal = wants_terminal(&program, &args);
    Ok(EditorCommand { program, args, wants_terminal })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("/repo/src/lib.rs")
    }

    /// A configured value with no placeholder: its binary picks the dialect.
    fn bare(value: &str) -> Option<EditorCommand> {
        resolve(value, &p(), 41).ok()
    }

    fn template(value: &str) -> EditorCommand {
        resolve(value, &p(), 41).unwrap()
    }

    fn argv(cmd: &EditorCommand) -> String {
        format!("{} {}", cmd.program, cmd.args.join(" "))
    }

    #[test]
    fn terminal_editors_take_a_plus_line_and_never_wait() {
        for name in ["vi", "vim", "nvim", "nano", "micro", "kak", "emacs"] {
            assert_eq!(
                argv(&bare(name).unwrap()),
                format!("{name} +41 /repo/src/lib.rs"),
                "{name} takes `+LINE` before the path and owns the pane already"
            );
        }
    }

    #[test]
    fn helix_takes_the_line_as_a_path_suffix() {
        assert_eq!(argv(&bare("hx").unwrap()), "hx /repo/src/lib.rs:41");
        assert_eq!(argv(&bare("helix").unwrap()), "helix /repo/src/lib.rs:41");
    }

    #[test]
    fn window_editors_get_their_line_and_nothing_else() {
        // reviewr adds no flag of its own: the launcher hands the file over and returns, and
        // nothing waits on it.
        for name in ["code", "code-insiders", "codium", "cursor", "windsurf", "positron"] {
            assert_eq!(argv(&bare(name).unwrap()), format!("{name} -g /repo/src/lib.rs:41"));
        }
        assert_eq!(argv(&bare("zed").unwrap()), "zed /repo/src/lib.rs:41");
        assert_eq!(argv(&bare("subl").unwrap()), "subl /repo/src/lib.rs:41");
        assert_eq!(argv(&bare("bbedit").unwrap()), "bbedit +41 /repo/src/lib.rs");
        assert_eq!(argv(&bare("mate").unwrap()), "mate --line 41 /repo/src/lib.rs");
        assert_eq!(argv(&bare("kate").unwrap()), "kate --line 41 /repo/src/lib.rs");
        assert_eq!(argv(&bare("xed").unwrap()), "xed +41 /repo/src/lib.rs");
        for name in ["idea", "pycharm", "webstorm", "goland", "clion", "rustrover", "fleet"] {
            assert_eq!(argv(&bare(name).unwrap()), format!("{name} --line 41 /repo/src/lib.rs"));
        }
    }

    #[test]
    fn only_a_terminal_editor_is_handed_the_pane() {
        // The one question the argv does not answer, so nothing else in this module asserts it:
        // which of the two paths `run_editor` takes.
        for name in ["vim", "nvim", "nano", "micro", "kak", "emacs", "hx", "helix"] {
            assert!(bare(name).unwrap().wants_terminal, "{name} paints in the pane");
        }
        // Argv-identical to a terminal editor of the same dialect, so only this tells them
        // apart: `zed`/`subl` against `hx`, `bbedit`/`gedit`/`mvim` against `vim`.
        for name in ["code", "cursor", "zed", "zeditor", "subl", "idea", "kate", "mate"] {
            assert!(!bare(name).unwrap().wants_terminal, "{name} opens a window");
        }
        for name in ["mvim", "gvim", "bbedit", "gedit"] {
            assert!(!bare(name).unwrap().wants_terminal, "{name} opens a window");
        }
        // A configured command spells its own arguments, but it is still one of these
        // binaries, and the same name answers the same question.
        let cfg = |t: &str| template(t).wants_terminal;
        assert!(!cfg("code -g {file}:{line}"), "the documented example keeps the pane");
        assert!(cfg("vim +{line} {file}"), "a terminal editor still takes it");
        // A wait flag the reviewer wrote names a window editor whatever the binary is called:
        // Zed's own bundle ships its CLI as `cli`, and that must not cost the reviewer the pane.
        assert!(!cfg("/Applications/Zed.app/Contents/MacOS/cli --wait {file}:{line}"));
        assert!(!cfg("/opt/weird/ed --block --line {line} {file}"));
        assert!(!bare("/Applications/Zed.app/Contents/MacOS/cli --wait").unwrap().wants_terminal);
        // Short spellings are ordinary flags elsewhere, so they say nothing: helix's `-w` is
        // `--working-dir`.
        assert!(cfg("hx -w {file}"));
        // An unknown binary says nothing, and only one of the two guesses is survivable.
        assert!(bare("myeditor").unwrap().wants_terminal);
        assert!(cfg("myed {file}"));
    }

    #[test]
    fn the_reviewers_own_flags_survive() {
        // `code --wait` is the documented git setup, so it arrives already waiting.
        assert_eq!(argv(&bare("code --wait").unwrap()), "code --wait -g /repo/src/lib.rs:41");
        assert_eq!(argv(&bare("kate -b").unwrap()), "kate -b --line 41 /repo/src/lib.rs");
        assert_eq!(argv(&bare("mvim -f").unwrap()), "mvim -f +41 /repo/src/lib.rs");
    }

    #[test]
    fn a_quoted_path_with_spaces_stays_one_word() {
        // The common macOS spelling. No shell runs the command, so quoting is the only escape.
        let subl = "/Applications/Sublime Text.app/Contents/SharedSupport/bin/subl";
        let cmd = bare(&format!("\"{subl}\"")).unwrap();
        assert_eq!(cmd.program, subl, "the whole quoted path is the program");
        assert_eq!(
            cmd.args,
            ["/repo/src/lib.rs:41"],
            "and the quoted path's own name still picks the dialect"
        );

        // Single quotes too, and a quoted argument after the program.
        let cmd = bare(&format!("'{subl}' --project 'My Project.sublime-project'")).unwrap();
        assert_eq!(cmd.program, subl);
        assert_eq!(cmd.args, ["--project", "My Project.sublime-project", "/repo/src/lib.rs:41"]);

        // A template quotes the same way.
        let cmd = template("'/opt/my editor' --at {line} {file}");
        assert_eq!(cmd.program, "/opt/my editor");
        assert_eq!(cmd.args, ["--at", "41", "/repo/src/lib.rs"]);

        // An unterminated quote closes at the end rather than dropping the word.
        assert_eq!(bare("\"/opt/my editor").unwrap().program, "/opt/my editor");

        // A closed empty quote is a word, so it can be the one in program position. Dropping
        // that distinction would silently run the second word as the editor instead.
        assert_eq!(resolve("'' vim", &p(), 41), Err(NoProgram));
    }

    #[test]
    fn an_unknown_editor_opens_the_file_without_a_line() {
        assert_eq!(
            argv(&bare("myeditor").unwrap()),
            "myeditor /repo/src/lib.rs",
            "guessing a flag an unknown editor may not accept would open a stray buffer"
        );
    }

    #[test]
    fn a_value_of_empty_quotes_names_no_program() {
        // Not empty, so config validation lets it through, but it splits to one empty word. The
        // pane must not change hands for a spawn that cannot happen, in either path.
        assert_eq!(resolve("\"\"", &p(), 41), Err(NoProgram));
        assert_eq!(resolve("\"\" {file}", &p(), 41), Err(NoProgram));
    }

    #[test]
    fn a_template_owns_the_whole_command() {
        assert_eq!(
            argv(&template("code -g {file}:{line}")),
            "code -g /repo/src/lib.rs:41",
            "a template takes no added flags"
        );
        assert_eq!(
            argv(&template("idea --line {line} --wait {file}")),
            "idea --line 41 --wait /repo/src/lib.rs"
        );
        assert_eq!(
            argv(&template("myed --goto {line}")),
            "myed --goto 41 /repo/src/lib.rs",
            "a template naming no `{{file}}` still gets the path"
        );
        assert_eq!(
            argv(&template("myed {line} {file} {line}")),
            "myed 41 /repo/src/lib.rs 41",
            "every occurrence substitutes"
        );
        // `{line}` substitutes before `{file}`, so a path is only ever placed into text nothing
        // reads again. Swap the two and this file name becomes a second placeholder.
        let odd = PathBuf::from("/repo/{line}.cshtml");
        assert_eq!(
            argv(&resolve("myed {file}:{line}", &odd, 41).unwrap()),
            "myed /repo/{line}.cshtml:41",
            "a path that spells a placeholder stays a path"
        );
    }

    #[test]
    fn a_bare_command_opens_at_the_line_in_its_own_dialect() {
        assert_eq!(argv(&template("micro")), "micro +41 /repo/src/lib.rs");
        assert_eq!(argv(&template("hx")), "hx /repo/src/lib.rs:41");
        assert_eq!(argv(&template("code --wait")), "code --wait -g /repo/src/lib.rs:41");
    }
}
