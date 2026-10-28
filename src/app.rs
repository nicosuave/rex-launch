//! What the popup shows and how keys and clicks change it.

use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};

use crate::chrome::Chrome;
use crate::config::{Access, Agent, Config, Project, State};
use crate::discover::{self, Repo};
use crate::editor::Editor;
use crate::launch::Launch;
use crate::models::{Harness, Model};
use crate::{fuzzy, slug};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Projects,
    Hosts,
    Models,
    /// Typing goes to the branch chip.
    Branch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Idle,
    Busy(String),
    Error(String),
    Notice(String),
}

/// Something on screen that reacts to a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Project,
    Host,
    Model,
    Access,
    Branch,
    Send,
    ProjectRow(usize),
    HostRow(usize),
    ModelRow(usize),
    /// Part of an open menu that isn't a row; clicking it does nothing.
    Menu,
}

pub enum Action {
    Submit,
}

pub struct App {
    pub config: Config,
    /// The connections to choose from: this computer (`None`), then Rex hosts.
    pub hosts: Vec<Option<String>>,
    /// Every connection's projects, most recently used first.
    pub projects: Vec<Project>,
    pub project: usize,
    /// Connections whose projects are still being looked for.
    pub scanning: Vec<Option<String>>,
    /// The models each agent offers, then any agents from the config.
    pub agents: Vec<Agent>,
    /// Agents whose model lists are still being asked for.
    pub fetching: Vec<Harness>,
    pub agent: usize,
    pub access: usize,
    pub editor: Editor,
    /// The branch name, generated from the prompt until the user edits it.
    pub branch: String,
    branch_edited: bool,
    pub overlay: Overlay,
    pub query: String,
    pub selected: usize,
    /// The first row a menu shows; the renderer keeps `selected` in view.
    pub menu_scroll: usize,
    pub status: Status,
    /// Clickable areas from the last frame, topmost last.
    pub hits: Vec<(Rect, Hit)>,
    pub hover: Option<Position>,
    /// The width the prompt was last wrapped at, for up and down.
    pub text_width: usize,
    pub client: Option<String>,
    /// Kitty graphics for rounded corners, when the terminal supports them.
    pub chrome: Option<Chrome>,
    pub done: bool,
    state: State,
}

impl App {
    /// `rex_hosts` are the host labels from `rex hosts`; hosts that only appear
    /// in the config are added after them. `repos` are the repositories found
    /// so far on each connection, keyed by host label ("" for this computer).
    pub fn new(
        config: Config,
        state: State,
        client: Option<String>,
        rex_hosts: Vec<String>,
        repos: &HashMap<String, Vec<Repo>>,
        models: &[Model],
    ) -> Result<App> {
        let mut hosts = vec![None];
        let configured = config.projects.iter().filter_map(|p| p.host.clone());
        let scanned = repos.keys().filter(|h| !h.is_empty()).cloned();
        for host in rex_hosts.into_iter().chain(configured).chain(scanned) {
            if !hosts.contains(&Some(host.clone())) {
                hosts.push(Some(host));
            }
        }
        let mut projects = config.projects.clone();
        for (host, found) in repos {
            let host = Some(host.as_str()).filter(|h| !h.is_empty());
            for project in discover::projects(host, found) {
                if !projects.iter().any(|p| p.key() == project.key()) {
                    projects.push(project);
                }
            }
        }
        for host in &hosts {
            projects.push(Project::scratch(host.as_deref()));
        }
        sort_projects(&mut projects, &state);
        let project = state
            .project
            .as_ref()
            .and_then(|key| projects.iter().position(|p| &p.key() == key))
            .unwrap_or(0);
        let agents = build_agents(models, &config);
        let agent = state
            .agent
            .as_ref()
            .and_then(|id| agents.iter().position(|a| &a.id == id))
            .unwrap_or(0);
        let mut app = App {
            config,
            hosts,
            projects,
            project,
            scanning: Vec::new(),
            agents,
            fetching: Vec::new(),
            agent,
            access: 0,
            editor: Editor::default(),
            branch: String::new(),
            branch_edited: false,
            overlay: Overlay::None,
            query: String::new(),
            selected: 0,
            menu_scroll: 0,
            status: Status::Idle,
            hits: Vec::new(),
            hover: None,
            text_width: 40,
            client,
            chrome: None,
            done: false,
            state,
        };
        app.access = app.remembered_access();
        Ok(app)
    }

