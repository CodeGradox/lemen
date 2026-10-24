# lemur

A macOS app for Claude Code sessions that live in tmux on a remote host: a sidebar of sessions next to a built-in terminal (`ssh -t host tmux attach`). Sessions keep running when your laptop sleeps.

[![lemur in use: Claude threads by project and worktree, answering one that waits, ⌘J for a new shell, ⌃Tab between threads, a pasted screenshot, and notes](docs/demo.avif)](docs/demo.avif)

## Install

Download `lemur-macos-arm64.dmg` from the [latest release](https://github.com/CodeGradox/lemur/releases/latest), open it and drag lemur into Applications. It's signed and notarized, and needs Apple Silicon and macOS 11 or newer.

lemur was called lemen up to 1.0.0. Coming from lemen, move `~/.config/lemen` to `~/.config/lemur` (lemur won't start without its config) and `~/.lemen` to `~/.lemur` (your notes). lemur still reads the tags lemen put on tmux sessions, and `lemen.app` can go once lemur runs.

From source:

```sh
sh scripts/build-app.sh  # build/lemur.app; drag it into Applications
cargo install --path .   # the lemur command, for `lemur ls`, `lemur paste` and the rest
```

Building needs Xcode (GPUI compiles Metal shaders). The host needs `tmux` and `git`. [docs/releases.md](docs/releases.md) covers the disk image and signed releases.

## Config

`~/.config/lemur/config.toml` (or `$LEMUR_CONFIG`):

```toml
# theme = "system"                # or "light" / "dark"; must come before the tables
# sidebar_width = 255             # pixels; dragging the sidebar's edge sets it
# notes_width = 360               # pixels; dragging the notes panel's edge sets it

# [terminal]
# font_family = "Menlo"
# font_size = 13                  # points, 6-72

# [terminal.dark]                 # colors while lemur is dark, as WezTerm names them;
# background = "#282c34"          # what's left out is lemur's own (One Half Dark)
# foreground = "#dcdfe4"
# cursor = "#979db4"
# ansi = ["#282c34", "#e17079", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#dcdfe4"]
# brights = ["#8c939f", "#e17079", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#ffffff"]
# [terminal.light]                # the same while lemur is light (One Half Light)

[[hosts]]
name = "agent"
ssh = "agent"                     # user@host or a ~/.ssh/config alias; "local" runs here
# upload_dir = "~/.lemur/uploads"
# tmux_socket = "work"            # tmux -L; default server if unset
# claude = "claude"               # command typed into new sessions

[[bases]]
host = "agent"
path = "~/projects/api"
# name = "api"                    # defaults to the last path component
# worktree_dir = ".claude/worktrees"
# collapsed = true                # folded in the sidebar
# worktree_order = [...]          # worktree paths, in the order they were dragged into
# worktree_colors = { "/path/to/worktree" = "blue" }  # red orange yellow green teal cyan blue purple pink gray
```

Settings (`⌘,`) writes `theme`, `[terminal]`, `[terminal.dark]` and `[terminal.light]` for you, dragging a side panel's edge writes `sidebar_width` or `notes_width`, and the sidebar writes a project's `collapsed`, `worktree_order` and `worktree_colors`, and the order of the `[[bases]]`; the rest of the file, comments included, stays as it is.

New worktrees go in `worktree_dir` (relative to the repo, absolute, or `~/...`): lemur runs `git worktree add` there, then starts Claude in it.

## Use

Run `lemur`. The sidebar lists each base's worktrees and their sessions, in the config's order whatever their host, then each host's sessions outside them (*other*); it refreshes every 3 s. A worktree shows as its branch (`worktree-` dropped from the branches `claude -w` and older lemurs made), or its folder's name when detached.

- **+** at the top of the sidebar adds a folder, as in Zed: pick a host (yours from the config, an alias from `~/.ssh/config`, or This Mac; or type `user@host`), then type the path while its folders are suggested (↑↓ to choose, Tab to go into one, Enter to add). A git repository shows each of its worktrees; a folder without git is a single row, with no **New worktree**. A folder inside a repository adds the repository. It's saved to the config as a `[[bases]]` entry, with a `[[hosts]]` entry for a new host.
- Click a session to show it. Each opened session keeps its own terminal, so switching is instant. lemur reopens the last session on launch.
- **New worktree** starts a thread in a new worktree; **+** on a worktree, or `⌘J` for the worktree of the thread on show, starts another session in it: **Claude**, or a plain **Shell**, picked in the spotlight with ↑↓ or a click. A spotlight asks for a label (`claude` or `shell` to start with, following the pick until you type one; a free random name for a new worktree): press Enter to take it, or type your own; Esc or a click outside cancels. For a new worktree it also asks for a **Branch** to create, empty for a detached worktree, and what to start **From**, picked from the host's branches; that starts at the default branch (`origin/HEAD`, like `origin/main`). A remote's branch is fetched first; if that fails, the worktree starts where it was last fetched, and the header says so. Several sessions can share a label: lemur names the tmux session `<label>--<random words>`, like `claude--calm-heron`, and shows only the label. Claude sessions get Claude's spark as their icon, shells a terminal.
- **×** on a session, clicked twice, kills it. The worktree stays.
- Right-click a worktree → **Remove worktree…** to take it out of git; its branch stays. If threads are running in it, the dialog names them: **Close and remove** closes them first, and **Cancel** keeps them and the worktree. lemur checks the host too, so a thread the sidebar hasn't shown yet, or one whose terminal has `cd`'d into the worktree, is asked about the same way. Its folder stays on the host as plain files unless you tick **Also delete**, which deletes it, read-only parts included. One that's locked (Claude Code locks the worktree its session runs in) asks again before **Remove anyway**, which unlocks it; so does deleting one with uncommitted changes, which drops them. These asks all come before any thread closes, so cancelling one keeps the threads running. The main checkout and plain folders can't be removed this way.
- Click a project's name to fold it away, or open it again.
- Right-click a worktree → **Color** to give its folder icon one of ten colors.
- Drag a worktree onto another in the same project to move it there.
- Drag a project's name onto another's to move it there, whatever their hosts; the config's `[[bases]]` take the new order, each entry with its comments.
- A project's **⋯** (on hover) or right-click → **Remove project…** takes it out of lemur and the config. Nothing on the host changes, and its running threads move to *other*.
- Double-click a session, or right-click → **Rename…**, to change its label. Only the thread's tmux name changes (its random words stay); Claude keeps running and its worktree and branch keep their names.
- Drag a thread onto another in the same worktree to move it to that place. The order is stored on the tmux sessions (`@lemur_order`), so it survives restarts and is the same from any Mac; new threads start at the bottom.
- `⌃Tab` shows the worktree's next thread: the one you opened before this one. Hold `⌃` and press `Tab` again to go further back, as `⌘Tab` does with apps; letting go of `⌃` stays there. `⌃⇧Tab` goes the other way.
- `⌘B` or the header button collapses the sidebar. `⌘Q` quits; sessions keep running.
- The menu bar has them too: **File** → **Add Project…** (`⌘O`), **New Thread in Worktree…** (`⌘J`) and **Refresh**; **View** → **Toggle Sidebar** (`⌘B`), **Toggle Notes** (`⌘R`, as Zed's right dock), and **Next** and **Previous Thread in Worktree** (`⌃Tab`, `⌃⇧Tab`).
- `⌘,` (**lemur → Settings…**) opens Settings: the theme (System, Light or Dark), the terminal's font and size, and its colors for a dark and a light lemur (One Half Dark and Light to start). Changes apply at once and are saved to the config. **Shortcuts** lists every shortcut.
- A terminal whose connection dropped (laptop slept) says so; click it to reconnect.

**Notes:** every worktree has a notepad. `⌘R` or the button at the right of the header opens it beside the terminal, on the worktree of the thread you opened last; a worktree's notes icon opens that one's. Type straight into it: it's Markdown, highlighted as you go. `⌘`-click a URL to open it; holding `⌘` underlines it first. Pasting a URL over selected text makes it a link, `[text](url)`, and so does `⌘K`, with the URL on the clipboard; `⌘K` in a link selects its URL. The two buttons at the top of the notes, or `⇧⌘L` for the checklist, make the cursor's line, or the selected lines, a bullet list or a checklist, and plain lines again when they are one already. `- [ ] ` starts an item with a box: `⌘`-click the box, or press `⌘↩` or `⇧⌘U` on its line, to tick it, and again to untick it; ticked items fade and are struck through. Enter at the end of a list item starts the next, and Enter on an empty one ends the list; Tab and `⇧Tab` indent and outdent an item, and `⌃⌘↑` and `⌃⌘↓` move it, with its nested items. These are Apple Notes' keys. Notes are saved as you type, on this Mac, in `~/.lemur/notes/<host>/<worktree path>.md`.

**Images:** drop files on the terminal, or `⌘V` with an image or copied files on the clipboard. lemur uploads them to the host's `upload_dir` and pastes the remote paths. Text pastes as text.

**Links:** hold `⌘` over a link in the terminal to underline it, and `⌘`-click to open it in your browser; plain clicks still go to tmux. That's an `http(s)://` URL written out in the text, or a link whose text hides its URL (OSC 8). tmux only passes those on to terminals it knows support them; to have it pass them to lemur, add this to `~/.tmux.conf` on the host (tmux 3.4 or newer):

```
set -as terminal-features ',xterm*:hyperlinks'
```

A URL runs on across rows the terminal wrapped, but not across lines Claude Code broke itself, which a long one in its replies may be.

**Scrolling and selection** belong to tmux: `set -g mouse on` on the host lets you scroll with the wheel and select with the mouse. tmux sends copied text to the terminal (OSC 52), and lemur puts it on your Mac clipboard.

CLI: `lemur ls`, `lemur new <base> <name> [--branch <branch>] [--from <branch>]` (a new worktree; detached without `--branch`) or `lemur new <base> <name> --worktree <path> [--shell]`, `lemur attach <host> <session>`, `lemur rename <host> <session> <name>`, `lemur upload <files>...`, `lemur paste` (clipboard image to the last session lemur showed).

### Status dots

Each session's dot comes from Claude Code itself: while a Claude runs on the host, each refresh runs `claude agents --json` there, which lists each running Claude with its process and status, and lemur matches the process to its tmux session. No setup needed; it needs Claude Code 2.1.139 or later (when `claude agents` arrived), on `PATH` or in `~/.local/bin`.

- **Green, breathing:** Claude is working.
- **Yellow:** Claude needs input (a question or permission prompt); hover the dot for what it's asking.
- **Blue:** it finished while you were looking at another session; opening it clears that.
- **Grey:** idle, or no Claude running.

### Notifications

When a session finishes or starts needing input, a popup appears at the top-right of the main screen, like Zed's, unless it's the session you're looking at in the active window. **Show** brings lemur forward and opens that session; **Dismiss** closes the popup. Opening the session from the sidebar closes its popup too.

The refresh runs every 3 seconds while lemur is in front, and every 15 seconds while it's in the background; a turn shorter than that can go unnoticed.

For older Claude Code without `claude agents`, hooks can report the same states through a `@lemur_state` tmux option (hooks written for lemen's `@lemen_state` keep working). Put this in `~/.claude/settings.json` on the host:

```json
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "[ -n \"$TMUX_PANE\" ] && tmux set-option -t \"$TMUX_PANE\" @lemur_state working || true" }] }],
    "PostToolUse": [{ "hooks": [{ "type": "command", "command": "[ -n \"$TMUX_PANE\" ] && tmux set-option -t \"$TMUX_PANE\" @lemur_state working || true" }] }],
    "Notification": [{ "hooks": [{ "type": "command", "command": "[ -n \"$TMUX_PANE\" ] && tmux set-option -t \"$TMUX_PANE\" @lemur_state needs_input || true" }] }],
    "Stop": [{ "hooks": [{ "type": "command", "command": "[ -n \"$TMUX_PANE\" ] && tmux set-option -t \"$TMUX_PANE\" @lemur_state idle || true" }] }]
  }
}
```

## Trademarks

lemur is an independent project, not affiliated with or endorsed by Anthropic. Claude, Claude Code and the Claude logo are trademarks of Anthropic.
