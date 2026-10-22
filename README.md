# lemen

A macOS app for Claude Code sessions that live in tmux on a remote host: a sidebar of sessions next to a built-in terminal (`ssh -t host tmux attach`). Sessions keep running when your laptop sleeps.

## Install

Download `lemen-macos-arm64.dmg` from the [latest release](https://github.com/CodeGradox/lemen/releases/latest), or the [newest build of main](https://github.com/CodeGradox/lemen/releases/download/latest-main/lemen-macos-arm64.dmg), open it and drag lemen into Applications. Both are signed and notarized; they need Apple Silicon and macOS 11 or newer.

From source:

```sh
sh scripts/build-app.sh  # build/lemen.app; drag it into Applications
cargo install --path .   # the lemen command, for `lemen ls`, `lemen paste` and the rest
```

Building needs Xcode (GPUI compiles Metal shaders). The host needs `tmux` and `git`. [docs/releases.md](docs/releases.md) covers the disk image and signed releases.

## Config

`~/.config/lemen/config.toml` (or `$LEMEN_CONFIG`):

```toml
# theme = "system"                # or "light" / "dark"; must come before the tables

# [terminal]
# font_family = "Menlo"
# font_size = 13                  # points, 6-72

[[hosts]]
name = "agent"
ssh = "agent"                     # user@host or a ~/.ssh/config alias; "local" runs here
# upload_dir = "~/.lemen/uploads"
# tmux_socket = "work"            # tmux -L; default server if unset
# claude = "claude"               # command typed into new sessions

[[bases]]
host = "agent"
path = "~/projects/api"
# name = "api"                    # defaults to the last path component
# worktree_dir = ".claude/worktrees"
```

Settings (`⌘,`) writes `theme` and `[terminal]` for you and leaves the rest of the file, comments included, as it is.

With the default `worktree_dir`, new worktree threads run `claude -w <name>` and Claude creates the worktree. Any other `worktree_dir` (relative to the repo, absolute, or `~/...`) makes lemen run `git worktree add -b worktree-<name>` there first.

## Use

Run `lemen`. The sidebar lists each base's worktrees and their sessions; it refreshes every 3 s. A worktree shows as its branch (`worktree-` dropped from the branches lemen and `claude -w` make), or its commit when detached.

- **+** at the top of the sidebar adds a folder, as in Zed: pick a host (yours from the config, an alias from `~/.ssh/config`, or This Mac; or type `user@host`), then type the path while its folders are suggested (↑↓ to choose, Tab to go into one, Enter to add). A git repository shows each of its worktrees; a folder without git is a single row, with no **New worktree**. A folder inside a repository adds the repository. It's saved to the config as a `[[bases]]` entry, with a `[[hosts]]` entry for a new host.
- Click a session to show it. Each opened session keeps its own terminal, so switching is instant. lemen reopens the last session on launch.
- **New worktree** starts a thread in a new worktree; **+** on a worktree starts another session in it: **Claude**, or a plain **Shell**. The field that appears in the header asks for a label (`claude` or `shell` to start with; a free random name for a new worktree): press Enter to take it, or type your own. Several sessions can share a label: lemen names the tmux session `<label>--<random words>`, like `claude--calm-heron`, and shows only the label. Claude sessions get Claude's spark as their icon, shells a terminal.
- **×** on a session, clicked twice, kills it. The worktree stays.
- Double-click a session, or right-click → **Rename…**, to change its label. Only the thread's tmux name changes (its random words stay); Claude keeps running and its worktree and branch keep their names.
- Drag a thread onto another in the same worktree to move it to that place. The order is stored on the tmux sessions (`@lemen_order`), so it survives restarts and is the same from any Mac; new threads start at the bottom.
- `⌘B` or the header button collapses the sidebar. `⌘Q` quits; sessions keep running.
- `⌘,` (**lemen → Settings…**) opens Settings: the theme (System, Light or Dark) and the terminal's font and size. Changes apply at once and are saved to the config.
- A terminal whose connection dropped (laptop slept) says so; click it to reconnect.

**Notes:** every worktree has a notepad. The button at the right of the header opens it beside the terminal, on the worktree of the thread you opened last; a worktree's notes icon opens that one's. It shows Markdown, and links (pasted bare URLs too) open with a click. **Edit** opens the Markdown for editing; Esc or **Done** shows it rendered again. Notes are saved as you type, on this Mac, in `~/.lemen/notes/<host>/<worktree path>.md`.

**Images:** drop files on the terminal, or `⌘V` with an image or copied files on the clipboard. lemen uploads them to the host's `upload_dir` and pastes the remote paths. Text pastes as text.

**Scrolling and selection** belong to tmux: `set -g mouse on` on the host lets you scroll with the wheel and select with the mouse. tmux sends copied text to the terminal (OSC 52), and lemen puts it on your Mac clipboard.

CLI: `lemen ls`, `lemen new <base> <name> [--worktree <path> [--shell]]`, `lemen attach <host> <session>`, `lemen rename <host> <session> <name>`, `lemen upload <files>...`, `lemen paste` (clipboard image to the last session lemen showed).

### Status dots

Each session's dot comes from Claude Code itself: every refresh runs `claude agents --json` on the host, which lists each running Claude with its process and status, and lemen matches the process to its tmux session. No setup needed; it needs Claude Code 2.1.139 or later (when `claude agents` arrived), on `PATH` or in `~/.local/bin`.

- **Green, breathing:** Claude is working.
- **Yellow:** Claude needs input (a question or permission prompt); hover the dot for what it's asking.
- **Blue:** it finished while you were looking at another session; opening it clears that.
- **Grey:** idle, or no Claude running.

### Notifications

When a session finishes or starts needing input, a popup appears at the top-right of the main screen, like Zed's, unless it's the session you're looking at in the active window. **Show** brings lemen forward and opens that session; **Dismiss** closes the popup. Opening the session from the sidebar closes its popup too.

The refresh runs every 3 seconds, so a turn shorter than that can go unnoticed.

For older Claude Code without `claude agents`, hooks can report the same states through a `@lemen_state` tmux option. Put this in `~/.claude/settings.json` on the host:

```json
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "[ -n \"$TMUX_PANE\" ] && tmux set-option -t \"$TMUX_PANE\" @lemen_state working || true" }] }],
    "PostToolUse": [{ "hooks": [{ "type": "command", "command": "[ -n \"$TMUX_PANE\" ] && tmux set-option -t \"$TMUX_PANE\" @lemen_state working || true" }] }],
    "Notification": [{ "hooks": [{ "type": "command", "command": "[ -n \"$TMUX_PANE\" ] && tmux set-option -t \"$TMUX_PANE\" @lemen_state needs_input || true" }] }],
    "Stop": [{ "hooks": [{ "type": "command", "command": "[ -n \"$TMUX_PANE\" ] && tmux set-option -t \"$TMUX_PANE\" @lemen_state idle || true" }] }]
  }
}
```

## Trademarks

lemen is an independent project, not affiliated with or endorsed by Anthropic. Claude, Claude Code and the Claude logo are trademarks of Anthropic.
