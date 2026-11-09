//! Syntax highlighting via `syntect`, themed by the active theme's paired syntax theme.
//!
//! The highlighter is rebuilt when the theme
//! changes and produces per-line foreground spans; the pane keeps the terminal's own
//! background, so only token colors come from the theme.

use std::fmt;
use std::io::Cursor;

use syntect::easy::HighlightLines;
use syntect::highlighting::{
    HighlightIterator, HighlightState, Highlighter as ThemeHighlighter, Theme, ThemeSet,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use std::sync::OnceLock;

use crate::diff::{Rgb, Span};
use crate::edit_buffer::Splice;
use crate::theme::SyntaxChoice;

/// The default text color when a theme carries none, or its syntax theme fails to load.
const DEFAULT_FG: Rgb = (0xcd, 0xd6, 0xf4);

/// The broad bat/two-face syntax set, built once per process (it is expensive to
/// deserialize) and shared across every `Highlighter`.
fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

/// The two-face embedded theme set, deserialized once and shared — like [`syntaxes`], so a
/// theme switch clones one theme out of the cached set instead of rebuilding the whole dump.
fn embedded_themes() -> &'static two_face::theme::EmbeddedLazyThemeSet {
    static THEMES: OnceLock<two_face::theme::EmbeddedLazyThemeSet> = OnceLock::new();
    THEMES.get_or_init(two_face::theme::extra)
}

/// Holds the active syntax theme (absent when it failed to load); highlights file content
/// into spans against the shared syntax set.
pub struct Highlighter {
    theme: Option<Theme>,
    default_fg: Rgb,
}

impl fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Highlighter").finish_non_exhaustive()
    }
}

impl Highlighter {
    /// Build from a theme's paired syntax source: a bundled `.tmTheme` (parsed from vendored
    /// bytes), or a theme from the `two-face` embedded set. A bundled theme that fails to
    /// parse leaves the highlighter theme-less, so highlighting degrades to plain spans
    /// rather than crashing. Most files color out of the box via the
    /// broad two-face syntax set.
    pub fn new(syntax: SyntaxChoice) -> Self {
        let theme = match syntax {
            SyntaxChoice::Bundled(bytes) => {
                match ThemeSet::load_from_reader(&mut Cursor::new(bytes)) {
                    Ok(theme) => Some(theme),
                    Err(e) => {
                        crate::logln!("bundled syntax theme failed to parse: {e}");
                        None
                    }
                }
            }
            SyntaxChoice::Embedded(name) => Some(embedded_themes().get(name).clone()),
        };
        let default_fg = theme
            .as_ref()
            .and_then(|t| t.settings.foreground)
            .map_or(DEFAULT_FG, |c| (c.r, c.g, c.b));
        Self { theme, default_fg }
    }

    /// Highlight `content` line by line. Each inner `Vec` is one line's spans. With no
    /// known `language` — or no loaded theme — every line is a single plain span in the
    /// default color. `language` matches as an extension first (paths), then as a token
    /// name (markdown fence tags like `rust` or `python`).
    pub fn highlight(&self, content: &str, language: Option<&str>) -> Vec<Vec<Span>> {
        let syntaxes = syntaxes();
        let Some((syntax, theme)) = self.tools(language) else {
            return content
                .lines()
                .map(|l| vec![Span { text: l.to_string(), color: self.default_fg }])
                .collect();
        };
        let mut h = HighlightLines::new(syntax, theme);
        let mut out = Vec::new();
        for line in LinesWithEndings::from(content) {
            let spans = match h.highlight_line(line, syntaxes) {
                Ok(regions) => regions
                    .into_iter()
                    .map(|(style, text)| Span {
                        text: text.trim_end_matches('\n').to_string(),
                        color: (style.foreground.r, style.foreground.g, style.foreground.b),
                    })
                    .collect(),
                // A grammar error degrades to plain text rather than blocking the diff.
                Err(_) => vec![Span {
                    text: line.trim_end_matches('\n').to_string(),
                    color: self.default_fg,
                }],
            };
            out.push(spans);
        }
        out
    }

