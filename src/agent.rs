//! Sending comments to the coding agent through herdr-core (`hc`).
//!
//! The agent is a live `hc` terminal on this machine whose program reports status (OSC 7501 -
//! the one way hc knows a program is an agent) and whose working directory is this worktree or
//! inside it. `hc send` pastes the text and presses Enter; it refuses a blocked agent, so a
//! send never answers someone's permission dialog. The parse and the pick are pure; `HcAgent`
//! runs the two subprocesses.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::export::ExportTarget;

/// One row of `hc ls --json`, only the fields the pick reads.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Terminal {
    pub address: String,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub state: Option<String>,
    /// Present only for a program that reports status: an agent.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// The viewer place, `workspace / tab`.
    #[serde(default)]
    #[serde(rename = "where")]
    pub place: Option<String>,
}

#[derive(Deserialize)]
struct Listing {
    #[serde(default)]
    terminals: Vec<Terminal>,
}

/// Parse `hc ls --json`.
pub fn parse_listing(json: &str) -> Result<Vec<Terminal>> {
    Ok(serde_json::from_str::<Listing>(json).context("parsing hc ls --json")?.terminals)
}

impl Terminal {
    /// How the status line names it: its name, else its address.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.address)
    }
}

/// Why no single agent was picked.
#[derive(Debug, PartialEq, Eq)]
pub enum PickError {
    None,
    Ambiguous(Vec<String>),
}

/// The agent for `repo`: live, local (a `machine/t7` address is another machine's path space),
/// reporting status, not `own` (the terminal hq-deck itself runs in), and working at or under
/// `repo`. Several of them narrow to the ones in hq-deck's own tab; still several is ambiguous.
pub fn pick<'a>(
    terminals: &'a [Terminal],
    repo: &Path,
    own: Option<&str>,
) -> Result<&'a Terminal, PickError> {
    let candidates: Vec<&Terminal> = terminals
        .iter()
        .filter(|t| t.state.as_deref().is_none_or(|s| s == "live"))
        .filter(|t| !t.address.contains('/'))
        .filter(|t| t.status.is_some())
        .filter(|t| Some(t.address.as_str()) != own)
        .filter(|t| t.cwd.as_deref().is_some_and(|cwd| cwd.starts_with(repo)))
        .collect();
    match candidates.as_slice() {
        [] => Err(PickError::None),
        [one] => Ok(one),
        many => {
            let own_place = own
                .and_then(|own| terminals.iter().find(|t| t.address == own))
                .and_then(|t| t.place.as_deref());
            let here: Vec<&&Terminal> = many
                .iter()
                .filter(|t| own_place.is_some() && t.place.as_deref() == own_place)
                .collect();
            if let [one] = here.as_slice() {
                return Ok(one);
            }
            Err(PickError::Ambiguous(many.iter().map(|t| t.label().to_string()).collect()))
        }
    }
}

/// A failed send, typed so the status line can say what to do.
#[derive(Debug)]
pub enum SendError {
    NoHc,
    NoAgent,
    Ambiguous(Vec<String>),
    Blocked(String),
    Failed { agent: String, detail: String },
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHc => write!(f, "hc not found on PATH"),
            Self::NoAgent => write!(f, "no agent in this worktree"),
            Self::Ambiguous(names) => write!(f, "several agents: {}", names.join(", ")),
            Self::Blocked(agent) => write!(f, "{agent} is blocked"),
            Self::Failed { agent, detail } => write!(f, "hc send to {agent} failed: {detail}"),
        }
    }
}

impl std::error::Error for SendError {}

/// The `hc` error envelope: `{"error":{"kind":...,"message":...}}`.
fn error_kind(stdout: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let error = value.get("error")?;
    Some((
        error.get("kind")?.as_str()?.to_string(),
        error.get("message").and_then(|m| m.as_str()).unwrap_or_default().to_string(),
    ))
}

/// The agent behind `hc`, for the worktree at `repo`.
#[derive(Debug)]
pub struct HcAgent {
    pub repo: PathBuf,
    /// `$HC_TERMINAL_ID`: the terminal hq-deck runs in, never a target.
    pub own: Option<String>,
}

impl HcAgent {
    pub fn from_env(repo: &Path) -> Self {
        let repo = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
        Self { repo, own: std::env::var("HC_TERMINAL_ID").ok().filter(|s| !s.is_empty()) }
    }

    fn hc() -> std::process::Command {
        crate::proc::command("hc")
    }

