//! Opening the agent's tab with the `rex` CLI.
//!
//! The tab's own command creates the worktree with worktrunk and then starts
//! the agent in it. Because that command runs on whichever Rex server hosts the
//! tab, remote projects work the same way as local ones.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

/// Runs in the new tab as `sh -c TAB_SCRIPT rex-launch REPO BRANCH AGENT...`.
///
/// The branch name is made unique here, on the machine that has the
/// repository: if it's taken, the next free `-2`, `-3`, ... is used and the
/// tab is renamed to match. The prompt stays among the agent's arguments and
/// never passes through `wt -x`, which would expand `{{ ... }}` templates in
/// it. The agent runs through an interactive login shell, so it's found on
/// the same PATH as in a terminal (startup files such as `.zshrc` often set
/// PATH only for interactive shells). When the agent exits, the tab keeps a
/// shell open in the worktree.
const TAB_SCRIPT: &str = r#"repo=$1
branch=$2
shift 2
export GIT_TERMINAL_PROMPT=0
name=$branch
n=2
while git -C "$repo" show-ref --verify --quiet "refs/heads/$branch"; do
  branch=$name-$n
  n=$((n + 1))
done
[ "$branch" = "$name" ] || rex window rename "$branch" >/dev/null 2>&1
base=$(git -C "$repo" symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)
if [ -n "$base" ]; then
  printf 'Fetching %s\n' "$base"
  git -C "$repo" fetch --quiet origin "${base#origin/}" ||
    printf 'Fetch failed; branching from the last fetched %s\n' "$base"
  wt -C "$repo" switch --create "$branch" --base "$base" --no-cd --yes
else
  wt -C "$repo" switch --create "$branch" --no-cd --yes
fi || {
  printf '\nrex-launch: could not create worktree %s in %s\n' "$branch" "$repo"
  exec "${SHELL:-/bin/sh}" -l
}
dir=$(git -C "$repo" worktree list --porcelain |
  awk -v ref="refs/heads/$branch" '/^worktree /{dir=substr($0, 10)} $0 == "branch " ref {print dir; exit}')
cd "$dir" || exec "${SHELL:-/bin/sh}" -l
"${SHELL:-/bin/sh}" -lic '"$@"' agent "$@"
exec "${SHELL:-/bin/sh}" -l
"#;

/// Runs in the new tab as `sh -c NO_WORKTREE_SCRIPT rex-launch AGENT...` for
/// "No project": the agent (through an interactive login shell, as in
/// `TAB_SCRIPT`), then a shell, in the tab's starting directory.
const NO_WORKTREE_SCRIPT: &str = r#""${SHELL:-/bin/sh}" -lic '"$@"' agent "$@"
exec "${SHELL:-/bin/sh}" -l
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The Rex host label to open the tab on, or `None` for this server.
    pub host: Option<String>,
    /// The session the tab goes into, created if missing.
    pub session: String,
    pub repo: String,
    pub branch: String,
    /// Whether to create a worktree. Without one, the agent starts in `repo`.
    pub worktree: bool,
    /// The agent's command, ending with the prompt.
    pub agent: Vec<String>,
}

#[derive(Debug)]
pub struct Opened {
    pub session_id: String,
    pub window_id: String,
}

impl Launch {
    /// The command the new tab runs.
    pub fn tab_command(&self) -> Vec<String> {
        if !self.worktree {
            let mut argv: Vec<String> = ["sh", "-c", NO_WORKTREE_SCRIPT, "rex-launch"].map(String::from).to_vec();
            argv.extend(self.agent.iter().cloned());
            return argv;
        }
        let mut argv: Vec<String> =
            ["sh", "-c", TAB_SCRIPT, "rex-launch"].map(String::from).to_vec();
        argv.push(self.repo.clone());
        argv.push(self.branch.clone());
        argv.extend(self.agent.iter().cloned());
        argv
    }

    /// The `rex` arguments that open the tab, in `session_id` when the project
    /// already has a session and in a new session otherwise.
    pub fn rex_args(&self, session_id: Option<&str>) -> Vec<String> {
        let mut args: Vec<String> = match session_id {
            Some(id) => vec!["window".into(), "new".into(), "-s".into(), id.into(), self.branch.clone()],
            None => vec!["new".into(), self.session.clone(), "--window".into(), self.branch.clone()],
        };
        args.extend(["-c".into(), self.repo.clone(), "--json".into(), "--".into()]);
        args.extend(self.tab_command());
        args
    }

    /// A readable summary for `--dry-run`.
    pub fn describe(&self) -> String {
        let target = self.host.as_deref().unwrap_or("this computer");
        let agent = self.agent.iter().map(|a| format!("{a:?}")).collect::<Vec<_>>().join(" ");
        format!(
            "host:    {target}\nsession: {}\ntab:     {}\nrepo:    {}\nagent:   {agent}",
            self.session, self.branch, self.repo
        )
    }

