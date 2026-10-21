# muxbox

A sidebar for Claude Code sessions that live in tmux on a remote host. It runs in a WezTerm split; the other split shows the selected session (`ssh -t host tmux attach`). Sessions keep running when your laptop sleeps.

## Install

```sh
cargo install --path .   # puts muxbox in ~/.cargo/bin
```

Needs `tmux` and `git` on the host, WezTerm locally.

## Config

`~/.config/muxbox/config.toml` (or `$MUXBOX_CONFIG`):

```toml
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

Run `muxbox` in a WezTerm pane.

| Key | Does |
| --- | --- |
| `j` / `k` | Move |
| `Enter` | Show the session in the Claude pane |
| `n` | New thread in a new worktree of the selected base |
| `t` | New thread in the selected worktree |
| `x` | Kill the session (the worktree stays) |
| `Tab` | Focus the Claude pane |
| `r` | Refresh now (it also refreshes every 3 s) |
| `q` | Quit (closes the Claude pane; sessions keep running) |

Drop files on the sidebar to upload them and paste their remote paths into the session in the Claude pane. Outside WezTerm, `Enter` attaches in the same terminal until you detach.

CLI: `muxbox ls`, `muxbox new <base> <name> [--worktree <path>]`, `muxbox attach <host> <session>`, `muxbox upload <files>...`, `muxbox paste`.

### Paste images with a key

`muxbox paste` uploads the clipboard image to the session muxbox is showing. Bind it in `~/.wezterm.lua`:

```lua
config.keys = {
  { key = 'V', mods = 'CMD|SHIFT',
    action = wezterm.action_callback(function(window, pane)
      -- full path: WezTerm's PATH may not include ~/.cargo/bin
      local ok, out, err = wezterm.run_child_process({ os.getenv('HOME') .. '/.cargo/bin/muxbox', 'paste' })
      window:toast_notification('muxbox', ok and out or err, nil, 3000)
    end) },
}
```

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
