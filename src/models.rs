//! The models each agent offers, asked of the agents themselves: Claude Code
//! through the Agent SDK's `initialize` request, Codex through its app
//! server's `model/list`. Both take a few seconds, so the popup starts from
//! the last answer (saved by `config::ModelCache`) and refreshes in the
//! background.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How long an agent gets to answer.
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    pub fn label(self) -> &'static str {
        match self {
            Harness::Claude => "Claude",
            Harness::Codex => "Codex",
        }
    }

    /// The program and arguments that start it with `model`.
    pub fn command(self, model: &str) -> Vec<String> {
        match self {
            Harness::Claude => vec!["claude".into(), "--model".into(), model.into()],
            Harness::Codex => vec!["codex".into(), "-m".into(), model.into()],
        }
    }

    /// The arguments that set the reasoning effort.
    pub fn effort_args(self, effort: &str) -> Vec<String> {
        match self {
            Harness::Claude => vec!["--effort".into(), effort.into()],
            Harness::Codex => vec!["-c".into(), format!("model_reasoning_effort={effort}")],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    pub harness: Harness,
    /// What the agent's model flag takes, such as "opus" or "gpt-6.1-sol".
    pub id: String,
    pub name: String,
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
}

pub fn fetch(harness: Harness) -> Result<Vec<Model>> {
    match harness {
        Harness::Claude => fetch_claude(),
        Harness::Codex => fetch_codex(),
    }
}

/// Claude Code reports its models in the response to the SDK's `initialize`
/// request. It runs without the user's settings (so no hooks) and without
/// saving a session, and no prompt is sent. (`--bare` would also skip the
/// login, which changes which models are listed.)
fn fetch_claude() -> Result<Vec<Model>> {
    let mut child = Command::new("claude")
        .args([
            "-p",
            "--setting-sources",
            "",
            "--no-session-persistence",
            "--verbose",
        ])
        .args([
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("starting claude")?;
    let request = r#"{"type":"control_request","request_id":"rex-launch","request":{"subtype":"initialize"}}"#;
    let reply = ask(&mut child, &[request], |v| v["type"] == "control_response")?;
    parse_claude(&reply["response"]["response"])
}

/// The Codex app server answers `model/list` once it's initialized.
fn fetch_codex() -> Result<Vec<Model>> {
    let mut child = Command::new("codex")
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("starting codex app-server")?;
    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"rex-launch","version":"0.1.0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"model/list","params":{}}"#,
    ];
    let reply = ask(&mut child, &requests, |v| v["id"] == 2)?;
    if let Some(error) = reply.get("error") {
        bail!("codex model/list failed: {error}");
    }
    parse_codex(&reply["result"])
}

/// Writes `requests` as JSON lines to the child, returns the first line of
/// output that `is_reply` accepts, and stops the child.
fn ask(child: &mut Child, requests: &[&str], is_reply: impl Fn(&Value) -> bool) -> Result<Value> {
    let mut stdin = child.stdin.take().context("no stdin")?;
    for request in requests {
        writeln!(stdin, "{request}")?;
    }
    stdin.flush()?;
    let stdout = child.stdout.take().context("no stdout")?;
    let (lines, received) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let reply = loop {
        match received.recv_timeout(TIMEOUT) {
            Ok(line) => {
                if let Ok(value) = serde_json::from_str::<Value>(&line)
                    && is_reply(&value)
                {
                    break Ok(value);
                }
            }
            Err(_) => break Err(anyhow::anyhow!("no answer within {}s", TIMEOUT.as_secs())),
        }
    };
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    reply
}

/// Keeps Claude Code's model aliases ("opus", "sonnet", ...), which follow
/// the latest version, and drops "default" and pinned versions.
fn parse_claude(response: &Value) -> Result<Vec<Model>> {
    let models = response["models"]
        .as_array()
        .context("no models in claude's answer")?;
    Ok(models
        .iter()
        .filter_map(|m| {
            let id = m["value"].as_str()?;
            if id == "default" || id.starts_with("claude-") {
                return None;
            }
            Some(Model {
                harness: Harness::Claude,
                id: id.to_string(),
                name: m["displayName"].as_str().unwrap_or(id).to_string(),
                efforts: strings(&m["supportedEffortLevels"]),
                default_effort: None,
            })
        })
        .collect())
}

/// Keeps the models Codex doesn't hide.
fn parse_codex(result: &Value) -> Result<Vec<Model>> {
    let models = result["data"]
        .as_array()
        .context("no models in codex's answer")?;
    Ok(models
        .iter()
        .filter(|m| m["hidden"] != true)
        .filter_map(|m| {
            let id = m["model"].as_str().or(m["id"].as_str())?;
            let efforts = m["supportedReasoningEfforts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| e["reasoningEffort"].as_str().map(String::from))
                .collect();
            Some(Model {
                harness: Harness::Codex,
                id: id.to_string(),
                name: codex_name(m["displayName"].as_str().unwrap_or(id)),
                efforts,
                default_effort: m["defaultReasoningEffort"].as_str().map(String::from),
            })
        })
        .collect())
}

/// "GPT-6.1-Sol" reads as "GPT-6.1 Sol", as in the Codex app.
fn codex_name(name: &str) -> String {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() > 2 {
        format!("{}-{} {}", parts[0], parts[1], parts[2..].join(" "))
    } else {
        name.to_string()
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(String::from))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_keeps_aliases_only() {
        let response = json!({"models": [
            {"value": "default", "displayName": "Default (recommended)"},
            {"value": "opus", "displayName": "Opus 5.5", "supportedEffortLevels": ["low", "high"]},
            {"value": "claude-opus-4-6", "displayName": "Opus 4.6"},
        ]});
        let models = parse_claude(&response).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(
            (models[0].id.as_str(), models[0].name.as_str()),
            ("opus", "Opus 5.5")
        );
        assert_eq!(models[0].efforts, ["low", "high"]);
    }

    #[test]
    fn codex_drops_hidden_models_and_tidies_names() {
        let result = json!({"data": [
            {"model": "gpt-6.1-sol", "displayName": "GPT-6.1-Sol", "hidden": false, "defaultReasoningEffort": "low",
             "supportedReasoningEfforts": [{"reasoningEffort": "low"}, {"reasoningEffort": "high"}]},
            {"model": "secret", "displayName": "Secret", "hidden": true},
        ]});
        let models = parse_codex(&result).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "GPT-6.1 Sol");
        assert_eq!(models[0].default_effort.as_deref(), Some("low"));
        assert_eq!(models[0].efforts, ["low", "high"]);
    }

    #[test]
    fn effort_arguments_follow_each_agent() {
        assert_eq!(Harness::Claude.effort_args("max"), ["--effort", "max"]);
        assert_eq!(
            Harness::Codex.effort_args("xhigh"),
            ["-c", "model_reasoning_effort=xhigh"]
        );
    }
}