    pub fn open(&self) -> Result<Opened> {
        let host = self.host.as_deref();
        let sessions = rex_json(host, &["ls", "--json"])?;
        let existing = sessions["sessions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|s| s["label"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(&self.session)))
            .and_then(|s| s["session_id"].as_str())
            .map(String::from);

        let created = rex_json(host, &self.rex_args(existing.as_deref()))?;
        let (session_id, window_id) = match existing {
            Some(id) => (Some(id), created["window_id"].as_str().map(String::from)),
            None => (
                created["session_id"].as_str().map(String::from),
                created["initial_windows"][0]["window_id"].as_str().map(String::from),
            ),
        };
        match (session_id, window_id) {
            (Some(session_id), Some(window_id)) => Ok(Opened { session_id, window_id }),
            _ => bail!("rex didn't report the new tab: {created}"),
        }
    }
}

/// Asks the Rex app that opened the popup to show the new tab.
pub fn show(client: &str, opened: &Opened) -> Result<()> {
    let session = format!("session_id={}", opened.session_id);
    let window = format!("window_id={}", opened.window_id);
    run(rex(None).args(["-C", client, "do", "session.select", &session, &window])).map(drop)
}

/// The labels of the Rex hosts this machine knows, from `rex hosts`. Empty
/// when they can't be read; the config's hosts still show up.
pub fn hosts() -> Vec<String> {
    let Ok(listing) = rex_json(None, &["hosts", "--json"]) else {
        return Vec::new();
    };
    listing["hosts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| h["host"]["label"].as_str())
        .filter(|label| !label.is_empty())
        .map(String::from)
        .collect()
}

pub fn rex(host: Option<&str>) -> Command {
    // Prefer the rex that belongs to the server running this terminal.
    let bin = std::env::var_os("REX_BIN_DIR")
        .map(|dir| PathBuf::from(dir).join("rex"))
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from("rex"));
    let mut cmd = Command::new(bin);
    if let Some(host) = host {
        cmd.args(["-S", host]);
    }
    cmd
}

pub fn rex_json(host: Option<&str>, args: &[impl AsRef<std::ffi::OsStr>]) -> Result<Value> {
    let stdout = run(rex(host).args(args))?;
    serde_json::from_slice(&stdout).context("reading rex's JSON output")
}

pub fn run(cmd: &mut Command) -> Result<Vec<u8>> {
    let output = cmd.output().context("running rex")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim().trim_start_matches("Error: ");
        return Err(anyhow!("{message}"));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch() -> Launch {
        Launch {
            host: Some("studio".into()),
            session: "api".into(),
            repo: "/Users/me/Code/api".into(),
            branch: "fix-login".into(),
            worktree: true,
            agent: vec!["claude".into(), "--model".into(), "opus".into(), "fix \"it\"\n{{ now }}".into()],
        }
    }

    #[test]
    fn tab_command_passes_the_prompt_through_unchanged() {
        let argv = launch().tab_command();
        assert_eq!(&argv[..2], ["sh", "-c"]);
        assert_eq!(&argv[3..6], ["rex-launch", "/Users/me/Code/api", "fix-login"]);
        assert_eq!(argv.last().unwrap(), "fix \"it\"\n{{ now }}");
    }

    #[test]
    fn new_session_args() {
        let args = launch().rex_args(None);
        assert_eq!(
            &args[..8],
            ["new", "api", "--window", "fix-login", "-c", "/Users/me/Code/api", "--json", "--"]
        );
        assert_eq!(args[8], "sh");
    }

    #[test]
    fn existing_session_args() {
        let args = launch().rex_args(Some("session:1"));
        assert_eq!(
            &args[..9],
            ["window", "new", "-s", "session:1", "fix-login", "-c", "/Users/me/Code/api", "--json", "--"]
        );
    }

    #[test]
    fn no_worktree_runs_the_agent_directly() {
        let argv = Launch { worktree: false, ..launch() }.tab_command();
        assert_eq!(&argv[..4], ["sh", "-c", NO_WORKTREE_SCRIPT, "rex-launch"]);
        assert_eq!(argv[4], "claude");
    }

    /// Runs the real tab script against a repository where the branch name
    /// is already taken, with stand-ins for `wt` (which adds the worktree)
    /// and `rex` (which records the rename).
    #[test]
    fn tab_script_picks_a_free_branch_name_and_renames_the_tab() {
        let dir = std::env::temp_dir().join(format!("rex-launch-tab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (repo, bin) = (dir.join("repo"), dir.join("bin"));
        std::fs::create_dir_all(&bin).unwrap();
        let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap().status.success());
        std::fs::create_dir_all(&repo).unwrap();
        git(&["init", "-q", "-b", "main"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "one"]);
        git(&["branch", "fix-login"]);
        let fake = |name: &str, body: &str| {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::os::unix::fs::PermissionsExt::set_mode(&mut std::fs::metadata(&path).unwrap().permissions(), 0o755);
            Command::new("chmod").arg("+x").arg(&path).status().unwrap();
        };
        // wt -C REPO switch --create BRANCH --no-cd --yes
        fake("wt", r#"git -C "$2" worktree add -q -b "$5" "$2.$5""#);
        fake("rex", &format!(r#"echo "$@" >> "{}""#, dir.join("rex.log").display()));
        // The user's shell: runs `-lic COMMAND NAME ARGS...` and nothing else,
        // so the final interactive shell exits at once.
        fake("user-shell", r#"[ "$1" = -lic ] || exit 0; shift; command=$1; shift; exec sh -c "$command" "$@""#);

        let launch = Launch {
            repo: repo.display().to_string(),
            branch: "fix-login".into(),
            agent: vec!["sh".into(), "-c".into(), "echo agent on $(git branch --show-current)".into()],
            ..launch()
        };
        let argv = launch.tab_command();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        let shell = bin.join("user-shell");
        let output = Command::new(&argv[0]).args(&argv[1..]).env("PATH", path).env("SHELL", shell).output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("agent on fix-login-2"), "{stdout}");
        let renamed = std::fs::read_to_string(dir.join("rex.log")).unwrap();
        assert_eq!(renamed.trim(), "window rename fix-login-2");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tab_script_is_valid_sh() {
        let status = Command::new("sh").args(["-n", "-c", TAB_SCRIPT]).status().unwrap();
        assert!(status.success());
    }
}