    /// The grammar and theme `language` highlights with, or `None` when either is missing and
    /// lines paint plain. `language` matches as an extension first (paths), then as a token
    /// name (markdown fence tags like `rust` or `python`).
    fn tools(&self, language: Option<&str>) -> Option<(&'static SyntaxReference, &Theme)> {
        let syntaxes = syntaxes();
        let syntax = language.and_then(|lang| {
            syntaxes.find_syntax_by_extension(lang).or_else(|| syntaxes.find_syntax_by_token(lang))
        })?;
        Some((syntax, self.theme.as_ref()?))
    }

    fn plain(&self, text: &str) -> Vec<Span> {
        vec![Span { text: text.to_owned(), color: self.default_fg }]
    }
}

/// The parser and highlighter state at a line boundary. Equal states at the start of a line
/// mean the line highlights the same, which is what lets an edit stop re-highlighting early.
#[derive(Clone, PartialEq, Eq)]
struct LineState {
    parse: ParseState,
    highlight: HighlightState,
}

/// One line's cached highlight and the states on either side of it. `None` states mark a
/// plain line: no grammar or no theme.
struct HighlightedLine {
    start: Option<LineState>,
    end: Option<LineState>,
    spans: Vec<Span>,
}

/// Per-line highlighting for text that changes a line at a time: the built-in editor's buffer.
///
/// [`Highlighter::highlight`] colors a whole file in one pass, which is right for a diff built
/// once and wrong for a buffer that changes on every keypress. This keeps each line's spans and
/// the parser state at its start. An edit replays as a [`Splice`] that drops the lines it
/// touched, and [`Self::ensure`] walks forward from the first unverified line, re-highlighting
/// only where a line's start state no longer matches. A typed character re-highlights its own
/// line and stops at the next; an opened block comment re-highlights down to where it closes.
///
/// The walk runs only as far as the caller asks (the bottom of the viewport), so opening a
/// large file or editing near its top costs what is on screen.
pub struct LineHighlights {
    language: Option<String>,
    lines: Vec<Option<HighlightedLine>>,
    /// Every line before this index holds a highlight that is correct for the current text.
    verified: usize,
}

impl fmt::Debug for LineHighlights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LineHighlights")
            .field("language", &self.language)
            .field("lines", &self.lines.len())
            .field("verified", &self.verified)
            .finish_non_exhaustive()
    }
}

impl LineHighlights {
    /// An empty cache for `line_count` lines of a file in `language` (its extension).
    pub fn new(language: Option<&str>, line_count: usize) -> Self {
        let mut lines = Vec::new();
        lines.resize_with(line_count, || None);
        Self { language: language.map(str::to_owned), lines, verified: 0 }
    }

    /// Follow one edit of the text: the spliced lines lose their highlight, and nothing from
    /// the first of them on is verified any more.
    pub fn splice(&mut self, splice: Splice) {
        let end = (splice.start + splice.removed).min(self.lines.len());
        let start = splice.start.min(end);
        self.lines.splice(start..end, std::iter::repeat_with(|| None).take(splice.inserted));
        self.verified = self.verified.min(start);
    }

    /// Forget every highlight, as a theme change requires.
    pub fn clear(&mut self) {
        self.lines.iter_mut().for_each(|line| *line = None);
        self.verified = 0;
    }

    /// Make the highlight of lines `..upto` correct for `text`, re-highlighting only the lines
    /// whose start state changed. `text` must be the lines the splices so far describe.
    pub fn ensure(&mut self, hl: &Highlighter, text: &[String], upto: usize) {
        debug_assert_eq!(self.lines.len(), text.len(), "the splices kept the cache aligned");
        let upto = upto.min(text.len()).min(self.lines.len());
        if self.verified >= upto {
            return;
        }
        let tools = hl.tools(self.language.as_deref());
        let theme = tools.map(|(_, theme)| ThemeHighlighter::new(theme));
        let mut state: Option<LineState> = match self.verified.checked_sub(1) {
            Some(prev) => self.lines[prev].as_ref().and_then(|line| line.end.clone()),
            None => tools.zip(theme.as_ref()).map(|((syntax, _), theme)| LineState {
                parse: ParseState::new(syntax),
                highlight: HighlightState::new(theme, ScopeStack::new()),
            }),
        };
        for (i, line_text) in text.iter().enumerate().take(upto).skip(self.verified) {
            if let Some(line) = &self.lines[i]
                && line.start == state
            {
                state.clone_from(&line.end);
                continue;
            }
            let (spans, end) = match (&state, &theme) {
                (Some(start), Some(theme)) => highlight_line(start, line_text, theme, hl),
                _ => (hl.plain(line_text), None),
            };
            self.lines[i] = Some(HighlightedLine { start: state, end: end.clone(), spans });
            state = end;
        }
        self.verified = upto;
    }

