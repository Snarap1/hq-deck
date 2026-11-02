//! The branch commit template (`[commit_message].branch_pattern` + `.template`): a commit message
//! shaped by the branch it is written on. Parsed and validated once, at config load, so every
//! template the config accepts renders; rendered by the ✧ generator ([`crate::commit_msg`])
//! around the one summary line it produced. Pure: the caller passes the branch name in.

use std::sync::LazyLock;

use regex::Regex;

/// `{type}` when the generated line carries none (the filename fallback, a model that ignored
/// the instruction): the honest claim for a change nobody classified.
pub const DEFAULT_TYPE: &str = "chore";

/// A conventional-commit prefix at the start of a line: `type`, an optional `(scope)`, an
/// optional `!`, then `:` and the description.
static CONVENTIONAL_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<type>[A-Za-z]+)(?:\([^)]*\))?!?:\s+(?P<rest>\S.*)$").unwrap()
});

/// `line` split into its conventional type and the description after the prefix; no type when
/// the line has no such prefix.
pub fn split_type(line: &str) -> (Option<&str>, &str) {
    match CONVENTIONAL_PREFIX.captures(line) {
        Some(c) => (Some(c.name("type").unwrap().as_str()), c.name("rest").unwrap().as_str()),
        None => (None, line),
    }
}

/// A compiled `branch_pattern` and its parsed `template`.
#[derive(Clone, Debug)]
pub struct BranchTemplate {
    pattern: Regex,
    template: String,
    parts: Vec<Part>,
}

/// Equal when both sources are: the compiled pattern and the parts follow from them.
impl PartialEq for BranchTemplate {
    fn eq(&self, other: &Self) -> bool {
        self.pattern.as_str() == other.pattern.as_str() && self.template == other.template
    }
}

impl Eq for BranchTemplate {}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    Literal(String),
    /// `{summary}`: the generated line, without its conventional `type(scope):` prefix.
    Summary {
        lower: bool,
    },
    /// `{type}`: the conventional type the generated line started with, or [`DEFAULT_TYPE`].
    Type {
        lower: bool,
    },
    /// `{name}`: the named capture group `name` of the branch match.
    Group {
        name: String,
        lower: bool,
    },
}

/// Why a pattern/template pair was refused, and which of the two keys to blame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateError {
    /// `branch_pattern` or `template`.
    pub key: &'static str,
    /// Phrased to follow "invalid value for `commit_message.<key>`: ".
    pub detail: String,
}

impl BranchTemplate {
    /// Compile `pattern` and parse `template` against its named groups.
    pub fn parse(pattern: &str, template: &str) -> Result<Self, TemplateError> {
        let pattern_error = |detail: String| TemplateError { key: "branch_pattern", detail };
        let compiled = Regex::new(pattern).map_err(|error| pattern_error(error.to_string()))?;
        let groups: Vec<&str> = compiled.capture_names().flatten().collect();
        for reserved in ["summary", "type"] {
            if groups.contains(&reserved) {
                return Err(pattern_error(format!(
                    "the group name `{reserved}` is reserved for the generated line"
                )));
            }
        }
        let parts = parse_parts(template, &groups)
            .map_err(|detail| TemplateError { key: "template", detail })?;
        Ok(Self { pattern: compiled, template: template.to_owned(), parts })
    }

    pub fn pattern(&self) -> &str {
        self.pattern.as_str()
    }

    pub fn template(&self) -> &str {
        &self.template
    }

    /// Whether the template asks the model for the commit type (`{type}`).
    pub fn uses_type(&self) -> bool {
        self.parts.iter().any(|part| matches!(part, Part::Type { .. }))
    }

    /// Whether the pattern matches `branch`, i.e. [`Self::render`] would render.
    pub fn matches(&self, branch: &str) -> bool {
        self.pattern.is_match(branch)
    }

    /// The message for the generated `line` on `branch`, or `None` when the pattern does not
    /// match the branch. A group that did not take part in the match renders empty. A
    /// conventional prefix on `line` never reaches `{summary}`: its type feeds `{type}`, and the
    /// template's own prefix is the only one the message carries.
    pub fn render(&self, branch: &str, line: &str) -> Option<String> {
        let captures = self.pattern.captures(branch)?;
        let (kind, summary) = split_type(line);
        let kind = kind.unwrap_or(DEFAULT_TYPE);
        let mut out = String::new();
        let mut push = |text: &str, lower: bool| {
            if lower {
                out.push_str(&text.to_lowercase());
            } else {
                out.push_str(text);
            }
        };
        for part in &self.parts {
            match part {
                Part::Literal(text) => push(text, false),
                Part::Summary { lower } => push(summary, *lower),
                Part::Type { lower } => push(kind, *lower),
                Part::Group { name, lower } => {
                    push(captures.name(name).map_or("", |m| m.as_str()), *lower);
                }
            }
        }
        Some(out)
    }
}

