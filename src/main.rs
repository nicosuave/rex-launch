//! rex-launch: a composer popup for Rex that turns a prompt into a git
//! worktree and an agent running in a new tab.

mod app;
mod chrome;
mod config;
mod discover;
mod editor;
mod fuzzy;
mod launch;
mod models;
mod setup;
mod slug;
mod theme;
mod ui;

use std::collections::HashMap;
use std::io::stdout;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use ratatui::DefaultTerminal;
use ratatui::crossterm::cursor::SetCursorStyle;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{supports_keyboard_enhancement, window_size};

use app::{Action, App, Status};
use chrome::{CellSize, Chrome};
use config::{Config, ModelCache, Scan, State};
use models::{Harness, Model};

/// Background work finishing.
enum Update {
    /// A connection's scan was saved.
    Scanned(String),
    /// An agent reported its models.
    Models(Harness, anyhow::Result<Vec<Model>>),
}

const USAGE: &str = "usage: rex-launch setup
       rex-launch [--client CLIENT] [--config PATH] [--dry-run] [--no-graphics]
       rex-launch --scan HOST [--config PATH]

  setup            install the Rex action (Cmd+Shift+N) that opens rex-launch in a popup

  --client CLIENT  the Rex app to show the new tab in (the popup's action passes it)
  --config PATH    read code folders and agents from PATH
  --dry-run        print what would be launched instead of launching it
  --no-graphics    draw corners with block characters, not kitty graphics
  --scan HOST      look for projects on the Rex host HOST and save what's found";

struct Options {
    client: Option<String>,
    config: Option<PathBuf>,
    dry_run: bool,
    graphics: bool,
    scan: Option<String>,
}

fn parse_args() -> Result<Options> {
    let mut options = Options {
        client: None,
        config: None,
        dry_run: false,
        graphics: true,
        scan: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--client" => options.client = args.next().filter(|c| !c.is_empty()),
            "--config" => options.config = args.next().map(PathBuf::from),
            "--dry-run" => options.dry_run = true,
            "--no-graphics" => options.graphics = false,
            "--scan" => options.scan = args.next(),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("rex-launch {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => bail!("unknown argument {other:?}\n\n{USAGE}"),
        }
    }
    Ok(options)
}

fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("setup") {
        return setup::run();
    }
    let options = parse_args()?;
    let config = Config::load(options.config.as_deref())?;
    if let Some(host) = options.scan {
        let repos = discover::scan_remote(&host, &config.code_folders, &config.exclude)?;
        return Scan {
            scanned_at: unix_now(),
            repos,
        }
        .save(Some(&host));
    }

    // This computer is scanned before drawing; it only reads directories.
    // Other connections start from their last scan and are rescanned in the
    // background, replacing it when the scan finishes.
    let mut repos = HashMap::new();
    repos.insert(
        String::new(),
        discover::scan_local(&config.code_folders, &config.exclude),
    );
    let rex_hosts = launch::hosts();
    let configured = config.projects.iter().filter_map(|p| p.host.clone());
    let remotes: Vec<String> = rex_hosts.iter().cloned().chain(configured).collect();
    for host in &remotes {
        repos.insert(
            host.clone(),
            Scan::load(Some(host)).map(|s| s.repos).unwrap_or_default(),
        );
    }
    let mut model_cache = ModelCache::load();
    let mut app = App::new(
        config,
        State::load(),
        options.client,
        rex_hosts,
        &repos,
        &model_cache.models,
    )?;
    let (updates, received) = mpsc::channel();
    start_scans(&mut app, remotes, options.config.as_deref(), &updates);
    start_model_fetches(&mut app, &updates);

    wait_until_shown();
    let mut terminal = ratatui::init();
    // Detection reads stdin directly, so it has to run before crossterm reads any input.
    if options.graphics {
        app.chrome = chrome::detect(Duration::from_millis(300))
            .ok()
            .flatten()
            .map(Chrome::new);
    }
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    // Disambiguated keys let Shift+Enter add a newline instead of submitting.
    if enhanced {
        execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    execute!(
        stdout(),
        EnableMouseCapture,
        EnableBracketedPaste,
        SetCursorStyle::SteadyBar
    )?;

    let result = run(
        &mut terminal,
        &mut app,
        options.dry_run,
        &received,
        &mut model_cache,
    );

    if let Some(chrome) = app.chrome.as_mut() {
        let _ = chrome.delete_all(&mut stdout());
    }
    let _ = execute!(
        stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        SetCursorStyle::DefaultUserShape
    );
    if enhanced {
        let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    }
    ratatui::restore();

    if let Some(summary) = result? {
        println!("{summary}");
    }
    Ok(())
}

/// Rex starts a terminal at 80x24 with no pixel size, and resizes it when an
/// app first shows the pane. Popups that drew before then showed up cropped in
/// the Rex app, so wait until the pane has a pixel size, or give up after a
/// while (for example when no app is showing it).
fn wait_until_shown() {
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        if window_size().is_ok_and(|size| size.width > 0) {
            return;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Scans each host in its own detached `rex-launch --scan` process, which
/// finishes and cleans up its temporary Rex session even if the popup closes
/// first. `Update::Scanned` arrives once a host's scan is saved.
fn start_scans(
    app: &mut App,
    hosts: Vec<String>,
    config: Option<&std::path::Path>,
    updates: &Sender<Update>,
) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    for host in hosts {
        let mut command = Command::new(&exe);
        command.args(["--scan", &host]);
        if let Some(config) = config {
            command.arg("--config").arg(config);
        }
        // Its own process group, so closing the popup's terminal doesn't stop it.
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let Ok(mut child) = command.spawn() else {
            continue;
        };
        app.scanning.push(Some(host.clone()));
        let updates = updates.clone();
        std::thread::spawn(move || {
            let _ = child.wait();
            let _ = updates.send(Update::Scanned(host));
        });
    }
}

/// Asks Claude Code and Codex for their models, each on its own thread.
fn start_model_fetches(app: &mut App, updates: &Sender<Update>) {
    for harness in [Harness::Claude, Harness::Codex] {
        app.fetching.push(harness);
        let updates = updates.clone();
        std::thread::spawn(move || {
            let _ = updates.send(Update::Models(harness, models::fetch(harness)));
        });
    }
}

/// Applies finished background work to the popup.
fn apply(app: &mut App, update: Update, model_cache: &mut ModelCache) {
    match update {
        Update::Scanned(host) => match Scan::load(Some(&host)) {
            Some(scan) => app.set_repos(Some(&host), &scan.repos),
            // A failed scan leaves the last one's projects in place.
            None => app.scanning.retain(|h| h.as_deref() != Some(host.as_str())),
        },
        Update::Models(harness, Ok(models)) if !models.is_empty() => {
            model_cache.replace(harness, models);
            let _ = model_cache.save();
            app.set_models(harness, &model_cache.models);
        }
        // An agent that couldn't be asked keeps its last known models.
        Update::Models(harness, _) => app.fetching.retain(|h| *h != harness),
    }
}

/// Draws a frame, then sends any images it introduced.
fn render(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    terminal.draw(|f| ui::draw(f, app))?;
    if let Some(chrome) = app.chrome.as_mut() {
        chrome.flush(terminal.backend_mut())?;
    }
    Ok(())
}

/// Re-renders the images for the new cell size. They're resent even when the
/// size is unchanged, because an app that just started showing the popup
/// doesn't get images that were sent before it attached.
fn resized(app: &mut App, columns: u16, rows: u16) {
    let Some(chrome) = app.chrome.as_mut() else {
        return;
    };
    if let Ok(size) = window_size()
        && size.width > 0
        && columns > 0
        && rows > 0
    {
        chrome.set_cell_size(CellSize {
            width: size.width / columns,
            height: size.height / rows,
        });
    }
    chrome.retransmit();
}

/// Runs until the popup is closed or a launch succeeds. Returns the launch
/// summary for `--dry-run`.
fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    dry_run: bool,
    updates: &Receiver<Update>,
    model_cache: &mut ModelCache,
) -> Result<Option<String>> {
    loop {
        for update in updates.try_iter() {
            apply(app, update, model_cache);
        }
        render(terminal, app)?;
        if app.done {
            return Ok(None);
        }
        // While background work runs, wake up regularly to show its results.
        let waiting = !app.scanning.is_empty() || !app.fetching.is_empty();
        if waiting && !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let action = match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => app.on_key(key),
            Event::Paste(text) => {
                app.on_paste(&text);
                None
            }
            Event::Mouse(mouse) => app.on_mouse(mouse),
            Event::Resize(columns, rows) => {
                resized(app, columns, rows);
                None
            }
            _ => None,
        };
        if let Some(Action::Submit) = action
            && let Some(summary) = submit(terminal, app, dry_run)?
        {
            return Ok(Some(summary));
        }
    }
}