    /// Replaces a connection's discovered projects with the result of a
    /// fresh scan, keeping the current project selected.
    pub fn set_repos(&mut self, host: Option<&str>, repos: &[Repo]) {
        let current = self.current_project().key();
        let configured: Vec<String> = self.config.projects.iter().map(Project::key).collect();
        self.projects
            .retain(|p| p.host.as_deref() != host || p.scratch || configured.contains(&p.key()));
        for project in discover::projects(host, repos) {
            if !self.projects.iter().any(|p| p.key() == project.key()) {
                self.projects.push(project);
            }
        }
        sort_projects(&mut self.projects, &self.state);
        self.project = self
            .projects
            .iter()
            .position(|p| p.key() == current)
            .unwrap_or(0);
        self.scanning.retain(|h| h.as_deref() != host);
    }

    pub fn current_project(&self) -> &Project {
        &self.projects[self.project]
    }

    pub fn current_agent(&self) -> &Agent {
        &self.agents[self.agent]
    }

    /// The effort the agent at `index` runs with: the one last used with it,
    /// else its default, else "high", else its first. `None` when the agent
    /// has no effort levels.
    pub fn effort(&self, index: usize) -> Option<&str> {
        let agent = &self.agents[index];
        let offered = |e: &&str| agent.efforts.iter().any(|x| x == e);
        self.state
            .efforts
            .get(&agent.id)
            .map(String::as_str)
            .filter(offered)
            .or(agent.default_effort.as_deref().filter(offered))
            .or(Some("high").filter(offered))
            .or(agent.efforts.first().map(String::as_str))
    }

    /// Rebuilds the model menu after `harness` reported its models; `models`
    /// is every agent's latest list. The current model stays selected.
    pub fn set_models(&mut self, harness: Harness, models: &[Model]) {
        let current = self.current_agent().id.clone();
        self.agents = build_agents(models, &self.config);
        self.agent = self
            .agents
            .iter()
            .position(|a| a.id == current)
            .unwrap_or(0);
        self.access = self.remembered_access();
        self.fetching.retain(|h| *h != harness);
    }

    pub fn current_access(&self) -> &Access {
        &self.current_agent().access[self.access]
    }

    pub fn project_available(&self, project: &Project) -> bool {
        project.scratch || project.host.is_some() || Path::new(&project.path).is_dir()
    }

    pub fn busy(&self) -> bool {
        matches!(self.status, Status::Busy(_))
    }

    /// The connection the current project is on.
    pub fn current_host(&self) -> Option<&str> {
        self.current_project().host.as_deref()
    }

    /// The indices of the projects on `host`, in config order.
    pub fn projects_on(&self, host: Option<&str>) -> Vec<usize> {
        let on_host = |(i, p): (usize, &Project)| (p.host.as_deref() == host).then_some(i);
        self.projects
            .iter()
            .enumerate()
            .filter_map(on_host)
            .collect()
    }

    /// The current connection's projects matching the picker's query, best first.
    pub fn project_matches(&self) -> Vec<usize> {
        let pool = self.projects_on(self.current_host());
        let names = pool.iter().map(|&i| self.projects[i].name.as_str());
        fuzzy::rank(&self.query, names)
            .into_iter()
            .map(|i| pool[i])
            .collect()
    }

    /// The connections matching the picker's query, best first.
    pub fn host_matches(&self) -> Vec<usize> {
        let labels = self.hosts.iter().map(|h| host_label(h.as_deref()));
        fuzzy::rank(&self.query, labels)
    }

    /// The agents matching the picker's query, best first.
    pub fn agent_matches(&self) -> Vec<usize> {
        let haystacks: Vec<String> = self
            .agents
            .iter()
            .map(|a| format!("{} {} {}", a.harness, a.name, a.detail))
            .collect();
        fuzzy::rank(&self.query, haystacks.iter().map(String::as_str))
    }

