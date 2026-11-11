//! the launcher: read the command line, hand over to the library.

use photon::shell::Launch;

const HELP: &str = "photon: the app

  photon [--open SESSION] [--focus]

  --open SESSION   open this session (any unique prefix of its id) at launch
  --focus          start with the sidebar hidden: only the open session is shown
  --overview       start on the overview of all sessions
  --panel TAB      start with the workspace panel open on changes, files, checks, artifacts or info
  --file PATH      open this file (a path inside the session's folder) in the panel's editor
  --dark, --light  use this appearance instead of following the system
  --ui FILE        draw the window from this layout file instead of ~/.photon/ui.json
  --check-ui FILE  check a layout file: errors, warnings, values moved onto photon's scales
  --bench-startup  print the time to the first frame showing sessions, then quit
  --bench-motion   run every transition, print frame times and late frames, then quit

keys: ⌘N new session · ⌘J next session that needs you · ⌘↵ allow · ⌘. stop the agent
      ⌥⌘S sidebar · ⌥⌘I inspector · ⇧⌘F focus on this session · ⇧⌘\\ all sessions
env:  PHOTON_HOME which daemon (default ~/.photon) · PHOTON_DEFAULT_CWD starting folder for new sessions

the window is a layout file, watched while it runs: ~/.photon/ui.json.
its reference and the shipped presets are in ~/.photon/ui/.";

fn main() {
    let started = std::time::Instant::now();
    let mut launch = Launch::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--open" => launch.open = args.next(),
            "--focus" => launch.focus = true,
            "--overview" => launch.overview = true,
            "--panel" => launch.panel = args.next(),
            "--file" => launch.file = args.next(),
            "--ui" => launch.ui = args.next().map(std::path::PathBuf::from),
            "--check-ui" => {
                let Some(path) = args.next() else {
                    eprintln!("photon: --check-ui needs a file");
                    std::process::exit(2);
                };
                let result = std::fs::read_to_string(&path).map_err(|e| format!("couldn't read {path}: {e}")).and_then(|t| photon::ui::parse(&t));
                print!("{}", photon::ui::harmony::check_text(&result));
                std::process::exit(if result.is_ok() { 0 } else { 1 });
            }
            "--bench-motion" => launch.bench_motion = true,
            "--dark" => launch.dark = Some(true),
            "--light" => launch.dark = Some(false),
            "--bench-startup" => launch.bench_from = Some(started),
            "--help" | "-h" => {
                println!("{HELP}");
                return;
            }
            other => {
                eprintln!("photon: unknown argument {other}\n\n{HELP}");
                std::process::exit(2);
            }
        }
    }
    photon::run(launch);
}
