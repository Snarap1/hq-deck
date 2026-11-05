//! Sending comments to the coding agent through herdr-core (`hc`) or the older herdr.
//!
//! The agent is a live terminal on this machine whose program is known to be an agent (under
//! `hc`, it reports status over OSC 7501; under herdr, `agent list` names it) and whose working
//! directory is this worktree or inside it. `hc send` pastes the text and presses Enter; it
//! refuses a blocked agent, so a send never answers someone's permission dialog. herdr has no
//! such refusal, so the status is checked here, and the text is pasted without Enter and the
//! agent focused for the reviewer to submit. The parses and the pick are pure; `Agent` runs the
//! subprocesses.

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

/// One entry of `herdr agent list`, only the fields the pick reads.
#[derive(Deserialize)]
struct HerdrAgent {
    pane_id: String,
    tab_id: String,
    agent_status: String,
    agent: Option<String>,
    name: Option<String>,
    display_agent: Option<String>,
    /// The live directory; `cwd` can stay at the pane's launch directory.
    foreground_cwd: Option<PathBuf>,
    cwd: Option<PathBuf>,
}

#[derive(Deserialize)]
struct HerdrResponse {
    result: HerdrAgents,
}

#[derive(Deserialize)]
struct HerdrAgents {
    agents: Vec<HerdrAgent>,
}

/// Parse `herdr agent list` into the rows the pick reads, with the tab id as the place.
pub fn parse_herdr_listing(json: &str) -> Result<Vec<Terminal>> {
    let response: HerdrResponse = serde_json::from_str(json).context("parsing herdr agent list")?;
    Ok(response
        .result
        .agents
        .into_iter()
        .filter(|agent| agent.agent.is_some())
        .map(|agent| Terminal {
            address: agent.pane_id,
            cwd: agent.foreground_cwd.or(agent.cwd),
            state: None,
            status: Some(agent.agent_status),
            name: [agent.name, agent.display_agent, agent.agent]
                .into_iter()
                .flatten()
                .find(|name| !name.is_empty()),
            place: Some(agent.tab_id),
        })
        .collect())
}

const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