    /// How many lines the cache holds a slot for: the text's, while the splices kept it aligned.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The spans of `line`, once [`Self::ensure`] has covered it.
    pub fn spans(&self, line: usize) -> Option<&[Span]> {
        self.lines.get(line)?.as_ref().map(|l| l.spans.as_slice())
    }
}

/// Highlight one line from `start`, returning its spans and the state after it. A grammar error
/// paints the line plain and carries the state through unchanged, like the whole-file pass.
fn highlight_line(
    start: &LineState,
    text: &str,
    theme: &ThemeHighlighter<'_>,
    hl: &Highlighter,
) -> (Vec<Span>, Option<LineState>) {
    let mut state = start.clone();
    // Grammars match line ends against the newline, so every line is fed with one.
    let line = format!("{text}\n");
    let Ok(ops) = state.parse.parse_line(&line, syntaxes()) else {
        return (hl.plain(text), Some(start.clone()));
    };
    let spans = HighlightIterator::new(&mut state.highlight, &ops, &line, theme)
        .map(|(style, piece)| Span {
            text: piece.trim_end_matches('\n').to_owned(),
            color: (style.foreground.r, style.foreground.g, style.foreground.b),
        })
        .filter(|span| !span.text.is_empty())
        .collect();
    (spans, Some(state))
}

#[cfg(test)]
mod tests {
    use super::Highlighter;
    use crate::theme;

    /// The bundled Catppuccin Mocha syntax, the default theme's pairing.
    fn mocha() -> super::SyntaxChoice {
        theme::resolve(Some("catppuccin")).syntax
    }

    #[test]
    fn highlights_rust_into_colored_spans() {
        let h = Highlighter::new(mocha());
        let lines = h.highlight("let x = 1;\n", Some("rs"));
        assert_eq!(lines.len(), 1);
        let spans = &lines[0];
        assert!(spans.len() > 1, "rust tokenizes into several spans");
        assert_eq!(spans.iter().map(|s| s.text.as_str()).collect::<String>(), "let x = 1;");
        // The Catppuccin keyword color (purple) differs from the default text color.
        assert!(spans.iter().any(|s| s.text == "let" && s.color != (0xcd, 0xd6, 0xf4)));
    }

