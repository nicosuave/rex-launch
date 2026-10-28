# rex-launch

A composer popup for [Rex](https://www.superlogical.com/rex/docs). Write a
prompt, pick a project and an agent, and press Return: rex-launch opens a tab
in the project's Rex session that creates a git worktree with
[worktrunk](https://worktrunk.dev) and starts the agent in it with your prompt.

## Install

You need [Rex](https://www.superlogical.com/rex/docs), and on every machine
you launch tasks on: `git`, [worktrunk](https://worktrunk.dev) (`wt`), and
[Claude Code](https://docs.anthropic.com/en/docs/claude-code) or
[Codex](https://github.com/openai/codex).

```sh
brew install nicosuave/tap/rex-launch    # or: cargo install --git https://github.com/nicosuave/rex-launch
rex-launch setup
```

The Homebrew formula also installs worktrunk.

`rex-launch setup` copies [`lua/rex-launch.lua`](lua/rex-launch.lua) beside
Rex's `init.lua`, adds this line to `init.lua` once, and reloads Rex:

```lua
require("rex-launch").setup{ bin = "/Users/you/.cargo/bin/rex-launch" }
```

Then press **Cmd+Shift+N**, or run **rex-launch: new task** from the command
palette.
`setup` also takes `key` (a different shortcut, or `false` for none) and
`columns`/`rows` (the popup's size in cells). Run `rex-launch setup` again
after upgrading to refresh the module.

To have Rex switch to the new tab on its own, turn on **Remote Control** in
Rex's settings under Rex Server. Without it the tab is still created.

## How it works

1. The `new_task` action opens `rex-launch` in a floating layer over the
   current tab, sized from the focused pane.
2. On submit, rex-launch finds the Rex session labeled with the project's name
   (creating it if needed) on the project's host, and opens a tab named after
   the branch.
3. The tab fetches the remote's default branch, runs
   `wt switch --create BRANCH --base origin/HEAD-target --no-cd`, then starts
   the agent in the new worktree. When the agent exits, a shell stays open
   there.
4. rex-launch asks the Rex app to show the new tab and exits, which closes the
   popup.

Because the tab's command runs on whichever server hosts it, projects on
another machine (`host = "studio"`) work the same way, as long as `git`,
`wt`, and the agent are installed there.

## Projects and models

Projects are found, not listed: every git repository up to two levels deep in
the code folders (`~/Code/editor`, `~/Code/work/api`), on this computer and on
each Rex host from `rex hosts`. Worktrees are skipped (their `.git` is a file),
and clones of the same remote become one project. The list is ordered by what
you launched last, then by recent git activity. This computer is scanned every
time the popup opens (about 15 ms for 130 repositories); other hosts start
from their last scan and are rescanned in the background through a
short-lived Rex session there (`rex-launch --scan HOST`). Every connection
also has **No project**, which starts the agent in the home directory without
a worktree.

Models come from the agents themselves: Claude Code's Agent SDK `initialize`
request and the Codex app server's `model/list`. The last answer is cached and
refreshed in the background each time.

`~/.config/rex-launch/config.toml` is optional:

```toml
code_folders = ["~/Code"]   # the default
exclude = ["_archive"]      # folder names to skip

# Projects outside the code folders:
[[project]]
name = "dotfiles"
path = "/Users/me/dotfiles"
host = "studio"        # a label from `rex hosts`; leave out for this computer

# Extra agents, added after the ones the agents report. The prompt is appended
# to `command`.
[[agent]]
id = "my-claude"
harness = "Claude"
name = "Opus (plan mode)"
command = ["claude", "--model", "opus", "--permission-mode", "plan"]
```

State lives in `~/.local/state/rex-launch/`: the last project, model, effort,
and access mode (`state.json`), each connection's last scan (`scans/`), and
the agents' models (`models.json`).

## Keys

| Key | Does |
| --- | --- |
| Return | Start |
| Shift+Return, Alt+Return, Ctrl+J | New line |
| Ctrl+P | Pick a project |
| Ctrl+R | Pick a connection |
| Ctrl+O | Pick a model; Left and Right change the highlighted model's effort |
| Ctrl+S | Cycle the access mode |
| Ctrl+B | Edit the branch name (generated from the prompt until you do) |
| Esc | Close the menu, or the popup |

Chips and menu rows are also clickable.

## Notes

- In terminals with kitty graphics (Rex, Ghostty, kitty), card corners, chip
  ends, and the send button are small anti-aliased images shown through
  Unicode placeholders; text stays ordinary cells. Elsewhere, or with
  `--no-graphics`, they're quadrant and half-block characters.
- Rex floating layers are transparent wherever a cell keeps the terminal's
  default background, so every cell the UI draws sets its own background.
- rex-launch waits until Rex has shown its pane (the pane has a pixel size)
  before drawing; popups that drew earlier showed up cropped.
- The prompt is passed to the agent as a single argument and never through
  `wt -x`, which would expand `{{ }}` in it.
- `rex-launch --dry-run` prints what it would launch instead of launching.