    pub fn can_submit(&self) -> bool {
        !self.editor.text().trim().is_empty() && !self.busy()
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        if self.busy() {
            return None;
        }
        if matches!(self.status, Status::Error(_)) {
            self.status = Status::Idle;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.done = true;
            return None;
        }
        match self.overlay {
            Overlay::None => self.prompt_key(key),
            Overlay::Projects | Overlay::Hosts | Overlay::Models => {
                self.menu_key(key);
                None
            }
            Overlay::Branch => {
                self.branch_key(key);
                None
            }
        }
    }

    pub fn on_paste(&mut self, text: &str) {
        match self.overlay {
            Overlay::None => {
                self.editor.insert(text);
                self.prompt_changed();
            }
            Overlay::Projects | Overlay::Hosts | Overlay::Models => {
                self.query.push_str(text.lines().next().unwrap_or(""));
                self.selected = 0;
            }
            Overlay::Branch => {
                self.branch.push_str(text.lines().next().unwrap_or(""));
                self.branch_edited = true;
            }
        }
    }

    pub fn on_mouse(&mut self, event: MouseEvent) -> Option<Action> {
        let at = Position::new(event.column, event.row);
        match event.kind {
            MouseEventKind::Moved => self.hover = Some(at),
            MouseEventKind::Down(MouseButton::Left) if !self.busy() => {
                let hit = self
                    .hits
                    .iter()
                    .rev()
                    .find(|(r, _)| r.contains(at))
                    .map(|&(_, h)| h);
                match hit {
                    Some(Hit::Project) => self.toggle(Overlay::Projects),
                    Some(Hit::Host) => self.toggle(Overlay::Hosts),
                    Some(Hit::Model) => self.toggle(Overlay::Models),
                    Some(Hit::Access) => self.cycle_access(),
                    Some(Hit::Branch) => self.toggle(Overlay::Branch),
                    Some(Hit::Send) => return self.can_submit().then_some(Action::Submit),
                    Some(Hit::ProjectRow(i)) => self.choose_project(i),
                    Some(Hit::HostRow(i)) => self.choose_host(i),
                    Some(Hit::ModelRow(i)) => self.choose_agent(i),
                    Some(Hit::Menu) => {}
                    None => self.close_overlay(),
                }
            }
            MouseEventKind::ScrollUp => self.move_selection(-1),
            MouseEventKind::ScrollDown => self.move_selection(1),
            _ => {}
        }
        None
    }

    /// The launch the current choices describe, or why there can't be one.
    pub fn plan(&self) -> Result<Launch, String> {
        let prompt = self.editor.text().trim();
        if prompt.is_empty() {
            return Err("Write a prompt first".into());
        }
        let project = self.current_project();
        if !self.project_available(project) {
            return Err(format!("{} doesn't exist", project.path));
        }
        // The tab makes the name unique on the repository's own machine.
        let mut branch = slug::sanitize_branch(&self.branch);
        if branch.is_empty() {
            branch = "task".into();
        }
        let current = self.current_agent();
        let mut agent = current.command.clone();
        if let (Some(kind), Some(effort)) = (current.kind, self.effort(self.agent)) {
            agent.extend(kind.effort_args(effort));
        }
        agent.extend(self.current_access().args.iter().cloned());
        // A leading space keeps a prompt such as "-v is broken" from being read as a flag.
        agent.push(if prompt.starts_with('-') {
            format!(" {prompt}")
        } else {
            prompt.to_string()
        });
        Ok(Launch {
            host: project.host.clone(),
            session: project.name.clone(),
            repo: project.path.clone(),
            branch,
            worktree: !project.scratch,
            agent,
        })
    }

    /// Remembers the project, agent, and access mode for next time.
    pub fn save_state(&mut self) {
        let key = self.current_project().key();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.state.launched.insert(key.clone(), now);
        self.state.project = Some(key);
        self.state.agent = Some(self.current_agent().id.clone());
        let (agent, access) = (
            self.current_agent().id.clone(),
            self.current_access().name.clone(),
        );
        self.state.access.insert(agent.clone(), access);
        if let Some(effort) = self.effort(self.agent).map(String::from) {
            self.state.efforts.insert(agent, effort);
        }
        // Losing the remembered choices isn't worth failing a launch over.
        let _ = self.state.save();
    }

