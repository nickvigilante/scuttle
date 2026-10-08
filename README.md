# scuttle

scuttle is an unofficial, full-screen terminal client for Coder Agents, written in Rust on Ratatui.
It is a personal project, and it is not built, supported, or endorsed by Coder.

## Quickstart

You need:

- A Coder deployment with Coder Agents enabled, and an account on it.
- The [`coder` CLI](https://coder.com/docs/install/cli), logged in to that deployment.
- A Rust toolchain from [rustup](https://rustup.rs), on a recent stable release.
- Optionally, a [Nerd Font](https://www.nerdfonts.com/) in your terminal, for icons.

1. Log in with the Coder CLI, if you have not already:

   ```sh
   coder login https://coder.example.com
   ```

1. Install scuttle from this repository:

   ```sh
   cargo install --git https://github.com/nickvigilante/scuttle --locked scuttle-tui
   ```

   This builds the `scuttle` binary into `~/.cargo/bin`, which rustup adds to your `PATH`.
   The first build takes a minute or two.

1. Run it:

   ```sh
   scuttle
   ```

   To open a chat you already have, pass its ID: `scuttle <chat-id>`.

1. If your terminal uses a Nerd Font, tell scuttle so it draws icons, by adding this to your shell profile:

   ```sh
   export NERD_FONT=1
   ```

   You can also set `icons = "nerd"` in `~/.config/scuttle/config.toml`, which `/settings` opens.

Type `/help` inside scuttle for every command and key.
To update, run the `cargo install` command again with `--force`.
To remove it, run `cargo uninstall scuttle-tui`.

## Building and running

To build from a clone instead, run this in the repository:

```sh
cargo run --release
```

scuttle reuses the session the `coder` CLI stored at `coder login`, so log in with the CLI first.

| Item | Behavior |
|---|---|
| Arguments | `scuttle [chat-id]` opens that chat. A non-UUID argument prints the usage line and exits 2. |
| Auth sources | `CODER_URL` and `CODER_SESSION_TOKEN` win when set. Otherwise the CLI's stored URL and session (keychain, then the session file in `CODER_CONFIG_DIR` or the default coderv2 directory). |
| Rejected session | Prints "the session token was rejected" with the `coder login <url>` command and exits. |
| `NERD_FONT` | `1`, `true`, or `yes` selects Nerd Font icons. `0`, `false`, or `no` selects text. `icons` in config.toml overrides it. With neither set, icons are text. |
| Version skew | A notice appears when the server version differs from the one the SDK was generated from. |
| `SCUTTLE_NO_TERMINAL_QUERY` | When set, skips the terminal query at startup. |

`/help` in the app is the live reference for commands and keys.

scuttle turns alternate scroll mode (mode 1007) on when it exits, because most terminals start with it on.
In a terminal that starts with it off, such as xterm, the mouse wheel then sends arrow keys to full-screen programs like `less` until you turn it off with `printf '\033[?1007l'`.

## Slash commands

Type `/` for the slash menu.
Personal and workspace skills follow the commands in the menu, and a personal skill named like a command shows as `/<username>:<name>`.
After `/workspace ` or `/ws `, the menu lists the organization's workspaces, most recently used first, and `none`; Tab completes the highlighted name.

| Command | What it does | Arguments |
|---|---|---|
| `/new` | Start a new chat. The current one keeps running. | none |
| `/chats` (`/resume`) | Find and open a chat or subagent. Same as Ctrl+R. | `[query]` |
| `/subagents` | Watch this chat's subagents live and open one. | none |
| `/parent` (`/back`) | Return from a subagent to its parent. | none |
| `/model` | Pick the model for the next message, and set when each model compacts. | `[name]` |
| `/effort` | Pick reasoning effort with a slider. | `[level]` |
| `/workspace` (`/ws`) | Attach or detach a workspace. | `[name\|none]` |
| `/organization` (`/org`) | Choose the organization new chats go to. | `[name]` |
| `/plan-mode` | Toggle plan mode, or set it. | `[on\|off]` |
| `/implement` | Leave plan mode and implement the proposed plan. | none |
| `/title` | Rename this chat, or edit a proposed title. | `[text]` |
| `/queue` | Run a queued message next, or remove it. | none |
| `/files` | List this chat's files, save them to disk, or view text files. | none |
| `/attach` | Attach a file to the next message. `@path` does the same. | `<path>` |
| `/info` (`/chat-info`) | Chat details, context, and cost. | none |
| `/git` | Branch, pull request, and local changes. | none |
| `/diff` | Show the chat's diff in your git pager. | none |
| `/mcp` | List MCP servers and turn them on or off. | none |
| `/usage` | AI spend, workspace quota, chat cost, context. | none |
| `/statusline` | Choose the footer's fields, order, and warnings. | none |
| `/compact` | Summarize the conversation to free context. | none |
| `/clear` | Reset the model context and keep the transcript. | none |
| `/copy` | Copy the last message, or its nth code block. | `[n]` |
| `/web` | Open the chat in the Coder web UI. | none |
| `/mouse` | Toggle mouse capture. | none |
| `/settings` | Edit config.toml in `$EDITOR`. Applied when you save and quit. | none |
| `/help` | Show every command and key. | none |
| `/quit` (`/exit`) | Exit. | none |

The diff pager is `$GIT_PAGER`, then `core.pager`, then `$PAGER`, then `less -R`.

## Keys

### Composer

| Key | Action |
|---|---|
| Enter | Send, or add a line, per your Coder send-key preference. |
| Shift+Enter, Alt+Enter, Ctrl+J | Add a line. |
| Esc | Interrupt the agent, or close the slash menu. |
| Esc in an idle subagent | Return to the parent, when nothing is typed. |
| Ctrl+R | Open /chats. |
| Ctrl+G | Edit the message in `$EDITOR`. |
| Ctrl+O | Copy everything typed in the composer. |
| Up, Down | Recall sent messages. |
| Home, End, Ctrl+A, Ctrl+E | Start or end of the current line. Cmd+Left and Cmd+Right too, where the terminal reports Super. |
| Ctrl+Home, Ctrl+End | Start or end of the whole message. |
| Tab | Complete a slash command or skill. After `@`, complete a file path to attach. |
| Slash menu: Up, Down, Tab | Move through commands. Tab completes the highlighted one. |
| Slash menu: send key | Run the highlighted command, or complete one that needs an argument. After a space, send the text. |
| Backspace on an empty composer | Remove the last attachment. |
| Send key on an empty composer | Send the first queued message now, interrupting a running turn. |
| Ctrl+Enter on an empty composer | Implement a proposed plan. Without keyboard enhancement, use `/implement`. |
| Tab on an empty composer | Show the questions Esc hid. |
| Up, Down, Enter, Left, Esc | Answer a plan-mode question while the composer is empty. Left goes back, Esc hides them. |
| Left, Right | Move the /effort slider. Enter saves, Esc cancels. |
| Ctrl+C twice | Quit. |

In Zellij, the default keymap takes Ctrl+O and Ctrl+G before scuttle sees them.
Sending on an unavailable model holds the message and opens /model.
Enter there sends it with the model you pick, and Esc puts it back in the composer.

### Transcript

| Key | Action |
|---|---|
| PageUp, PageDown | Scroll. PageUp at the top loads older messages. |
| Wheel | Scroll while mouse capture is on. At the top it loads older messages. |
| End | Jump to the latest message. In a draft, press End twice. |
| Click | Open a link, expand a tool call or thinking, copy a code block, or save an attached file. |
| Pointer over a link | Underlines and brightens it, with mouse capture on. |

### Selection and copy

| Action | How |
|---|---|
| Select | Drag. The text copies on release. |
| Code block | Click it, or `/copy n`. |
| Last message | `/copy`. |
| Composer text | Ctrl+O. |
| Where it goes | The native clipboard locally, OSC 52 over SSH, and tmux passthrough. OSC 52 copies cap at 100,000 bytes. |

### Paste snippets

A paste of enough lines or characters (the web UI's thresholds) becomes a token such as `[Pasted text #1 +120 lines]` or `[Pasted text #2 +1500 chars]`.
It is sent as a text file named `pasted-text-YYYY-MM-DD-HH-MM-SS.txt`.
Tab on the token expands it into the composer.
Pasting the same text again right away also expands it, and Backspace after that removes it.

## Overlays

Esc or Ctrl+C closes an overlay.
Wheel moves like Up and Down in tables.

### /chats

| Feature | Behavior |
|---|---|
| Filters | Tab cycles All, Active, Unread, Archived. |
| Typing | Filters loaded chats by title. The last row, "Search all chats", asks the server. |
| Server search operators | `status:`, `archived:`, `has_unread:`, `pr_status:`, `pr:`, `pr_title:`, `title:`, `repo:`, `source:created_by_me`, `source:shared_with_me`, `diff_url:`. Quote values with spaces. |
| Subagents | Right shows and Left hides a chat's subagents. They are indented under the parent, and `+N` counts them. |
| Columns | Pin, status, title, family count, archived marker, age. From 100 columns, a summary column. From 120 columns, a pull request column with its state. |
| Status | Spinner while working, error, waiting on you, then unread. Nerd Font icons replace the text and emoji. |
| Pin (Ctrl+P) | Pins or unpins. The marker comes from `chats.pin_icon`. |
| Rename (Ctrl+E) | Renames the chat. |
| Read (Ctrl+U) | Marks read or unread. |
| Archive (Ctrl+A) | Opens a confirmation. See below. A child chat or a running family is refused. Archived chats unarchive at once. |
| Paging | End, or moving to the last row, loads more. |
| Live updates | A "live updates paused" note shows when the watch socket is down. |

Archive confirmation: choose Archive, or Archive and delete workspace (when the chat has one).
The delete choice ends with "(can't be undone)" in the error color, and a narrow box drops words from the row before it cuts the flag.
Enter on the delete choice only arms it.
Pressing `D` then confirms, so a held key never deletes.
A second Ctrl+A archives without deleting.
Esc or Cancel closes the box and leaves /chats open.
The box keeps its place and size as you move between the choices.
After an archive, the highlight moves to the chat above the archived one, or to the first chat when none is above it.

### /model

/model fills the transcript area, as /chats does.
Models are grouped under their provider, and a provider that cannot be used says why on its row while its models are dimmed.
Each model shows `current` or `default`, its context window, its compaction threshold, and its reasoning efforts.
When the terminal is too narrow for every column, the reasoning efforts give way first, then the context window, and then the threshold's note, so `70% (default)` reads `70%*`.
The model's name keeps its room.
The compaction threshold is how full the context window gets before the server summarizes the chat's history.
A threshold you set applies to your chats on that model, in scuttle and in the web UI.
Without one, the model's own threshold applies and shows `(default)`.
At 100% a chat never compacts, and at 0% it compacts after every turn.

| Key | Action |
|---|---|
| Typing, Backspace | Filter the models. |
| Enter | Use the highlighted model for the next message. |
| Left, Right | Move the highlighted model's threshold by 5%, within 0% and 100%. |
| Delete | Go back to the model's default threshold. |

A change saves when you move to another model or close /model, so holding Right sends one request.
If the server refuses a change, scuttle says why and shows the value the server holds again.

### Other overlays

| Overlay | What it shows | Keys |
|---|---|---|
| /usage | AI spend, budget, workspace quota, chat cost, context. | Up and Down scroll. |
| /info | ID, parent, organization, owner, model and effort, plan mode, workspace, created and updated times, context, cost, warnings. | Up and Down scroll. |
| /statusline | Every footer field with shown or hidden state, and warning thresholds. | Space or Enter toggles. `[` and `]`, Alt+Up and Alt+Down, or Shift+Up and Shift+Down move. Left and Right set the warning on context, spend, and quota. |
| /mcp | MCP servers for this chat, or for a new chat's first message. | Enter or Space turns an organization server on or off for the next message. |
| /subagents | Live list with a preview below. | Up and Down preview, Enter opens, PageUp and PageDown scroll the preview. |
| /git | Branch, pull request, local changes. | Enter on Open pull request or View diff. |
| /workspace | Workspace details. | Enter on Copy SSH command, Open in web, Detach, or Switch workspace. |
| /queue | Queued messages. | Enter sends now, Delete or Backspace removes. |
| /files | The chat's files, newest first, with type, size, sender, message, and age. | Enter saves to `files.save_dir`, `s` saves as, `v` views text in scuttle's pager, `g` goes to the file's message. |
| /workspace picker, /organization | Tables with type-to-filter. | Enter chooses. |
| /effort | Slider. | Left and Right, Enter saves, Esc cancels. |
| /help | Commands, keys, /chats markers, search operators. | Scroll. |

### Saving files

`/files` saves to `files.save_dir`, and clicking an attached file's line in the transcript saves it there too.
When the name is already taken, a box asks what to do, and only a letter answers it:

| Key | Action |
|---|---|
| `k` | Keep both: save as `name (1)`, or the next free number. |
| `r` | Replace the file there. It can't be undone. |
| `c`, Esc, or Ctrl+C | Cancel, and save nothing. |

Enter, Up, and Down do nothing in the box, so a held Enter that asked the question can't also answer it.
While the box shows, keys and pastes go to it rather than to what it covers, a save-as line included.

## Footer and status line

The footer is one line of fields joined by ` · `, plus a status.
`/statusline` edits it, and the choice is saved to config.toml.

| Field | Shows |
|---|---|
| `model` | The chat's model. |
| `effort` | Reasoning effort, when the model has levels. |
| `context` | Context window used, of the model's limit. |
| `workspace` | The attached workspace. |
| `organization` | The open chat's organization, when you have several. |
| `plan-mode` | Plan mode, while on. |
| `spend` | AI spend this period, of your budget, with when it resets. |
| `status` | The chat's status and the connection. |
| `cost` | This chat's cost, for its whole tree. |
| `quota` | Workspace credits used, of your quota. |
| `queue` | Messages waiting. |
| `mcp` | MCP servers the next message uses. |

The default is the first eight, in that order, from `model` through `status`.
A field with nothing to show stays hidden.
`context`, `spend`, and `quota` can warn at a percent of their limit (1 to 100), even when the footer hides the field.

When the line is too wide, whole fields drop in this order:
the spend reset text first, then `workspace`, `organization`, `queue`, `mcp`, `cost`, `quota`, `effort`, `spend`, `context`, `model`, `plan-mode`.
A field that is warning, or an unavailable model, drops only after every field that is not.
`status` never drops, and is only cut when nothing else is left.

## Config

Path: `$XDG_CONFIG_HOME/scuttle/config.toml`, else `~/.config/scuttle/config.toml`.
`/settings` creates it from a commented template if missing, and opens it in `$EDITOR`.
A missing file means all defaults.

| Key | Default | Notes |
|---|---|---|
| `mouse` | `true` | Mouse capture. `/mouse` toggles and saves it. |
| `busy_behavior` | `"queue"` | Sending while the agent works: `"queue"` or `"interrupt"`. |
| `composer_max_lines` | `10` | The most rows the composer grows to. |
| `spinner` | `"random"` | Or `"braille"`, `"line"`, `"arc"`, `"bounce"`, `"bar"`. |
| `icons` | unset | `"nerd"` or `"text"`. Unset follows `NERD_FONT`, then text. |
| `statusline.fields` | model, effort, context, workspace, organization, plan-mode, spend, status | Repeats keep their first place. |
| `statusline.thresholds.context` | unset | Percent, 1 to 100. Unset means no warning. |
| `statusline.thresholds.spend` | unset | Same. |
| `statusline.thresholds.quota` | unset | Same. |
| `chats.pin_icon` | unset | Any text, `""` for none. Unset gives the pin emoji, or the Octicons pin with Nerd icons. |
| `files.save_dir` | unset | Where `/files` saves; `~/` is your home directory. Unset gives `~/Downloads` when it exists, else your home directory. |
| `welcome.show` | `true` | Welcome screen on a blank chat. |
| `welcome.art_file` | unset | Text file whose lines replace the Coder wordmark. |
| `welcome.art_color` | `"accent"` | `"accent"` or `"plain"`. How `welcome.art_file`'s art is drawn: in the brand accent like the wordmark, or in the normal text color. The built-in wordmark is always the accent, and `NO_COLOR` draws everything plain. |
| `density.<tool>` | unset | How much of a tool's output shows: `"expanded"`, `"summary"`, or `"hidden"`. |
| `organization` | unset | Saved by `/organization`. A non-UUID value is ignored. |
| `efforts.<model-uuid>` | unset | Effort per model, saved by `/effort`. Invalid entries are dropped. |

Changes to `welcome.*`, `organization`, and `efforts` need a restart.
Other keys apply when you save and quit the editor.
A key that looks like a secret (token, secret, password, api_key, or a suffix like `_key`) is refused.
A parse error names the line.

## Local state

Path: `$XDG_STATE_HOME/scuttle/state.toml` (absolute paths only), else `~/.local/state/scuttle/state.toml`.
Today it holds one key, `nerd_font_tip_shown`, which stops the welcome screen suggesting a Nerd Font again.
scuttle writes it, and you do not edit it.
A missing or damaged file just means the tip shows again.

## Safety and privacy

| Topic | Behavior |
|---|---|
| Token | Read from the environment, keychain, or the CLI's session file. Held in memory as a secret. Never written to config or state, and config refuses secret-looking keys. |
| Archive and delete | Needs Enter to arm and `D` to confirm, so a held key never deletes. Nothing else deletes a workspace. |
| Config and state writes | Written atomically, readable only by you (mode 0600), through symlinks to their target. A read-only config is left alone and the save reports it. |
| Saved files | Never replaces a file unless you press `r` at the save question. Names are cut to a plain file name, and files over 10 MiB are refused. The download goes to a hidden `.scuttle-<uuid>.part` file beside the target and takes its name only once it is whole; a hard kill can leave that file behind. A saved file gets mode 0644 before your umask, unlike config and state. scuttle never opens or runs a file it saved. |
| Temp files | Ctrl+G writes `scuttle-<uuid>.md` in the temp dir (0600) and removes it afterward. |
| Clipboard | Copies go to the system clipboard, or through the terminal over SSH. |

## Trademarks

Coder, the Coder logo, and the Codernaut are trademarks of Coder Technologies, Inc.
scuttle is an unofficial project and is not affiliated with, endorsed by, or supported by Coder Technologies, Inc.

## License

MIT. See [LICENSE](LICENSE).

scuttle depends on [unofficial-coder-sdk-rs](https://github.com/nickvigilante/unofficial-coder-sdk-rs), which is licensed under the AGPL-3.0, so a built `scuttle` binary includes AGPL-3.0 code.
Because it links the AGPL-3.0 SDK, a built `scuttle` binary must be distributed under the AGPL-3.0 as a whole, and anyone who distributes one must meet the AGPL-3.0's terms, including making the source of the whole work available.
Two dependencies, [nucleo-matcher](https://crates.io/crates/nucleo-matcher) and [progenitor-client](https://crates.io/crates/progenitor-client), are licensed under the MPL-2.0.
