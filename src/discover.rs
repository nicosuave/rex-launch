//! Finding the git repositories in each connection's code folders.
//!
//! A repository is a folder whose `.git` is a directory, so worktrees (whose
//! `.git` is a file) never show up. Folders are searched two levels deep, as
//! in `~/Code/editor` and `~/Code/work/api`, without looking inside
//! repositories. Clones of the same remote are merged into one project.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::Project;
use crate::launch;

/// Folder names that are never searched, besides hidden ones.
const ALWAYS_SKIPPED: &[&str] = &["node_modules", "target"];

/// The remote scan, run in a short-lived terminal on the host.
const SCAN_SCRIPT: &str = include_str!("scan.sh");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub path: String,
    /// When git last changed HEAD (a commit, checkout, or pull), unix seconds.
    pub activity: u64,
    pub origin: Option<String>,
}

/// Finds the repositories in `folders` on this computer.
pub fn scan_local(folders: &[String], exclude: &[String]) -> Vec<Repo> {
    let mut repos = Vec::new();
    for folder in folders {
        for dir in subdirs(&expand_home(folder), exclude) {
            if is_repo(&dir) {
                repos.push(describe(&dir));
                continue;
            }
            for child in subdirs(&dir, exclude) {
                if is_repo(&child) {
                    repos.push(describe(&child));
                }
            }
        }
    }
    repos
}

/// Finds the repositories in `folders` on a Rex host by running the scan in
/// a short-lived session there.
pub fn scan_remote(host: &str, folders: &[String], exclude: &[String]) -> Result<Vec<Repo>> {
    let skip = exclude.join(" ");
    let mut args = vec!["new", "rex-launch-scan", "--keep-open", "--shell=none", "--json", "--"];
    args.extend(["sh", "-c", SCAN_SCRIPT, "rex-launch", &skip]);
    args.extend(folders.iter().map(String::as_str));
    let created = launch::rex_json(Some(host), &args)?;
    let session = created["session_id"].as_str().context("rex didn't report the scan's session")?;
    let block = created["initial_windows"][0]["block_ids"][0].as_str().context("rex didn't report the scan's terminal")?;

    let output = launch::run(launch::rex(Some(host)).args(["wait", "-s", session, block, "--for", "60s"]))
        .and_then(|_| launch::run(launch::rex(Some(host)).args(["capture", "-s", session, "-b", block, "--unwrap"])));
    let _ = launch::run(launch::rex(Some(host)).args(["kill", session]));
    parse_scan(&String::from_utf8_lossy(&output?))
}

/// The projects for `repos` on `host`, one per remote. Of several clones of
/// the same remote, the one named after it is kept, or else the most active;
/// the project gets the most recent activity of them all.
pub fn projects(host: Option<&str>, repos: &[Repo]) -> Vec<Project> {
    let mut groups: HashMap<String, Vec<&Repo>> = HashMap::new();
    for repo in repos {
        let key = repo.origin.as_deref().map_or_else(|| repo.path.clone(), origin_key);
        groups.entry(key).or_default().push(repo);
    }
    let mut projects: Vec<Project> = groups
        .into_values()
        .map(|clones| {
            let wanted = clones[0].origin.as_deref().map(origin_name);
            let keep = clones
                .iter()
                .find(|r| wanted.is_some_and(|w| dir_name(&r.path).eq_ignore_ascii_case(w)))
                .or_else(|| clones.iter().max_by_key(|r| r.activity))
                .expect("groups are never empty");
            Project {
                name: dir_name(&keep.path).to_string(),
                path: keep.path.clone(),
                host: host.map(String::from),
                activity: clones.iter().map(|r| r.activity).max().unwrap_or(0),
                scratch: false,
            }
        })
        .collect();
    projects.sort_by(|a, b| a.path.cmp(&b.path));
    projects
}

/// Reads the scan script's output.
fn parse_scan(text: &str) -> Result<Vec<Repo>> {
    if !text.lines().any(|line| line.trim_end() == "DONE") {
        bail!("the scan didn't finish: {}", text.trim());
    }
    let mut repos: Vec<Repo> = Vec::new();
    for line in text.lines().map(str::trim_end) {
        if let Some(path) = line.strip_prefix("R:") {
            repos.push(Repo { path: path.to_string(), activity: 0, origin: None });
        } else if let (Some(time), Some(repo)) = (line.strip_prefix("T:"), repos.last_mut()) {
            repo.activity = time.trim().parse().unwrap_or(0);
        } else if let (Some(url), Some(repo)) = (line.strip_prefix("U:"), repos.last_mut()) {
            repo.origin = Some(url.trim().to_string()).filter(|u| !u.is_empty());
        }
    }
    Ok(repos)
}

fn subdirs(dir: &Path, exclude: &[String]) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && !skipped(path, exclude))
        .collect();
    dirs.sort();
    dirs
}