    #[test]
    fn unknown_language_is_one_plain_span_per_line() {
        let h = Highlighter::new(mocha());
        let lines = h.highlight("alpha\nbeta\n", None);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], vec![super::Span { text: "alpha".into(), color: (0xcd, 0xd6, 0xf4) }]);
    }

    /// A line's spans with the empty ones dropped, for comparing the two highlight paths: the
    /// whole-file pass keeps the empty span its newline leaves, the incremental one drops it.
    fn solid(spans: &[super::Span]) -> Vec<(String, crate::diff::Rgb)> {
        spans.iter().filter(|s| !s.text.is_empty()).map(|s| (s.text.clone(), s.color)).collect()
    }

    /// The incremental cache, brought up to date with `buffer` and compared line by line with
    /// a fresh whole-file highlight of the same text.
    fn assert_matches_whole_file(
        h: &Highlighter,
        cache: &mut super::LineHighlights,
        buffer: &mut crate::edit_buffer::Buffer,
        lang: &str,
    ) {
        for splice in buffer.take_splices() {
            cache.splice(splice);
        }
        cache.ensure(h, buffer.lines(), usize::MAX);
        let whole = h.highlight(&format!("{}\n", buffer.lines().join("\n")), Some(lang));
        for (i, expected) in whole.iter().enumerate() {
            assert_eq!(
                solid(cache.spans(i).expect("ensured")),
                solid(expected),
                "line {i} of {:?}",
                buffer.lines()
            );
        }
    }

    #[test]
    fn incremental_highlight_matches_a_whole_file_pass_after_edits() {
        use crate::edit_buffer::Buffer;
        let h = Highlighter::new(mocha());
        let mut buffer = Buffer::parse(b"fn a() {\n    let x = 1;\n}\n\nfn b() {}\n").unwrap();
        let mut cache = super::LineHighlights::new(Some("rs"), buffer.line_count());
        assert_matches_whole_file(&h, &mut cache, &mut buffer, "rs");
        // Open a block comment on line 2: everything below turns into comment.
        buffer.down(1, false);
        buffer.home(false);
        buffer.insert("/* ");
        assert_matches_whole_file(&h, &mut cache, &mut buffer, "rs");
        // Close it on the last line: the lines after it come back.
        buffer.doc_end(false);
        buffer.insert(" */\nlet y = \"s\";");
        assert_matches_whole_file(&h, &mut cache, &mut buffer, "rs");
        // Undo both, and cut a line out.
        buffer.undo();
        buffer.undo();
        buffer.doc_start(false);
        buffer.cut_line();
        assert_matches_whole_file(&h, &mut cache, &mut buffer, "rs");
    }

    #[test]
    fn an_edit_rehighlights_only_until_the_state_settles() {
        use crate::edit_buffer::Buffer;
        let h = Highlighter::new(mocha());
        let text = (0..200).map(|i| format!("let v{i} = {i};")).collect::<Vec<_>>().join("\n");
        let mut buffer = Buffer::parse(text.as_bytes()).unwrap();
        let mut cache = super::LineHighlights::new(Some("rs"), buffer.line_count());
        cache.ensure(&h, buffer.lines(), usize::MAX);
        let before: Vec<_> =
            (0..buffer.line_count()).map(|i| solid(cache.spans(i).unwrap())).collect();
        buffer.down(5, false);
        buffer.type_char('x');
        for splice in buffer.take_splices() {
            cache.splice(splice);
        }
        assert!(cache.spans(5).is_none(), "the edited line lost its highlight");
        assert!(cache.spans(6).is_some(), "the line after it kept its own");
        cache.ensure(&h, buffer.lines(), usize::MAX);
        for i in (0..buffer.line_count()).filter(|&i| i != 5) {
            assert_eq!(solid(cache.spans(i).unwrap()), before[i], "line {i} is untouched");
        }
    }

    #[test]
    fn ensure_stops_at_the_requested_line() {
        let h = Highlighter::new(mocha());
        let lines: Vec<String> = (0..10).map(|i| format!("x{i}")).collect();
        let mut cache = super::LineHighlights::new(Some("rs"), lines.len());
        cache.ensure(&h, &lines, 3);
        assert!(cache.spans(2).is_some());
        assert!(cache.spans(3).is_none(), "nothing past the viewport is highlighted yet");
    }

    #[test]
    fn an_unknown_language_paints_plain_lines_incrementally() {
        let h = Highlighter::new(mocha());
        let lines = vec!["alpha".to_owned(), "beta".to_owned()];
        let mut cache = super::LineHighlights::new(None, lines.len());
        cache.ensure(&h, &lines, usize::MAX);
        assert_eq!(
            cache.spans(1).unwrap(),
            [super::Span { text: "beta".into(), color: (0xcd, 0xd6, 0xf4) }]
        );
    }

    #[test]
    fn bundled_syntax_themes_all_parse() {
        // Each bundled `.tmTheme` must load, or highlighting silently degrades to plain spans
        // A loaded theme tokenizes rust into more than one span; a failed
        // load would yield a single plain span — so this guards the parse path for every
        // bundled theme, the only `SyntaxChoice` that can fail.
        for name in ["catppuccin", "tokyo-night", "tokyo-night-day", "rose-pine", "rose-pine-dawn"]
        {
            let h = Highlighter::new(theme::resolve(Some(name)).syntax);
            let spans = h.highlight("let x = 1;\n", Some("rs"));
            assert!(spans[0].len() > 1, "{name}: bundled syntax theme failed to load");
        }
    }
}