    fn prompt_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let width = self.text_width;
        let e = &mut self.editor;
        match key.code {
            KeyCode::Enter if shift || alt => e.insert("\n"),
            KeyCode::Char('j') if ctrl => e.insert("\n"),
            KeyCode::Enter => return self.can_submit().then_some(Action::Submit),
            KeyCode::Esc => self.done = true,
            KeyCode::Char('p') if ctrl => self.toggle(Overlay::Projects),
            KeyCode::Char('r') if ctrl => self.toggle(Overlay::Hosts),
            KeyCode::Char('o') if ctrl => self.toggle(Overlay::Models),
            KeyCode::Char('s') if ctrl => self.cycle_access(),
            KeyCode::Char('b') if ctrl => self.toggle(Overlay::Branch),
            KeyCode::Char('a') if ctrl => e.line_start(),
            KeyCode::Char('e') if ctrl => e.line_end(),
            KeyCode::Char('w') if ctrl => e.delete_word_left(),
            KeyCode::Char('h') if ctrl => e.delete_word_left(),
            KeyCode::Char('u') if ctrl => e.kill_to_line_start(),
            KeyCode::Char('k') if ctrl => e.kill_to_line_end(),
            KeyCode::Char('b') if alt => e.word_left(),
            KeyCode::Char('f') if alt => e.word_right(),
            KeyCode::Backspace if alt || ctrl => e.delete_word_left(),
            KeyCode::Backspace => e.backspace(),
            KeyCode::Delete => e.delete(),
            KeyCode::Left if alt || ctrl => e.word_left(),
            KeyCode::Right if alt || ctrl => e.word_right(),
            KeyCode::Left => e.left(),
            KeyCode::Right => e.right(),
            KeyCode::Up => e.move_vertical(width, -1),
            KeyCode::Down => e.move_vertical(width, 1),
            KeyCode::Home => e.line_start(),
            KeyCode::End => e.line_end(),
            KeyCode::Char(c) if !ctrl && !alt => e.insert(c.encode_utf8(&mut [0; 4])),
            _ => return None,
        }
        self.prompt_changed();
        None
    }

    fn menu_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Left if self.overlay == Overlay::Models => self.cycle_effort(-1),
            KeyCode::Right if self.overlay == Overlay::Models => self.cycle_effort(1),
            KeyCode::Esc | KeyCode::Tab => self.close_overlay(),
            KeyCode::Enter => {
                let matches = self.menu_matches();
                if let Some(&i) = matches.get(self.selected) {
                    match self.overlay {
                        Overlay::Projects => self.choose_project(i),
                        Overlay::Hosts => self.choose_host(i),
                        _ => self.choose_agent(i),
                    }
                }
            }
            KeyCode::Char('p') if ctrl && self.overlay == Overlay::Projects => self.close_overlay(),
            KeyCode::Char('r') if ctrl && self.overlay == Overlay::Hosts => self.close_overlay(),
            KeyCode::Char('o') if ctrl && self.overlay == Overlay::Models => self.close_overlay(),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('p' | 'k') if ctrl => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Char('n' | 'j') if ctrl => self.move_selection(1),
            KeyCode::Backspace => {
                self.query.pop();
                self.selected = 0;
            }
            KeyCode::Char('u' | 'w') if ctrl => {
                self.query.clear();
                self.selected = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                self.query.push(c);
                self.selected = 0;
            }
            _ => {}
        }
    }

    fn branch_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter | KeyCode::Esc | KeyCode::Tab => self.close_overlay(),
            KeyCode::Char('b') if ctrl => self.close_overlay(),
            KeyCode::Backspace => {
                self.branch.pop();
                self.branch_edited = true;
            }
            KeyCode::Char('u' | 'w') if ctrl => {
                self.branch.clear();
                self.branch_edited = true;
            }
            KeyCode::Char(c) if !ctrl => {
                self.branch.push(if c == ' ' { '-' } else { c });
                self.branch_edited = true;
            }
            _ => {}
        }
    }

    fn menu_matches(&self) -> Vec<usize> {
        match self.overlay {
            Overlay::Projects => self.project_matches(),
            Overlay::Hosts => self.host_matches(),
            Overlay::Models => self.agent_matches(),
            _ => Vec::new(),
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let count = self.menu_matches().len();
        if count > 0 {
            self.selected = self.selected.saturating_add_signed(delta).min(count - 1);
        }
    }

    fn toggle(&mut self, overlay: Overlay) {
        if self.overlay == overlay {
            self.close_overlay();
            return;
        }
        self.overlay = overlay;
        self.query.clear();
        self.menu_scroll = 0;
        // Start the menu on the current choice.
        self.selected = match overlay {
            Overlay::Projects => self
                .project_matches()
                .iter()
                .position(|&i| i == self.project),
            Overlay::Hosts => {
                let current = self.current_host().map(String::from);
                self.host_matches()
                    .iter()
                    .position(|&i| self.hosts[i] == current)
            }
            Overlay::Models => self.agent_matches().iter().position(|&i| i == self.agent),
            _ => None,
        }
        .unwrap_or(0);
    }

    fn close_overlay(&mut self) {
        if self.overlay == Overlay::Branch {
            self.branch = slug::sanitize_branch(&self.branch);
            if self.branch.is_empty() {
                // Clearing the name hands it back to the prompt.
                self.branch_edited = false;
                self.prompt_changed();
            }
        }
        self.overlay = Overlay::None;
    }

    fn choose_project(&mut self, index: usize) {
        self.project = index;
        self.close_overlay();
    }

    /// Switches to the most recent project on the chosen connection, unless
    /// the current project is already there. Every connection has at least
    /// its "No project" entry.
    fn choose_host(&mut self, index: usize) {
        let host = self.hosts[index].clone();
        self.close_overlay();
        if self.current_host() == host.as_deref() {
            return;
        }
        if let Some(&project) = self.projects_on(host.as_deref()).first() {
            self.project = project;
        }
    }

    fn choose_agent(&mut self, index: usize) {
        self.agent = index;
        self.access = self.remembered_access();
        self.close_overlay();
    }

    /// Moves the highlighted model's effort one level up or down.
    fn cycle_effort(&mut self, delta: isize) {
        let Some(&index) = self.agent_matches().get(self.selected) else {
            return;
        };
        let Some(current) = self.effort(index).map(String::from) else {
            return;
        };
        let agent = &self.agents[index];
        let at = agent
            .efforts
            .iter()
            .position(|e| *e == current)
            .unwrap_or(0);
        let next = at.saturating_add_signed(delta).min(agent.efforts.len() - 1);
        let (id, effort) = (agent.id.clone(), agent.efforts[next].clone());
        self.state.efforts.insert(id, effort);
    }

    fn cycle_access(&mut self) {
        self.access = (self.access + 1) % self.current_agent().access.len();
    }

    fn remembered_access(&self) -> usize {
        let agent = self.current_agent();
        self.state
            .access
            .get(&agent.id)
            .and_then(|name| agent.access.iter().position(|a| &a.name == name))
            .unwrap_or(0)
    }

    fn prompt_changed(&mut self) {
        if !self.branch_edited {
            self.branch = slug::branch_from_prompt(self.editor.text());
        }
    }
}