fn submit(terminal: &mut DefaultTerminal, app: &mut App, dry_run: bool) -> Result<Option<String>> {
    let plan = match app.plan() {
        Ok(plan) => plan,
        Err(message) => {
            app.status = Status::Error(message);
            return Ok(None);
        }
    };
    if dry_run {
        app.done = true;
        return Ok(Some(plan.describe()));
    }

    let place = match &plan.host {
        Some(host) => format!("{} on {host}", plan.session),
        None => plan.session.clone(),
    };
    app.status = Status::Busy(format!("Starting {} in {place}", plan.branch));
    render(terminal, app)?;

    let opened = match plan.open() {
        Ok(opened) => opened,
        Err(err) => {
            app.status = Status::Error(format!("{err:#}"));
            return Ok(None);
        }
    };
    app.save_state();
    app.done = true;

    // Switching the app to the new tab is a convenience; the tab exists either way.
    let shown = match app.client.as_deref() {
        Some(client) => launch::show(client, &opened),
        None => Ok(()),
    };
    if let Err(err) = shown {
        let hint = if err.to_string().contains("not accepting actions") {
            "turn on Remote Control in Rex Settings to jump there".to_string()
        } else {
            format!("couldn't switch to it: {err:#}")
        };
        app.status = Status::Notice(format!("Opened {} in {place}; {hint}", plan.branch));
        render(terminal, app)?;
        // Leave the notice up briefly; any key dismisses it sooner.
        if event::poll(Duration::from_secs(4))? {
            let _ = event::read()?;
        }
    }
    Ok(None)
}
