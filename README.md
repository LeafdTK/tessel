# Tessel

A Mac app for running a bunch of coding agents at once. Each one gets its own git
worktree, they keep running when you close the window, and anything you can click you
can also script.

Built in Rust on [gpui](https://www.gpui.rs), on top of the
[Claude Code](https://docs.claude.com/en/docs/claude-code) CLI.

![Tessel, dark](assets/screenshots/dark.png)

## What's in it

- **Agents in parallel.** New session, new worktree and branch. Whatever needs you
  floats to the top of the sidebar.
- **Workspaces that just work.** New worktrees get your `.env` files, run your setup
  script and get ten ports of their own (`$TESSEL_PORT`). Scripts live in
  `.tessel/settings.toml`, or ask an agent to write them.
- **Checkpoints.** Every message takes one. Undo a turn, or fork from any point into a
  new tab or worktree.
- **Shipping.** Open a PR, squash or rebase it in, or squash straight into main. Never
  a merge commit. Conflicts go back to the agent with one click.
- **A shared browser.** A real WebKit page in a pane. The agent clicks around your app
  in it while you watch. Hit Pick, click something, and it's attached to your next
  message, with the source file it came from when you're on a dev build.
- **Desktops.** Separate layouts per project. `Cmd 1`-`9` or swipe on the sidebar.
- **Panes.** Terminal (on [Ghostty](https://ghostty.org)'s core), git graph, diffs,
  files, PR checks. Split them however you like.
- **The notch.** On a notched MacBook, your agents live around the camera. Hover to see
  them all and approve things without switching apps.
- **`Cmd K`** for everything, plus usage limits by the message box and a short tour on
  first launch.

![Tiles layout with a terminal and the git graph](assets/screenshots/tiles.png)

<p align="center"><img src="assets/screenshots/notch.png" width="620" alt="The notch, open"></p>

![Command palette](assets/screenshots/palette.png)

## Scripting

The window, the CLI and the agents all talk to the same daemon, so they can all do the
same things.

```sh
tesselctl new --worktree --model sonnet "add a --json flag to the forecast command"
tesselctl ls
tesselctl approve 4f7b
tesselctl ui layout.use name=tiles
tesselctl term run 2 "cargo test"
```

## Looks

Twelve themes, light and dark, glass or felt. The whole window is `~/.tessel/ui.json`,
and it reloads when you save.

![Light](assets/screenshots/light.png)

![Themes](assets/screenshots/themes.png)

## Building

You need macOS 13+, Rust (pinned in `rust-toolchain.toml`), `brew install zig@0.15`
for the terminal, and the `claude` CLI logged in.

```sh
export PATH="$(brew --prefix zig@0.15)/bin:$PATH"
cargo build --release -p tessel -p tesseld -p tesselctl
scripts/bundle-macos.sh release
open target/release/Tessel.app
```

Heads up: the first build is big, several GB.

State lives in `~/.tessel` and worktrees in `~/tessel/workspaces` (`TESSEL_HOME` and
`TESSEL_WORKSPACES` move them).

## Code

| crate           | what                                   |
| --------------- | -------------------------------------- |
| `tessel`        | the app                                |
| `tesseld`       | the daemon                             |
| `tesselctl`     | the CLI, and the MCP server for agents |
| `tessel-proto`  | the protocol                           |
| `tessel-client` | talking to the daemon                  |
| `tessel-agent`  | running agents                         |
| `tessel-term`   | the terminal                           |
| `tessel-rich`   | diagrams and math in replies           |

```sh
cargo test -p tessel -p tesseld -p tesselctl -p tessel-proto -p tessel-client -p tessel-agent -p tessel-term
```

Tests replay recorded sessions, so no network or `claude` needed.