/// The menu's agents: one per model the agents reported, then the config's
/// own agents. Before any agent has answered, the built-in fallbacks.
fn build_agents(models: &[Model], config: &Config) -> Vec<Agent> {
    let mut agents: Vec<Agent> = models.iter().map(Agent::from_model).collect();
    agents.extend(config.agents.iter().cloned());
    if agents.is_empty() {
        agents = Config::fallback_agents();
    }
    agents
}

/// Recently launched projects first, then the most recently active, with
/// "No project" last.
fn sort_projects(projects: &mut [Project], state: &State) {
    let launched = |p: &Project| state.launched.get(&p.key()).copied().unwrap_or(0);
    projects.sort_by(|a, b| {
        a.scratch
            .cmp(&b.scratch)
            .then(launched(b).cmp(&launched(a)))
            .then(b.activity.cmp(&a.activity))
            .then(a.name.cmp(&b.name))
    });
}

/// How a connection is shown: its host label, or "This computer".
pub fn host_label(host: Option<&str>) -> &str {
    host.unwrap_or("This computer")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        toml::from_str(
            r#"
            [[project]]
            name = "webapp"
            path = "/Users/me/Code/webapp"

            [[project]]
            name = "data"
            path = "/Users/me/Code/data"
            host = "studio"

            [[project]]
            name = "api"
            path = "/Users/me/Code/api"
            host = "studio"

            [[agent]]
            id = "claude"
            harness = "Claude"
            name = "Opus"
            command = ["claude"]
            access = [{ name = "Default" }]
            "#,
        )
        .unwrap()
    }

    fn app() -> App {
        App::new(
            config(),
            State::default(),
            None,
            vec!["buildbox".into(), "studio".into()],
            &HashMap::new(),
            &[],
        )
        .unwrap()
    }

    fn host_index(app: &App, label: &str) -> usize {
        app.hosts
            .iter()
            .position(|h| h.as_deref() == Some(label))
            .unwrap()
    }

    #[test]
    fn lists_this_computer_then_rex_hosts_without_duplicates() {
        let app = app();
        assert_eq!(
            app.hosts,
            [None, Some("buildbox".into()), Some("studio".into())]
        );
    }

    #[test]
    fn switching_connections_switches_to_a_project_there() {
        let mut app = app();
        app.choose_host(0);
        assert_eq!(app.current_project().name, "webapp");
        app.choose_host(host_index(&app, "studio"));
        assert_eq!(app.current_host(), Some("studio"));
        let names: Vec<&str> = app
            .project_matches()
            .iter()
            .map(|&i| app.projects[i].name.as_str())
            .collect();
        assert_eq!(names, ["api", "data", "No project"]);
    }

    #[test]
    fn a_connection_without_repositories_offers_no_project() {
        let mut app = app();
        app.choose_host(host_index(&app, "buildbox"));
        assert_eq!(app.current_host(), Some("buildbox"));
        assert!(app.current_project().scratch);
        let plan = {
            app.editor.insert("look around");
            app.plan().unwrap()
        };
        assert!(!plan.worktree);
        assert_eq!(plan.host.as_deref(), Some("buildbox"));
    }

    #[test]
    fn a_scan_replaces_a_connections_projects_and_keeps_the_selection() {
        let mut app = app();
        app.choose_host(0);
        let repo = |path: &str, activity| Repo {
            path: path.into(),
            activity,
            origin: None,
        };
        app.set_repos(
            Some("studio"),
            &[
                repo("/Users/me/Code/new-thing", 9),
                repo("/Users/me/Code/old", 1),
            ],
        );
        assert_eq!(app.current_project().name, "webapp");
        let remote: Vec<&str> = app
            .projects_on(Some("studio"))
            .iter()
            .map(|&i| app.projects[i].name.as_str())
            .collect();
        // Configured projects stay; discovered ones are ordered by recent activity.
        assert_eq!(remote, ["new-thing", "old", "api", "data", "No project"]);
    }

    #[test]
    fn live_models_take_their_effort_from_the_arrow_keys() {
        let model = Model {
            harness: Harness::Codex,
            id: "gpt-6.1-sol".into(),
            name: "GPT-6.1 Sol".into(),
            efforts: vec!["low".into(), "high".into(), "xhigh".into()],
            default_effort: Some("low".into()),
        };
        let mut app = App::new(
            config(),
            State::default(),
            None,
            vec![],
            &HashMap::new(),
            &[model],
        )
        .unwrap();
        assert_eq!(app.current_agent().id, "codex:gpt-6.1-sol");
        assert_eq!(app.effort(0), Some("low"));

        app.toggle(Overlay::Models);
        app.cycle_effort(1);
        assert_eq!(app.effort(0), Some("high"));
        app.cycle_effort(5);
        assert_eq!(app.effort(0), Some("xhigh"), "stops at the last level");

        app.close_overlay();
        // A project on another host needs no local folder.
        app.choose_host(host_index(&app, "studio"));
        app.editor.insert("tidy the readme");
        let plan = app.plan().unwrap();
        assert_eq!(&plan.agent[..3], ["codex", "-m", "gpt-6.1-sol"]);
        assert!(
            plan.agent
                .windows(2)
                .any(|w| w == ["-c", "model_reasoning_effort=xhigh"])
        );
        assert_eq!(plan.agent.last().unwrap(), "tidy the readme");
    }
}