/// `text` as one bracketed paste, so a vim-style input in normal mode inserts it instead of
/// running it. The body is rebuilt with a suffix check per character, so a terminator never
/// survives, not even one spliced together by an earlier removal.
fn pasted(text: &str) -> String {
    let mut body = String::with_capacity(text.len());
    for ch in text.chars() {
        body.push(ch);
        if body.ends_with(PASTE_END) {
            body.truncate(body.len() - PASTE_END.len());
        }
    }
    format!("{PASTE_START}{body}{PASTE_END}")
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
            Self::Failed { agent, detail } => write!(f, "send to {agent} failed: {detail}"),
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

/// What hq-deck runs inside, which decides how it reaches the agent.
#[derive(Debug, PartialEq, Eq)]
pub enum Host {
    /// `hc`, with the terminal hq-deck runs in, if any: never a target.
    Hc { own: Option<String> },
    /// A herdr pane, with hq-deck's own pane (never a target) and tab.
    Herdr { pane: String, tab: Option<String> },
}

impl Host {
    /// An `hc` terminal wins over herdr's variables, which it may have inherited from the pane
    /// that started the core. Outside both, `hc` still works, so it is the default.
    fn resolve(
        hc_terminal: Option<String>,
        herdr_pane: Option<String>,
        herdr_tab: Option<String>,
    ) -> Self {
        match (hc_terminal, herdr_pane) {
            (Some(own), _) => Self::Hc { own: Some(own) },
            (None, Some(pane)) => Self::Herdr { pane, tab: herdr_tab },
            (None, None) => Self::Hc { own: None },
        }
    }

    fn from_env() -> Self {
        let var = |name| std::env::var(name).ok().filter(|s: &String| !s.is_empty());
        Self::resolve(var("HC_TERMINAL_ID"), var("HERDR_PANE_ID"), var("HERDR_TAB_ID"))
    }
}

/// The agent for the worktree at `repo`, reached through `host`.
#[derive(Debug)]
pub struct Agent {
    pub repo: PathBuf,
    pub host: Host,
}

impl Agent {
    pub fn from_env(repo: &Path) -> Self {
        let repo = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
        Self { repo, host: Host::from_env() }
    }

    fn hc() -> std::process::Command {
        crate::proc::command("hc")
    }

    fn herdr() -> std::process::Command {
        crate::proc::command(std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into()))
    }

    fn send_to(&self, text: &str) -> Result<String, SendError> {
        match &self.host {
            Host::Hc { own } => self.send_through_hc(own.as_deref(), text),
            Host::Herdr { pane, tab } => self.send_through_herdr(pane, tab.as_deref(), text),
        }
    }

    fn send_through_herdr(
        &self,
        pane: &str,
        tab: Option<&str>,
        text: &str,
    ) -> Result<String, SendError> {
        let failed =
            |agent: &str, detail: String| SendError::Failed { agent: agent.to_string(), detail };
        let out = Self::herdr()
            .args(["agent", "list"])
            .output()
            .map_err(|e| failed("herdr", e.to_string()))?;
        let listing = String::from_utf8_lossy(&out.stdout);
        let mut terminals = parse_herdr_listing(&listing).map_err(|e| {
            failed("herdr", format!("{e:#}: {}", String::from_utf8_lossy(&out.stderr).trim()))
        })?;
        // herdr lists agents only, and the pick finds hq-deck's own tab through its own row.
        if !terminals.iter().any(|t| t.address == pane) {
            terminals.push(Terminal {
                address: pane.to_string(),
                cwd: None,
                state: None,
                status: None,
                name: None,
                place: tab.map(str::to_string),
            });
        }
        let agent = match pick(&terminals, &self.repo, Some(pane)) {
            Ok(agent) => agent,
            Err(PickError::None) => return Err(SendError::NoAgent),
            Err(PickError::Ambiguous(names)) => return Err(SendError::Ambiguous(names)),
        };
        let label = agent.label().to_string();
        if agent.status.as_deref() == Some("blocked") {
            return Err(SendError::Blocked(label));
        }
        let out = Self::herdr()
            .args(["pane", "send-text", &agent.address, &pasted(text)])
            .output()
            .map_err(|e| failed(&label, e.to_string()))?;
        if !out.status.success() {
            return Err(failed(&label, String::from_utf8_lossy(&out.stderr).trim().to_string()));
        }
        // The text is delivered: a failed focus must not fail the export, or the comments stay
        // and the next send pastes them twice.
        let _ = Self::herdr().args(["agent", "focus", &agent.address]).output();
        Ok(label)
    }

    fn send_through_hc(&self, own: Option<&str>, text: &str) -> Result<String, SendError> {
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
        let agent = match pick(&terminals, &self.repo, own) {
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

impl ExportTarget for Agent {
    fn label(&self) -> &'static str {
        "agent"
    }

    fn success_message(&self, count: usize) -> String {
        let noun = if count == 1 { "comment" } else { "comments" };
        match self.host {
            Host::Hc { .. } => format!("sent {count} {noun} to the agent"),
            Host::Herdr { .. } => format!("pasted {count} {noun} into the agent"),
        }
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
    use super::{
        Host, PickError, Terminal, error_kind, parse_herdr_listing, parse_listing, pasted, pick,
    };
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
    fn an_hc_terminal_wins_over_inherited_herdr_variables() {
        let hc = || Some("t1".to_string());
        let pane = || Some("w1:p1".to_string());
        let tab = || Some("w1:t1".to_string());
        assert_eq!(Host::resolve(hc(), pane(), tab()), Host::Hc { own: hc() });
        assert_eq!(
            Host::resolve(None, pane(), tab()),
            Host::Herdr { pane: "w1:p1".into(), tab: tab() }
        );
        assert_eq!(Host::resolve(None, None, None), Host::Hc { own: None });
    }

    const HERDR_LISTING: &str = r#"{"id":"cli:agent:list","result":{"type":"agent_list","agents":[
        {"agent":"claude","agent_status":"blocked","cwd":"/launch","foreground_cwd":"/w/app/sub",
         "pane_id":"w1:p1","tab_id":"w1:t1","workspace_id":"w1"},
        {"agent":"codex","name":"reviewer","agent_status":"idle","cwd":"/w/other",
         "pane_id":"w1:p2","tab_id":"w1:t2","workspace_id":"w1"},
        {"agent_status":"unknown","cwd":"/w/app","pane_id":"w1:p3","tab_id":"w1:t1",
         "workspace_id":"w1"}
    ]}}"#;

    #[test]
    fn a_herdr_listing_maps_onto_the_same_pick() {
        let terminals = parse_herdr_listing(HERDR_LISTING).unwrap();
        assert_eq!(terminals.len(), 2, "a pane without an agent is not an agent");
        assert_eq!(terminals[1].label(), "reviewer");
        let picked = pick(&terminals, Path::new("/w/app"), Some("w1:p9")).unwrap();
        assert_eq!(picked.address, "w1:p1", "the live directory decides, not the launch one");
        assert_eq!(picked.label(), "claude");
        assert_eq!(picked.status.as_deref(), Some("blocked"));
    }

    #[test]
    fn a_paste_cannot_be_ended_early_by_its_own_text() {
        assert_eq!(pasted("hi\nthere"), "\x1b[200~hi\nthere\x1b[201~");
        let spliced = "a\x1b[20\x1b[201~1~b";
        assert_eq!(pasted(spliced), "\x1b[200~ab\x1b[201~");
    }

    #[test]
    fn reads_the_hc_error_envelope() {
        let out = r#"{"error":{"hint":"h","kind":"blocked","message":"t1 is blocked"}}"#;
        assert_eq!(error_kind(out), Some(("blocked".into(), "t1 is blocked".into())));
        assert_eq!(error_kind("not json"), None);
    }
}
