# tessel's window, as data

Everything in tessel's window is a tree of components described in one file:

    ~/.tessel/ui.json        (or $TESSEL_HOME/ui.json)

tessel watches the file. Save it and the window rebuilds within a second, keeping
what is open, what is typed, and where things are scrolled. If the file has a
mistake, the window keeps the last layout that worked and shows the error,
with where it is in the file and what would have been accepted. Delete the file
to go back to the default layout.

After every load tessel writes what it found to `~/.tessel/ui/check.txt`: `ok`, or
the error, plus any warnings and any values it moved onto its scales. **If you are an
agent editing the file, read `check.txt` after each save and fix what it says.**
`tessel --check-ui FILE` prints the same thing without opening a window.

This reference is rewritten to `~/.tessel/ui/README.md` every time tessel starts,
next to the shipped presets in `~/.tessel/ui/presets/`. They always match the
running build. Read a preset before writing a layout from scratch: it is the
quickest way to see how the pieces fit.

## The file

```json
{
  "theme": { "base": "orchard", "appearance": "dark" },
  "root": { "type": "row", "children": [ ... ] }
}
```

| key      | what it is                                                                    |
| -------- | ----------------------------------------------------------------------------- |
| `preset` | start from a shipped layout: `default`                                        |
| `theme`  | colours and fonts (below). layered over the preset's theme                    |
| `root`   | the component tree. replaces the preset's tree entirely when given            |
| `freestyle` | `true` turns off every guardrail below: drawn exactly as written           |
| `transcript` | the transcript's options (`user`, `thoughts`, `tools`), wherever the layout put it |
| `sessions` | the session list's options (`trace`)                                          |
| `desktops` | a list of desktops, each `{"id", "name", "layout", "scope", "open"}`: its own panes (`layout`, shaped like a `panes` node's), the projects the sidebar lists on it (`scope`, empty for all), and the chat it had open. everything else (the sidebar, the theme, the options) is shared |
| `desktop` | the id of the desktop showing (or its number, from 1) |
| `notch` | the agents drawn around the camera notch. on where there is one; `false` hides it, `true` shows it on a Mac without one, mid menu bar |

A file with only a `"theme"` restyles the default layout. A file with `"root"` is your
own layout. `transcript` and `sessions` set the
options people change most without writing a layout: `{"transcript": {"user": "left"}}`. The settings page (⌘,) writes this file too.

## The default layout

A sidebar of sessions in a drawer, and a `panes` workspace: the conversation, and
beside it the workspace (changes, files, checks, artifacts, info as tabs), folded
away until ⌥⌘I or the button in the conversation's header opens it. A file with no
`"root"` (only a theme, say) uses it. The shipped layout is in `presets/default.json`.

## Panes

A `panes` node is a tiled workspace: a split tree of panes, each showing one module.
People rearrange it directly (every pane's header splits it right or down, swaps
its module, or closes it, and the gaps between panes can be dragged), and every
change is written back into this file. When the file changes, from a click or from
you, panes glide from where they were to where they now belong, new panes grow in
and closed panes fade out. So rearranging a window by editing this file animates.

```json
{ "type": "panes", "grow": true, "h": "full", "p": 8,
  "layout": { "split": "row", "children": [
    { "module": "conversation", "size": 3 },
    { "split": "column", "size": 2, "children": [
      { "module": "changes" },
      { "id": "tree", "module": "files", "open": false }
    ]}
  ]}}
```

A layout is a **pane** or a **split**.

- A split: `"split"` is `"row"` (side by side) or `"column"` (stacked), `"children"`
  is its panes and splits, `"size"` its share of the split it is in (default 1).
- A pane: `"module"` (below; leave it out for an empty pane that offers modules),
  `"size"` its share, `"open": false` to start it folded away (it takes no room
  until `toggle:<id>` opens it), `"id"` to name it for buttons and conditions (`open:<id>`),
  `"pip": "tl"|"tr"|"bl"|"br"` to float it in that corner over the others as a picture
  in picture (its split gives its room to the rest; drag its grip to another corner).
  Panes without an id are known by their module name. The conversation pane also
  takes the transcript's options: `user`, `thoughts`, `tools`, `width`, `gutter`;
  the sessions pane takes `trace`.

Modules:

| module         | shows                                                                  |
| -------------- | ---------------------------------------------------------------------- |
| `conversation` | the session: its transcript, what is waiting on you, the message box (and the new-session form when none is open). one per window |
| `sessions`     | every session, the ones that need you first. one per window           |
| `workspace`    | changes, files, checks, artifacts and info as tabs. one per window    |
| `changes`      | what changed in the session's folder, with diffs                      |
| `files`        | the session's folder, with an editor                                  |
| `checks`       | the branch, its pull request and CI                                   |
| `artifacts`    | plans and files the agent made                                        |
| `info`         | the session: model, cost, tool servers, linked folders                |
| `terminal`     | a shell in the session's folder, run by the daemon on Ghostty's core: it outlives the window, and scripts can read and drive it (`tesselctl term`) |
| `browser`      | the open chat's page, in a browser the daemon runs: the agent drives it with its tools while you watch, you can click, scroll and type in it too, and Pick puts a part of the page in your next message |
| `git`          | every branch's history as a graph; stage and unstage files (or lines, from a diff), commit with a message you write or one written for you, undo what tessel did |
| `surfaces`     | live views the agent drew for a pane (`tessel_surface`, region `pane`). while one is open those leave the chat |

Panes are tiles on the frame: 8 points apart, each a sheet with orchard's panel corner.

## Nodes

Every node is an object with a `"type"`. Any node can also have:

- `id`: a name for it. Drawers need one. Buttons use it as their element id.
- `when`: a condition, or a list of them that must all hold, for the node to show (below).
- any of the style keys (below).

Only `row`, `column` and `drawer` take `"children"`.

### Arranging

| type      | what it does                                                                                     | its own keys                              |
| --------- | ------------------------------------------------------------------------------------------------ | ----------------------------------------- |
| `row`     | lays children out left to right, centred vertically                                              | `children`                                |
| `column`  | lays children out top to bottom, stretched to its width                                          | `children`                                |
| `drawer`  | a panel at the side that opens and closes with a spring, resizable by dragging its edge, thrown shut if dragged most of the way closed | `children`, `side`, `width`, `min`, `max`, `open` |
| `spacer`  | takes up the room left in a row or column                                                        |                                           |
| `divider` | a hairline across a column, or down a row                                                        |                                           |

Drawer keys: `side` is `"left"` or `"right"` (default left), `width` its width when open
(points), `min` and `max` the range dragging may set, `open` whether it starts open.
The window remembers a drawer's open state and width across reloads of the file,
as long as its `id` stays the same. ⌥⌘S toggles the drawer called `sidebar` and
⌥⌘I the one called `inspector`; any drawer can be toggled by a button
(`"action": "toggle:<id>"`).

### Small things

| type         | what it shows                                                                       | its own keys                                          |
| ------------ | ----------------------------------------------------------------------------------- | ----------------------------------------------------- |
| `text`       | fixed text                                                                          | `text`                                                |
| `button`     | a button that does an action (below)                                                | `action`, `icon`, `label`, `tooltip`, `variant`, `size` |
| `title`      | the open session's name, or "New Session"                                           |                                                       |
| `status`     | what the open session is doing ("Working · running cargo test · 2m"); nothing when idle and new |                                          |
| `connection` | a line saying the daemon can't be reached; nothing while all is well                |                                                       |
| `error`      | the last thing that went wrong (a file too big to attach, say); nothing otherwise   |                                                       |
| `add`        | the + menu: open a project folder, open a GitHub project, quick start a new project from a template, or a recent folder |              |
| `desktops`   | the desktops by name: click to go there, double-click to rename, + for another. a layout without one gets one beside its `settings` button |              |

Buttons: `icon` is any [lucide](https://lucide.dev/icons) icon name in kebab case
(`panel-left`, `arrow-up`, `paperclip`, `sparkles`, `square`, `layout-grid`, `git-branch`,
`settings`, ...). `label` is text beside it. A button needs at least one of the two.
`tooltip` defaults to a description of the action. `variant`: `ghost` (default),
`primary` (filled with the accent; one per view), `outline`, `text`. `size`: `xsmall`,
`small`, `medium` (default).

### The session

| type          | what it shows                                                                                        | its own keys                   |
| ------------- | ---------------------------------------------------------------------------------------------------- | ------------------------------ |
| `sessions`    | every session, the ones that need you first. scrolls, and fills the height it is given                | `trace`                        |
| `new-session` | the form for starting a session: folder, in place or in a new worktree, what to do                    |                                |
| `transcript`  | the conversation. scrolls, keeps to the bottom while the agent writes. give it room with `"grow": true` | `user`, `thoughts`, `tools`  |
| `prompts`     | permission requests, questions and plans waiting on you, as cards that slide open; nothing otherwise |                                |

`sessions`: `trace` (default true) draws the little activity graph on each row.

`transcript`: it fills the pane it is given (so its scrollbar sits at the pane's edge)
and sets each row in a reading column `width` points wide (default 720), with `gutter`
points either side of the text (default 16). `user` is `"left"` (default) or `"right"`: which side your messages sit
on. `thoughts` is `"shown"` (default; folded to two lines once finished) or `"hidden"`.
`tools` is `"shown"` (default), `"failed"` (only tool calls that failed) or `"hidden"`.

Only one `sessions`, `new-session`, `transcript`, `input` and `panel` may appear in a layout.

### The message box

The message box is in pieces so it can be arranged any way: put them in a
`column` with a `bg`, `border` and `radius` and it is a card; leave those off and
it is a strip.

| type           | what it shows                                                                         | its own keys |
| -------------- | ------------------------------------------------------------------------------------- | ------------ |
| `input`        | where you type. Return sends, ⇧Return is a new line. Paste or drop images and PDFs on the window to attach them | `bare` |
| `attachments`  | the files that will go with the next message, each removable; nothing when there are none |          |
| `model-picker` | which model answers, as a menu                                                         |              |
| `mode-picker`  | how much the agent may do unasked (Ask First, Accept Edits, Auto, Plan), as a menu     |              |
| `context`      | how full the agent's context is, as a ring and a percentage. turns `warning` at 80%    |              |

`input`: `bare: true` drops its own border and fill, for when the node around it is the box.

### Beside the session

| type    | what it shows                                                                                   | its own keys |
| ------- | ----------------------------------------------------------------------------------------------- | ------------ |
| `panel` | the workspace: tabs for the open session's changes (with diffs), files (with an editor), checks (branch, pull request, CI), artifacts (plans and files it made) and info | `tabs` |
| `panes` | a tiled workspace of panes, each showing a module (above) | `layout` |

`panel`: `tabs` is the list of tabs to show, in order, from `changes`, `files`,
`checks`, `artifacts`, `info` (default: all five). The panel only asks the daemon
for anything while it is visible.

## Actions

What a `button` does:

| action            | does                                                          |
| ----------------- | ------------------------------------------------------------- |
| `toggle:<id>`     | opens or closes the drawer or pane with that id               |
| `new-session`     | shows the new-session form (⌘N)                               |
| `overview`        | every session as a grid of cards (⇧⌘\\)                       |
| `stop`            | stops the agent (⌘.)                                          |
| `send`            | sends what is typed, with any attachments                     |
| `attach`          | picks images or PDFs to send with the next message            |
| `next`            | goes to the session that has waited on you longest (⌘J)       |
| `allow`           | allows the oldest permission request (⌘Return)                |
| `focus`           | closes every drawer, or opens the sidebar again (⇧⌘F)         |
| `customize`       | Settings, at Customize. to change the window, ask any session: every agent has tessel's tools |
| `settings`        | the settings page (⌘,)                                        |

## Conditions

`"when": "busy"` shows a node only while the agent works. `"when": ["session", "!busy"]`
needs both. A `!` in front negates.

| condition       | holds when                                                         |
| --------------- | ------------------------------------------------------------------ |
| `session`       | a session is open (not the new-session form)                       |
| `busy`          | the open session's agent is working                                |
| `pending`       | the agent is waiting on you: a permission, a question or a plan    |
| `planning`      | the session is in plan mode                                        |
| `attachments`   | files are waiting to go with the next message                      |
| `connected`     | the daemon is reachable                                            |
| `dark`          | the window is dark                                                 |
| `focused`       | the window is in front                                             |
| `open:<id>`     | that drawer or pane is open                                        |

## Style keys

Any node can have these. Numbers are points. Space comes from a 4 point grid:
use 4, 8, 12, 16, 24, 32, 48, 64. A container's padding should be at least 1.5×
the gap between its children.

| key            | value                                                                                       |
| -------------- | ------------------------------------------------------------------------------------------- |
| `w`, `h`       | points, `"full"` or `"auto"`                                                                |
| `min_w`, `max_w`, `min_h`, `max_h` | points                                                                  |
| `grow`         | `true` takes the room left over in its row or column                                        |
| `shrink`       | `false` never gets squeezed                                                                 |
| `p`, `px`, `py`, `pt`, `pr`, `pb`, `pl` | padding: all sides, horizontal, vertical, then one side each       |
| `gap`          | space between children                                                                      |
| `align`        | how children sit across the direction of flow: `start`, `center`, `end`, `stretch`         |
| `justify`      | how children sit along it: `start`, `center`, `end`, `between`                              |
| `bg`           | a colour (below)                                                                            |
| `color`        | the text colour                                                                             |
| `hover`        | the background while the pointer is over it                                                 |
| `border`       | `true` for all sides, a side (`"top"`, `"right"`, `"bottom"`, `"left"`) or a list of them. hairline |
| `border_color` | a colour. default `border`                                                                  |
| `radius`       | points or a name: `none` 0, `tile` 8, `control` 10, `card` 14, `panel` 18, `widget` 22, `full` |
| `font`         | a text style or a size in points. styles: `title` 15/20 semibold, `headline` 13/16 semibold, `body` 13/16, `callout` 12/15, `subheadline` 11/14, `prose` 14/21, `code` 12/17 mono |
| `weight`       | `regular` 450, `medium` 550, `semibold` 650, `bold` 700, or a number                        |
| `opacity`      | 0 to 1                                                                                      |
| `clip`         | `true` hides what spills over its edges                                                     |
| `scroll`       | `true` scrolls its children vertically                                                      |
| `drag`         | `true` makes it title bar: dragging it moves the window, double-clicking zooms              |
| `inset`        | room at the leading edge for the window's close/minimise/zoom buttons: `true` always, or a drawer id for only while that drawer is closed |
| `shadow`       | `true` lifts it off what is behind it                                                       |
| `wrap`         | `true` lets a row's children wrap onto more lines                                           |

The window's close/minimise/zoom buttons are drawn by macOS at the top left,
about 52 points down from the top. Whatever is at the top left of the layout needs
`"inset"` (or a drawer over it) so they don't sit on top of your buttons, and the
top 52 points want a node with `"drag": true` so the window can be moved.

## Colours

A colour is a role, a role with an opacity (`"accent/20"` is the accent at 20%),
`"transparent"`, or a hex value (`#rgb`, `#rrggbb`, `#rrggbbaa`). Prefer roles: they
follow light and dark, and the theme.

| role        | for                                                             |
| ----------- | --------------------------------------------------------------- |
| `app`       | the frame around the content: the sidebar                       |
| `sheet`     | the content                                                     |
| `raised`    | surfaces floating above the content: cards, the message box     |
| `sunken`    | quiet fills set into the content: your messages, fields         |
| `hover`     | under the pointer                                               |
| `active`    | the selected row                                                |
| `text`      | text                                                            |
| `muted`     | secondary text                                                  |
| `faint`     | the quietest readable text                                      |
| `ghost`     | ink too faint to read: drag handles, empty marks                |
| `border`    | hairlines                                                       |
| `accent`    | the one accent: selection, focus, the primary button            |
| `on-accent` | text on the accent                                              |
| `warning`   | blocked on you                                                  |
| `error`     | failed                                                          |
| `success`   | finished and not yet seen                                       |

Colour means something. `warning`, `error` and `success` are for those states only,
and never the only way a state is shown. Everything else is neutral, with one accent.

## Theme

```json
"theme": {
  "base": "orchard",
  "appearance": "system",
  "font": "Inter",
  "mono": "JetBrains Mono",
  "light": { "accent": "#df3047", "app": "#f7eeee" },
  "dark":  { "accent": "#eb4b58" }
}
```

- `base`: the palette to start from. `tessel` (default): apple's system colours.
  `orchard`: Hack Club Orchard's default palette, a warm tinted neutral with a red accent.
- `appearance`: `system` (default) follows macOS, or always `light` or `dark`.
- `size`: how big the interface is drawn: `compact`, `default` or `large`, or a number
  from 0.75 to 1.5. Text, menus and controls scale with it.
- `message_size`: the message box's text size, in points, 12 to 24 (default 16).
- `font`: the interface font, by family name. `mono`: the font for code and commands.
  Use fonts that are installed; an unknown family falls back to the system font.
- `light`, `dark`: any of the colour roles above, set for that appearance.

`"theme": "orchard"` on its own is short for `{"base": "orchard"}`.

## Examples

Restyle the default layout with orchard's palette, always dark:

```json
{ "theme": { "base": "orchard", "appearance": "dark" } }
```

A message box as a floating card:

```json
{
  "type": "column", "bg": "raised", "border": true, "radius": "panel", "shadow": true, "p": 12, "gap": 8,
  "children": [
    { "type": "attachments" },
    { "type": "input", "bare": true },
    { "type": "row", "gap": 4, "children": [
      { "type": "model-picker" }, { "type": "mode-picker" }, { "type": "spacer" },
      { "type": "context" },
      { "type": "button", "action": "send", "icon": "arrow-up", "variant": "primary" }
    ]}
  ]
}
```

A Stop button that only exists while the agent works:

```json
{ "type": "button", "action": "stop", "icon": "square", "tooltip": "Stop", "when": "busy" }
```

## Scripting: if you can click it, you can script it

Every command the window takes is in one table. The palette (⌘K), the keys, the menus,
`tesselctl ui` and agents' tools all run the same entries, so anything you can click
you can also run from a script or ask an agent to do.

```
tesselctl ui ls                         what the window shows: panes, drawers, theme
tesselctl ui commands                   this table
tesselctl ui pane.split dir=down        any command, with key=value arguments
tesselctl ui show changes @focused      shorthands: split, close, zoom, show, focus, theme
tesselctl ui events --filter layout_changed,pane_focused
```

Panes are named by id, `@focused`, or the module they show. Sessions by `@open`, an id
prefix, or part of their name. Commands go through the daemon, so they work the same
from a terminal, an agent, or (later) another machine. Events a window publishes:
`command_run`, `layout_changed`, `session_opened`, `pane_focused`.

| command | what it does | keys | arguments |
| --- | --- | --- | --- |
| `app.palette` | Command palette | ⌘ K |  |
| `app.settings` | Settings | ⌘ , | section?: appearance, themes, models… opens there |
| `app.overview` | All sessions | ⇧ ⌘ \\ |  |
| `app.sidebar` | Show or hide the sidebar | ⌥ ⌘ S |  |
| `app.workspace` | Show or hide the workspace | ⌥ ⌘ I |  |
| `app.focus` | Focus on this session | ⇧ ⌘ F |  |
| `session.new` | New session | ⌘ N | cwd?, prompt?, worktree?: with a prompt it starts at once |
| `session.next` | Next session that needs you | ⌘ J |  |
| `session.stop` | Stop the agent | ⌘ . | session? |
| `session.allow` | Allow | ⌘ ↩ | session? |
| `composer.attach` | Attach files… | ⌘ U | opens the file picker; what you choose goes with the next message |
| `session.open` | Open a session |  | session: id prefix, @open, or part of its name |
| `session.send` | Send a message |  | text, session? |
| `session.rename` | Rename a session |  | name, session? |
| `session.delete` | Delete a session |  | session |
| `session.model` | Change the model |  | model, session? |
| `session.tab` | New tab in this workspace |  | a new session in the open session's folder |
| `session.fork` | Fork to new tab |  | workspace?: true for a new worktree; session?. carries the conversation on, and opens it |
| `model.favorite` | Use favourite model 1 | ⌃ ⌘ 1 | n: 1-5, the favourite's place; session? |
| `model.effort` | Think harder | ⇧ ⌘ E | effort?: low|medium|high|xhigh|max|default; without one, the next level up; session? |
| `model.fast` | Fast mode on or off | ⌥ ⌘ F | fast?: true|false; without it, flips; session? |
| `model.list` | The models and the favourites |  |  |
| `model.favorites` | Replace the favourites |  | models: [{model, effort?, fast?}], at most 5 |
| `session.mode` | Mode: Ask first |  | mode: default|acceptEdits|auto|plan, session? |
| `pane.split` | Split right | ⌘ D | dir: right|down, pane? |
| `pane.zoom` | Maximize or restore pane | ⇧ ⌘ ↩ | pane? |
| `pane.close` | Close pane | ⌘ W | pane? |
| `pane.pip` | Float pane as picture in picture |  | pane?, corner?: tl, tr, bl or br (default br) |
| `pane.dock` | Put pane back in the layout |  | pane? |
| `pane.show` | Show Conversation here |  | module, pane? |
| `files.open` | Open a file |  | path: relative to the session's folder, line?, col?, session? |
| `files.close` | Close the open file |  |  |
| `ui.shot` | Take a picture of the window |  | path?: where the PNG goes (a temporary file if not given) |
| `terminal.new` | New terminal | ⌃ ` | opens a shell below the focused pane; answers with its pane |
| `pane.focus` | Focus a pane |  | pane |
| `pane.toggle` | Show or hide a pane |  | pane |
| `pane.size` | Pane: small |  | size: small|half|large, or a fraction like 0.75; dir?: width|height (default width); pane? |
| `pane.move` | Move a pane |  | pane, to, zone: left|right|top|bottom|swap |
| `workspace.tab` | Workspace: Changes |  | tab: changes|files|checks|artifacts|info |
| `theme.set` | Theme: Tessel |  | base: tessel|orchard |
| `theme.appearance` | Appearance: System |  | mode: system|light|dark |
| `theme.size` | Interface size: Compact |  | size: compact|default|large |
| `theme.accent` | Set the accent colour |  | hex: #rrggbb, or null for the theme's own |
| `daemon.set` | Agents: let them use tessel |  | agent_tools?: true|false. tessel's tools for agents, as each next starts |
| `theme.use` | Use a theme |  | name: one of theme.list, or a file in ~/.tessel/themes |
| `theme.list` | Every theme |  |  |
| `theme.density` | Density: Islands |  | density: islands|compact |
| `theme.material` | Material: Solid |  | material: solid|glass|felt |
| `theme.backdrop` | Backdrop: None |  | backdrop: none|dither|space |
| `workspace.run` | Run the workspace's script |  | script?: its name (the default one if not given) |
| `workspace.stop` | Stop the workspace's script |  | script?: its name (the default one if not given) |
| `workspace.setup` | Run the workspace's setup again |  |  |
| `workspace.write_scripts` | Set up scripts with an agent |  | a new chat that writes .tessel/settings.toml |
| `workspace.scripts` | The workspace's scripts and ports |  |  |
| `ship.pr` | Create a pull request |  | draft?: true for a draft |
| `ship.merge` | Merge the pull request |  | rebase?: true to rebase instead of squash |
| `ship.update` | Update from main |  |  |
| `ship.into_main` | Squash into main |  | push?: true to push main too; rebase?: true to keep every commit |
| `browser.open` | Open the browser |  | url?: where to go (the workspace's server if not given) |
| `browser.pick` | Point at part of the page |  |  |
| `desktop.next` | Next desktop | ⌥ ⌘ → |  |
| `desktop.prev` | Previous desktop | ⌥ ⌘ ← |  |
| `desktop.new` | New desktop |  | name?; preset?: default|studio|focus|tiles|rail; copy?: true to start from the layout showing; scope?: project names its sidebar lists |
| `desktop.close` | Close this desktop |  | desktop?: its number (from 1), name or id. its chats keep running |
| `desktop.use` | Show a desktop |  | desktop: its number (from 1), name or id |
| `desktop.rename` | Rename a desktop |  | name; desktop? |
| `desktop.scope` | Pick the projects a desktop lists |  | projects: project names, or [] for all; desktop? |
| `desktop.show` | Show a chat on a desktop |  | session: id prefix or part of its name; desktop?: number, name or id, or "new" for one of its own |
| `desktop.list` | Every desktop |  |  |
| `app.tour` | Take the tour |  |  |
| `app.notch` | Notch: Show agents |  | show: true|false. the agents at work, around the camera |
| `backdrop.next` | Next space photo |  | another photograph from NASA's library, for the Space backdrop |
| `terminal.look` | Terminal colours: Theme |  | size?: 9 to 24 points; colors?: theme|ink|paper|midnight |
| `layout.use` | Layout: Studio |  | name: studio|focus|tiles|rail |
| `layout.reset` | Reset the layout |  |  |
| `layout.get` | The layout file (ui.json) |  |  |
| `layout.set` | Replace the layout file |  | json: the whole ui.json; answers with what check.txt says |
| `ui.ls` | What the window shows |  |  |
| `commands.list` | Every command |  |  |

## Guardrails

A layout can say anything, but it can't look broken by accident. Before it is
drawn, tessel pulls it onto its scales and reports what it changed in `check.txt`:

- **Spacing** (`p*`, `gap`) lands on 0, 2, 4, 6, 8, 12, 16, 20, 24, 32, 40, 48, 64.
- **Sizes** (`w`, `h`, `min_*`, `max_*`) are multiples of 4.
- **Radii** land on the named ones. A surface inside a rounded surface gets a
  concentric corner: inner radius = outer radius minus the padding between them.
  Leave `radius` off inner surfaces and they get the right one.
- **Type sizes** land on 10, 11, 12, 13, 14, 15, 17, 20, 24, 28, 34. **Weights** on 450, 550, 650.
- **Opacity** never goes below 0.4.
- A **transcript** always gets room to grow.
- **Text is always readable.** Text and secondary text in a theme are kept at 4.5:1
  on every surface, the accent at 3:1 on the content, and text on the accent readable.
  A `color` asked for on a node is darkened or lightened until it reads on what it
  sits on. A node with a background the theme didn't plan for (`"bg": "accent"`, a
  hex value) has every ink inside it, including the components', re-derived for that
  background. So `{"type": "column", "bg": "accent", "children": [{"type": "title"}]}`
  draws a readable title, in light and dark.

And it warns about what it can't fix quietly: more than one primary button always
on screen, a bordered card in a bordered card without a shadow to say why, status
colours used as decoration, hex colours (they don't follow light and dark), no title
bar to move the window by, nothing leaving room for the window buttons, no `input`,
no `transcript`.

To break the rules on purpose, put `"freestyle": true` at the top of the file. It
is drawn exactly as written, with nothing adjusted and nothing checked except mistakes
that stop it loading.

## Taste

The guardrails keep a layout from looking broken. These keep it calm. They come from
orchard's design rules.

- One primary button per view. Everything else is `ghost`.
- Hierarchy from weight and tone (`text`, `muted`, `faint`), not size.
- Hairlines (`border`), not boxes inside boxes.
- Every word earns its place. A `text` that doesn't change what someone does is cut.
