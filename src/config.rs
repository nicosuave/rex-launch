//! The user's code folders and agents, what was picked last time, and what
//! the last project scans found.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::discover::Repo;
use crate::models::{Harness, Model};

#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub name: String,
    /// The repository's path on the machine that runs it.
    pub path: String,
    /// A Rex host label from `rex hosts`. Absent means this computer.
    #[serde(default)]
    pub host: Option<String>,
    /// When git last changed HEAD, unix seconds; orders the picker.
    #[serde(default)]
    pub activity: u64,
    /// "No project": the agent starts in the home directory, without a worktree.
    #[serde(skip)]
    pub scratch: bool,
}

impl Project {
    /// The "No project" entry for a connection.
    pub fn scratch(host: Option<&str>) -> Project {
        Project { name: "No project".into(), path: "~".into(), host: host.map(String::from), activity: 0, scratch: true }
    }

    /// Identifies the project across runs and connections.
    pub fn key(&self) -> String {
        format!("{}|{}", self.host.as_deref().unwrap_or(""), self.path)
    }
}

/// How much an agent may do without asking, which decides the chip's color.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    #[default]
    Default,
    Auto,
    Full,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Access {
    pub name: String,
    #[serde(default)]
    pub level: Level,
    /// Arguments added to the agent's command in this mode.
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Agent {
    pub id: String,
    /// The program family shown in the model menu, such as "Claude".
    pub harness: String,
    /// The model, such as "Opus 5.5".
    pub name: String,
    /// Shown dimmed after the name when the agent has no effort levels.
    #[serde(default)]
    pub detail: String,
    /// The program and its arguments. The prompt is appended last.
    pub command: Vec<String>,
    #[serde(default)]
    pub access: Vec<Access>,
    /// Reasoning effort levels, chosen with the arrow keys in the model menu.
    #[serde(default)]
    pub efforts: Vec<String>,
    #[serde(default)]
    pub default_effort: Option<String>,
    /// How the effort is passed, for agents built from a live model list.
    #[serde(skip)]
    pub kind: Option<Harness>,
}

impl Agent {
    /// The menu entry for a model an agent reported.
    pub fn from_model(model: &Model) -> Agent {
        let access = match model.harness {
            Harness::Claude => vec![
                Access { name: "Default".into(), level: Level::Default, args: vec![] },
                Access { name: "Auto".into(), level: Level::Auto, args: vec!["--permission-mode".into(), "auto".into()] },
                Access {
                    name: "Full access".into(),
                    level: Level::Full,
                    args: vec!["--dangerously-skip-permissions".into()],
                },
            ],
            Harness::Codex => vec![
                Access { name: "Default".into(), level: Level::Default, args: vec![] },
                Access {
                    name: "Full access".into(),
                    level: Level::Full,
                    args: vec!["--dangerously-bypass-approvals-and-sandbox".into()],
                },
            ],
        };
        Agent {
            id: format!("{}:{}", model.harness.label().to_lowercase(), model.id),
            harness: model.harness.label().into(),
            name: model.name.clone(),
            detail: String::new(),
            command: model.harness.command(&model.id),
            access,
            efforts: model.efforts.clone(),
            default_effort: model.default_effort.clone(),
            kind: Some(model.harness),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Folders searched for git repositories, on every connection.
    #[serde(default = "default_code_folders")]
    pub code_folders: Vec<String>,
    /// Folder names to skip while searching, such as "_archive".
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Projects outside the code folders.
    #[serde(default, rename = "project")]
    pub projects: Vec<Project>,
    #[serde(default, rename = "agent")]
    pub agents: Vec<Agent>,
}

const DEFAULT_AGENTS: &str = r#"
[[agent]]
id = "claude-opus"
harness = "Claude"
name = "Opus 5.5"
detail = "High"
command = ["claude", "--model", "opus", "--effort", "high"]
access = [
  { name = "Default", level = "default" },
  { name = "Auto", level = "auto", args = ["--permission-mode", "auto"] },
  { name = "Full access", level = "full", args = ["--dangerously-skip-permissions"] },
]

[[agent]]
id = "codex-astra"
harness = "Codex"
name = "GPT-6 Astra"
detail = "Medium"
command = ["codex", "-m", "gpt-6-astra", "-c", "model_reasoning_effort=medium"]
access = [
  { name = "Default", level = "default" },
  { name = "Full access", level = "full", args = ["--dangerously-bypass-approvals-and-sandbox"] },
]

[[agent]]
id = "codex-sol"
harness = "Codex"
name = "GPT-6.1 Sol"
detail = "High"
command = ["codex", "-m", "gpt-6.1-sol", "-c", "model_reasoning_effort=high"]
access = [
  { name = "Default", level = "default" },
  { name = "Full access", level = "full", args = ["--dangerously-bypass-approvals-and-sandbox"] },
]
"#;

fn default_code_folders() -> Vec<String> {
    vec!["~/Code".into()]
}

impl Config {
    /// Reads the config file. Without one, the defaults apply: `~/Code` and
    /// the built-in agents.
    pub fn load(path: Option<&Path>) -> Result<Config> {
        let path = path.map_or_else(default_config_path, Path::to_path_buf);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        let config = Config::parse(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(config)
    }

    /// The agents offered before any agent has reported its models.
    pub fn fallback_agents() -> Vec<Agent> {
        Config::parse(DEFAULT_AGENTS).map(|c| c.agents).unwrap_or_default()
    }

    fn parse(text: &str) -> Result<Config> {
        let mut config: Config = toml::from_str(text)?;
        for agent in &mut config.agents {
            if agent.command.is_empty() {
                bail!("agent {:?} has an empty command", agent.id);
            }
            if agent.access.is_empty() {
                agent.access.push(Access { name: "Default".into(), level: Level::Default, args: vec![] });
            }
        }
        Ok(config)
    }
}

/// What the user picked last, so the next popup starts there.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    /// The last project's `Project::key`.
    pub project: Option<String>,
    pub agent: Option<String>,
    /// The access mode last used with each agent, by agent ID.
    #[serde(default)]
    pub access: HashMap<String, String>,
    /// When each project was last launched, unix seconds, by `Project::key`.
    #[serde(default)]
    pub launched: HashMap<String, u64>,
    /// The effort last used with each agent, by agent ID.
    #[serde(default)]
    pub efforts: HashMap<String, String>,
}

impl State {
    /// The saved state, or an empty one if there is none or it can't be read.
    pub fn load() -> State {
        read_json(&state_dir().join("state.json"))
    }

    pub fn save(&self) -> Result<()> {
        write_json(&state_dir().join("state.json"), self)
    }
}

/// What the last scan of one connection found, so the picker is full before
/// this run's scan finishes. Each connection has its own file, written by the
/// process that scanned it.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Scan {
    /// Unix seconds.
    pub scanned_at: u64,
    pub repos: Vec<Repo>,
}

impl Scan {
    pub fn load(host: Option<&str>) -> Option<Scan> {
        let path = scan_path(host);
        path.exists().then(|| read_json(&path))
    }

    pub fn save(&self, host: Option<&str>) -> Result<()> {
        write_json(&scan_path(host), self)
    }
}

/// The models each agent reported last time.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ModelCache {
    #[serde(default)]
    pub models: Vec<Model>,
}

impl ModelCache {
    pub fn load() -> ModelCache {
        read_json(&state_dir().join("models.json"))
    }

    pub fn save(&self) -> Result<()> {
        write_json(&state_dir().join("models.json"), self)
    }

    /// Replaces one agent's models.
    pub fn replace(&mut self, harness: Harness, models: Vec<Model>) {
        self.models.retain(|m| m.harness != harness);
        self.models.extend(models);
        self.models.sort_by_key(|m| m.harness == Harness::Codex);
    }
}

fn scan_path(host: Option<&str>) -> PathBuf {
    let name = host.unwrap_or("this-computer").replace(['/', '\\'], "_");
    state_dir().join("scans").join(format!("{name}.json"))
}

/// The file's contents, or the default if it's missing or unreadable.
fn read_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map_or_else(|| home().join(fallback), PathBuf::from)
}

fn default_config_path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("rex-launch/config.toml")
}

fn state_dir() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join("rex-launch")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_agents_parse() {
        let config = Config::parse(DEFAULT_AGENTS).unwrap();
        assert_eq!(config.agents.len(), 3);
        assert!(config.agents.iter().all(|a| !a.access.is_empty()));
    }

    #[test]
    fn agents_without_access_modes_get_a_default_one() {
        let config = Config::parse(
            r#"
            [[project]]
            name = "data"
            path = "/Users/me/Code/data"
            host = "studio"

            [[agent]]
            id = "x"
            harness = "Claude"
            name = "Opus"
            command = ["claude"]
            "#,
        )
        .unwrap();
        assert_eq!(config.projects[0].host.as_deref(), Some("studio"));
        assert_eq!(config.agents[0].access[0].level, Level::Default);
    }
}