    fn send_to(&self, text: &str) -> Result<String, SendError> {
        if !crate::proc::on_path("hc") {
            return Err(SendError::NoHc);
        }
        let out = Self::hc()
            .args(["ls", "--json"])
            .output()
            .map_err(|e| SendError::Failed { agent: "hc".into(), detail: e.to_string() })?;
        let listing = String::from_utf8_lossy(&out.stdout);
        let terminals = parse_listing(&listing).map_err(|e| SendError::Failed {
            agent: "hc".into(),
            detail: format!("{e:#}: {}", String::from_utf8_lossy(&out.stderr).trim()),
        })?;
        let agent = match pick(&terminals, &self.repo, self.own.as_deref()) {
            Ok(agent) => agent,
            Err(PickError::None) => return Err(SendError::NoAgent),
            Err(PickError::Ambiguous(names)) => return Err(SendError::Ambiguous(names)),
        };
        let label = agent.label().to_string();
        let out = Self::hc()
            .args(["send", "--json", "--", &agent.address, text])
            .output()
            .map_err(|e| SendError::Failed { agent: label.clone(), detail: e.to_string() })?;
        if out.status.success() {
            return Ok(label);
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        match error_kind(&stdout) {
            Some((kind, _)) if kind == "blocked" => Err(SendError::Blocked(label)),
            Some((kind, message)) => {
                Err(SendError::Failed { agent: label, detail: format!("{kind}: {message}") })
            }
            None => Err(SendError::Failed {
                agent: label,
                detail: format!(
                    "{} {}",
                    stdout.trim(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            }),
        }
    }
}

impl ExportTarget for HcAgent {
    fn label(&self) -> &'static str {
        "agent"
    }

    fn success_message(&self, count: usize) -> String {
        let noun = if count == 1 { "comment" } else { "comments" };
        format!("sent {count} {noun} to the agent")
    }

    fn failure_message(&self) -> String {
        "send failed".to_string()
    }

    fn failure_message_for(&self, err: &anyhow::Error) -> String {
        match err.downcast_ref::<SendError>() {
            Some(SendError::NoHc) => "hc not found - install herdr-core, or y to copy".into(),
            Some(SendError::NoAgent) => "no agent in this worktree - y to copy".into(),
            Some(SendError::Ambiguous(names)) => {
                format!("several agents here ({}) - move one to this tab", names.join(", "))
            }
            Some(SendError::Blocked(agent)) => format!("{agent} is blocked - answer it first"),
            Some(SendError::Failed { agent, .. }) => format!("send to {agent} failed"),
            None => self.failure_message(),
        }
    }

    fn export(&self, text: &str) -> Result<()> {
        match self.send_to(text) {
            Ok(agent) => {
                crate::logln!("sent to {agent}");
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PickError, Terminal, error_kind, parse_listing, pick};
    use std::path::Path;

    const LISTING: &str = r#"{"hidden_finished":2,"terminals":[
        {"address":"t19","agent":"claude-code","cwd":"/w/app/sub","state":"live","status":"working","where":"app / 1"},
        {"address":"t1t","agent":"zsh","cwd":"/w/app","state":"live","turn":0,"where":"app / 1"},
        {"address":"t20","agent":"zsh","cwd":"/w/app","state":"live","where":"app / 1"},
        {"address":"t30","agent":"claude-code","cwd":"/w/other","state":"live","status":"idle","where":"x / 1"},
        {"address":"box/t3","agent":"claude-code","cwd":"/w/app","state":"live","status":"idle"}
    ]}"#;

    fn agent(address: &str, cwd: &str, place: &str) -> Terminal {
        Terminal {
            address: address.into(),
            cwd: Some(cwd.into()),
            state: Some("live".into()),
            status: Some("idle".into()),
            name: None,
            place: Some(place.into()),
        }
    }

    #[test]
    fn picks_the_one_status_reporting_local_terminal_under_the_worktree() {
        let terminals = parse_listing(LISTING).unwrap();
        let picked = pick(&terminals, Path::new("/w/app"), Some("t20")).unwrap();
        assert_eq!(
            picked.address, "t19",
            "shells, other trees, and other machines are not agents here"
        );
    }

    #[test]
    fn a_sibling_directory_with_a_shared_prefix_is_not_inside_the_worktree() {
        let terminals = vec![agent("t1", "/w/app-old", "a / 1")];
        assert_eq!(pick(&terminals, Path::new("/w/app"), None), Err(PickError::None));
    }

    #[test]
    fn never_targets_its_own_terminal_or_a_finished_one() {
        let mut done = agent("t2", "/w/app", "a / 1");
        done.state = Some("finished".into());
        let terminals = vec![agent("t1", "/w/app", "a / 1"), done];
        assert_eq!(pick(&terminals, Path::new("/w/app"), Some("t1")), Err(PickError::None));
    }

    #[test]
    fn several_agents_narrow_to_the_one_in_this_tab_else_are_ambiguous() {
        let mut own = agent("t0", "/w/app", "a / 2");
        own.status = None;
        let terminals = vec![own, agent("t1", "/w/app", "a / 1"), agent("t2", "/w/app", "a / 2")];
        assert_eq!(pick(&terminals, Path::new("/w/app"), Some("t0")).unwrap().address, "t2");
        assert_eq!(
            pick(&terminals, Path::new("/w/app"), None),
            Err(PickError::Ambiguous(vec!["t1".into(), "t2".into()]))
        );
    }

    #[test]
    fn reads_the_hc_error_envelope() {
        let out = r#"{"error":{"hint":"h","kind":"blocked","message":"t1 is blocked"}}"#;
        assert_eq!(error_kind(out), Some(("blocked".into(), "t1 is blocked".into())));
        assert_eq!(error_kind("not json"), None);
    }
}