fn skipped(path: &Path, exclude: &[String]) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.starts_with('.') || ALWAYS_SKIPPED.contains(&name) || exclude.iter().any(|e| e == name)
}

fn is_repo(dir: &Path) -> bool {
    dir.join(".git").is_dir()
}

fn describe(dir: &Path) -> Repo {
    let git = dir.join(".git");
    let reflog = git.join("logs/HEAD");
    let marker = if reflog.is_file() { reflog } else { git.join("HEAD") };
    let activity = std::fs::metadata(marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let origin = std::fs::read_to_string(git.join("config")).ok().and_then(|c| origin_url(&c));
    Repo { path: dir.display().to_string(), activity, origin }
}

/// The url of `[remote "origin"]` in a git config file.
fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_origin = line == r#"[remote "origin"]"#;
        } else if in_origin
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "url"
        {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// The same key for every way of writing a remote, such as
/// `git@github.com:org/repo.git` and `https://github.com/org/repo`.
fn origin_key(url: &str) -> String {
    let url = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let url = url.split_once("://").map_or(url, |(_, rest)| rest);
    let url = url.split_once('@').map_or(url, |(_, rest)| rest);
    url.replacen(':', "/", 1).to_lowercase()
}

/// The repository's name in its remote url.
fn origin_name(url: &str) -> &str {
    let url = url.trim().trim_end_matches('/').trim_end_matches(".git");
    url.rsplit(['/', ':']).next().unwrap_or(url)
}

fn dir_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

fn expand_home(path: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    match path {
        "~" => home(),
        _ => path.strip_prefix("~/").map_or_else(|| PathBuf::from(path), |rest| home().join(rest)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A code folder with a repository, an organization folder holding two
    /// clones of one remote and a worktree, a nested repo inside a repository,
    /// and an excluded folder.
    fn fixture(test: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rex-launch-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let repo = |path: &str, origin: Option<&str>| {
            let git = root.join(path).join(".git");
            fs::create_dir_all(git.join("logs")).unwrap();
            fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
            fs::write(git.join("logs/HEAD"), "").unwrap();
            let remote = origin.map_or(String::new(), |u| format!("[remote \"origin\"]\n\turl = {u}\n"));
            fs::write(git.join("config"), format!("[core]\n\tbare = false\n{remote}")).unwrap();
        };
        repo("editor", Some("git@github.com:me/editor.git"));
        repo("work/data", Some("https://github.com/acme/data"));
        repo("work/data-fix", Some("git@github.com:acme/data.git"));
        repo("editor/vendor/inner", None);
        repo("_archive/old", None);
        fs::create_dir_all(root.join("work/data.worktree")).unwrap();
        fs::write(root.join("work/data.worktree/.git"), "gitdir: ../data/.git/worktrees/x\n").unwrap();
        root
    }

    fn names(projects: &[Project]) -> Vec<&str> {
        projects.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn finds_repositories_two_levels_deep_and_skips_worktrees() {
        let root = fixture("finds_reposi");
        let repos = scan_local(&[root.display().to_string()], &["_archive".into()]);
        let mut found: Vec<&str> = repos.iter().map(|r| r.path.strip_prefix(&*root.to_string_lossy()).unwrap()).collect();
        found.sort();
        assert_eq!(found, ["/editor", "/work/data", "/work/data-fix"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn merges_clones_and_keeps_the_one_named_after_the_remote() {
        let root = fixture("merges_clone");
        let repos = scan_local(&[root.display().to_string()], &["_archive".into()]);
        let projects = projects(Some("studio"), &repos);
        let mut found = names(&projects);
        found.sort();
        assert_eq!(found, ["data", "editor"]);
        assert!(projects.iter().all(|p| p.host.as_deref() == Some("studio")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_remote_script_finds_the_same_repositories() {
        let root = fixture("the_remote_s");
        let local = scan_local(&[root.display().to_string()], &["_archive".into()]);
        let output = std::process::Command::new("sh")
            .args(["-c", SCAN_SCRIPT, "rex-launch", "_archive"])
            .arg(&root)
            .output()
            .unwrap();
        let mut remote = parse_scan(&String::from_utf8_lossy(&output.stdout)).unwrap();
        let mut local = local;
        local.sort_by(|a, b| a.path.cmp(&b.path));
        remote.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(remote, local);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn origin_keys_ignore_how_the_remote_is_written() {
        assert_eq!(origin_key("git@github.com:Org/Repo.git"), origin_key("https://github.com/org/repo/"));
        assert_eq!(origin_name("git@github.com:org/repo.git"), "repo");
    }

    #[test]
    fn unfinished_scans_are_errors() {
        assert!(parse_scan("R:/x\nT:1\nU:\n").is_err());
        assert_eq!(parse_scan("R:/x\nT:5\nU:\nDONE\n").unwrap()[0].activity, 5);
    }
}

