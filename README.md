# muxbox

A macOS app for Claude Code sessions that live in tmux on a remote host: a sidebar of sessions next to a built-in terminal (`ssh -t host tmux attach`). Sessions keep running when your laptop sleeps.

## Install

```sh
cargo install --path .   # puts muxbox in ~/.cargo/bin
```

Building needs Xcode (GPUI compiles Metal shaders). The host needs `tmux` and `git`.

## Config

`~/.config/muxbox/config.toml` (or `$MUXBOX_CONFIG`):

```toml
# theme = "system"                # or "light" / "dark"; must come before the tables

[[hosts]]
name = "agent"
ssh = "agent"                     # user@host or a ~/.ssh/config alias; "local" runs here
# upload_dir = "~/.muxbox/uploads"
# tmux_socket = "work"            # tmux -L; default server if unset
# claude = "claude"               # command typed into new sessions

[[bases]]
host = "agent"
path = "~/projects/api"
# name = "api"                    # defaults to the last path component
# worktree_dir = ".claude/worktrees"
```

With the default `worktree_dir`, new worktree threads run `claude -w <name>` and Claude creates the worktree. Any other `worktree_dir` (relative to the repo, absolute, or `~/...`) makes muxbox run `git worktree add -b worktree-<name>` there first.

## Use

Run `muxbox`. The sidebar lists each base's worktrees and their sessions; it refreshes every 3 s.

- Click a session to show it. Each opened session keeps its own terminal, so switching is instant. muxbox reopens the last session on launch.
- **New worktree** starts a thread in a new worktree; **+** on a worktree starts another thread in it. The field that appears in the header suggests a free random name like `calm-heron`: press Enter to take it, or type your own.
- **×** on a session, clicked twice, kills it. The worktree stays.
- Double-click a session, or right-click → **Rename…**, to rename it. Only the thread's tmux name changes; Claude keeps running and its worktree and branch keep their names.
- `⌘B` or the header button collapses the sidebar. `⌘Q` quits; sessions keep running.
- A terminal whose connection dropped (laptop slept) says so; click it to reconnect.

**Images:** drop files on the terminal, or `⌘V` with an image or copied files on the clipboard. muxbox uploads them to the host's `upload_dir` and pastes the remote paths. Text pastes as text.

**Scrolling and selection** belong to tmux: `set -g mouse on` on the host lets you scroll with the wheel and select with the mouse. tmux sends copied text to the terminal (OSC 52), and muxbox puts it on your Mac clipboard.

CLI: `muxbox ls`, `muxbox new <base> <name> [--worktree <path>]`, `muxbox attach <host> <session>`, `muxbox rename <host> <session> <name>`, `muxbox upload <files>...`, `muxbox paste` (clipboard image to the last session muxbox showed).

### Status dots

The sidebar shows working / needs input / idle from a `@muxbox_state` tmux option. Set it with Claude Code hooks in `~/.claude/settings.json` on the host:

```json
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "[ -n \"$TMUX_PANE\" ] && tmux set-option -t \"$TMUX_PANE\" @muxbox_state working || true" }] }],
    "PostToolUse": [{ "hooks": [{ "type": "command", "command": "[ -n \"$TMUX_PANE\" ] && tmux set-option -t \"$TMUX_PANE\" @muxbox_state working || true" }] }],
    "Notification": [{ "hooks": [{ "type": "command", "command": "[ -n \"$TMUX_PANE\" ] && tmux set-option -t \"$TMUX_PANE\" @muxbox_state needs_input || true" }] }],
    "Stop": [{ "hooks": [{ "type": "command", "command": "[ -n \"$TMUX_PANE\" ] && tmux set-option -t \"$TMUX_PANE\" @muxbox_state idle || true" }] }]
  }
}
```