/// Split `template` into literals and placeholders. Every `{` opens a placeholder that must
/// close, name `summary`, `type`, or one of `groups`, and carry at most the `lower` filter; a `}` outside
/// a placeholder is literal. At least one `{summary}` is required.
fn parse_parts(template: &str, groups: &[&str]) -> Result<Vec<Part>, String> {
    let mut parts = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        if open > 0 {
            parts.push(Part::Literal(rest[..open].to_owned()));
        }
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Err(format!("unclosed `{{` at {:?}", &rest[open..]));
        };
        let inner = &after[..close];
        let (name, filter) = match inner.split_once('|') {
            Some((name, filter)) => (name, Some(filter)),
            None => (inner, None),
        };
        let lower = match filter {
            None => false,
            Some("lower") => true,
            Some(other) => {
                return Err(format!("unknown filter `{other}` in `{{{inner}}}`; expected `lower`"));
            }
        };
        if name == "summary" {
            parts.push(Part::Summary { lower });
        } else if name == "type" {
            parts.push(Part::Type { lower });
        } else if groups.contains(&name) {
            parts.push(Part::Group { name: name.to_owned(), lower });
        } else {
            let mut expected = vec!["`{summary}`".to_owned(), "`{type}`".to_owned()];
            expected.extend(groups.iter().map(|g| format!("`{{{g}}}`")));
            return Err(format!(
                "unknown placeholder `{{{inner}}}`; expected one of {}",
                expected.join(", ")
            ));
        }
        rest = &after[close + 1..];
    }
    if !rest.is_empty() {
        parts.push(Part::Literal(rest.to_owned()));
    }
    if !parts.iter().any(|part| matches!(part, Part::Summary { .. })) {
        return Err("no `{summary}` placeholder".to_owned());
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDF_PATTERN: &str = r"(?P<ticket>IDC-\d+)-(?P<scope>.+)";
    const IDF_TEMPLATE: &str =
        "feat({scope|lower}): {ticket} {summary}\n\nTicket: https://jira.example/browse/{ticket}";

    fn idf() -> BranchTemplate {
        BranchTemplate::parse(IDF_PATTERN, IDF_TEMPLATE).unwrap()
    }

    #[test]
    fn a_matching_branch_fills_groups_filters_and_summary() {
        assert_eq!(
            idf().render("PN-IDC-29308-AI", "Add retry to the upload client").as_deref(),
            Some(
                "feat(ai): IDC-29308 Add retry to the upload client\n\n\
                 Ticket: https://jira.example/browse/IDC-29308"
            ),
        );
        assert_eq!(
            idf().render("IDC-27532-ANV-SERVICE", "Fix it").as_deref(),
            Some(
                "feat(anv-service): IDC-27532 Fix it\n\nTicket: https://jira.example/browse/IDC-27532"
            ),
        );
    }

    #[test]
    fn a_conventional_prefix_on_the_line_is_dropped_not_doubled() {
        assert_eq!(
            idf().render("PN-IDC-33012-DCO-PART-7", "docs(idc-33012): log PN session").as_deref(),
            Some(
                "feat(dco-part-7): IDC-33012 log PN session\n\n\
                 Ticket: https://jira.example/browse/IDC-33012"
            ),
        );
    }

    #[test]
    fn type_comes_from_the_line_or_falls_back_to_chore() {
        let template =
            BranchTemplate::parse(IDF_PATTERN, "{type|lower}({scope|lower}): {ticket} {summary}")
                .unwrap();
        assert!(template.uses_type());
        assert!(!idf().uses_type());
        assert_eq!(
            template.render("IDC-1-X", "Docs: log session").as_deref(),
            Some("docs(x): IDC-1 log session")
        );
        assert_eq!(
            template.render("IDC-1-X", "fix(api)!: drop retry").as_deref(),
            Some("fix(x): IDC-1 drop retry")
        );
        assert_eq!(
            template.render("IDC-1-X", "Update app.rs").as_deref(),
            Some("chore(x): IDC-1 Update app.rs")
        );
        // A colon later in the line is not a prefix.
        assert_eq!(
            template.render("IDC-1-X", "Handle a: b in URLs").as_deref(),
            Some("chore(x): IDC-1 Handle a: b in URLs")
        );
    }

    #[test]
    fn a_branch_the_pattern_misses_renders_nothing() {
        assert_eq!(idf().render("main", "Fix it"), None);
        assert_eq!(idf().render("feature/login", "Fix it"), None);
    }

    #[test]
    fn a_group_outside_the_match_renders_empty() {
        let template = BranchTemplate::parse(r"(?P<a>x)|(?P<b>y)", "[{a}{b}] {summary}").unwrap();
        assert_eq!(template.render("y", "s").as_deref(), Some("[y] s"));
    }

    #[test]
    fn a_lone_closing_brace_is_literal() {
        let template = BranchTemplate::parse("(?P<t>T-1)", "} {t} {summary} }").unwrap();
        assert_eq!(template.render("T-1", "s").as_deref(), Some("} T-1 s }"));
    }

    #[test]
    fn invalid_pairs_name_the_key_to_blame() {
        let err =
            |pattern: &str, template: &str| BranchTemplate::parse(pattern, template).unwrap_err();

        let e = err("(?P<t>", "{summary}");
        assert_eq!(e.key, "branch_pattern");

        let e = err("(?P<summary>x)", "{summary}");
        assert_eq!(e.key, "branch_pattern");
        assert!(e.detail.contains("reserved"), "{}", e.detail);

        let e = err("(?P<type>x)", "{summary}");
        assert_eq!(e.key, "branch_pattern");

        let e = err("(?P<t>x)", "{t} only");
        assert_eq!(e.key, "template");
        assert!(e.detail.contains("{summary}"), "{}", e.detail);

        let e = err("(?P<t>x)", "{ticket} {summary}");
        assert_eq!(e.key, "template");
        assert!(e.detail.contains("`{ticket}`") && e.detail.contains("`{t}`"), "{}", e.detail);

        let e = err("(?P<t>x)", "{t|upper} {summary}");
        assert!(e.detail.contains("unknown filter `upper`"), "{}", e.detail);

        let e = err("(?P<t>x)", "{summary} {t");
        assert!(e.detail.contains("unclosed"), "{}", e.detail);
    }

    #[test]
    fn equality_follows_the_two_sources() {
        assert_eq!(idf(), idf());
        assert_ne!(idf(), BranchTemplate::parse(IDF_PATTERN, "{summary}").unwrap());
    }
}
