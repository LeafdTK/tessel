//! projects made where the daemon runs: a repository cloned, or a new one
//! from a template, and the folders to choose from. here, not in the window,
//! because the window may be on another machine (a mac showing a dev box).

use std::path::{Path, PathBuf};
use std::process::Command;
use tessel_proto::ProjectHow;

/// `~` and `~/…` as this machine's home.
pub fn expand(path: &str) -> PathBuf {
    let home = || PathBuf::from(std::env::var("HOME").unwrap_or_default());
    match path.trim() {
        "~" => home(),
        p => match p.strip_prefix("~/") {
            Some(rest) => home().join(rest),
            None => PathBuf::from(p),
        },
    }
}

/// a folder name made from what someone typed: lower case, dashes for spaces.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("Couldn't run {what}: {e}."))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("{what} failed: {}", err.lines().last().unwrap_or("").trim()))
    }
}

/// make the project. its folder, and what its first session is asked.
pub fn make(parent: &str, how: &ProjectHow) -> Result<(PathBuf, String), String> {
    let parent = expand(parent);
    match how {
        ProjectHow::Clone { url, name } => {
            let dir = parent.join(name);
            if dir.exists() {
                return Err(format!("{} already exists.", dir.display()));
            }
            let _ = std::fs::create_dir_all(&parent);
            run(Command::new("git").arg("clone").arg(url).arg(&dir), "git clone")?;
            Ok((dir, String::new()))
        }
        ProjectHow::New { name, template, github } => {
            let folder = slug(name);
            if folder.is_empty() {
                return Err("Give the project a name.".into());
            }
            let dir = parent.join(&folder);
            new_project(&dir, name, template, *github).map(|prompt| (dir, prompt))
        }
    }
}

fn new_project(dir: &Path, name: &str, template: &str, github: bool) -> Result<String, String> {
    if dir.exists() && std::fs::read_dir(dir).map(|mut d| d.next().is_some()).unwrap_or(false) {
        return Err(format!("{} already exists and isn't empty.", dir.display()));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't make {}: {e}.", dir.display()))?;
    let git = |args: &[&str]| run(Command::new("git").args(args).current_dir(dir), "git");
    match template {
        "rust" => run(Command::new("cargo").args(["init", "--vcs", "git", "--name", &slug(name).replace('.', "_")]).current_dir(dir), "cargo init")?,
        _ => {
            git(&["init", "-b", "main"])?;
            std::fs::write(dir.join("README.md"), format!("# {name}\n")).map_err(|e| format!("Couldn't write the readme: {e}."))?;
        }
    }
    // a first commit, so worktrees and branches have something to start from.
    // it can fail (no git identity set up); the project is still usable.
    let _ = git(&["add", "-A"]).and_then(|_| git(&["commit", "-q", "-m", "chore: start the project"]));
    if github {
        run(Command::new("gh").args(["repo", "create", &slug(name), "--private", "--source", ".", "--push"]).current_dir(dir), "gh repo create")?;
    }
    Ok(match template {
        "next" => "Set up a Next.js app in this empty repository with create-next-app: TypeScript, Tailwind and the App Router, in this folder rather than a new one. Then commit it.".into(),
        _ => String::new(),
    })
}

/// the folders in `path`: its full path, their names (hidden ones left out,
/// sorted), and whether it's a repository.
pub fn folders(path: &str) -> Result<(String, Vec<String>, bool), String> {
    let dir = expand(path);
    let dir = dir.canonicalize().map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    let repo = dir.join(".git").exists();
    Ok((dir.to_string_lossy().into_owned(), names, repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_project_is_a_git_repository_with_a_readme_and_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let how = ProjectHow::New { name: "My App".into(), template: "empty".into(), github: false };
        let (dir, prompt) = make(&tmp.path().to_string_lossy(), &how).unwrap();
        assert_eq!(dir, tmp.path().join("my-app"));
        assert!(dir.join(".git").is_dir() && dir.join("README.md").is_file());
        assert!(prompt.is_empty());
        assert!(make(&tmp.path().to_string_lossy(), &how).is_err(), "never into a folder with things in it");
        let (_, names, repo) = folders(&tmp.path().to_string_lossy()).unwrap();
        assert_eq!(names, vec!["my-app"]);
        assert!(!repo);
        assert!(folders(&dir.to_string_lossy()).unwrap().2, "a repository says so");
    }

    #[test]
    fn home_is_this_machines() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand("~"), PathBuf::from(&home));
        assert_eq!(expand("~/code"), PathBuf::from(&home).join("code"));
        assert_eq!(expand("/srv/x"), PathBuf::from("/srv/x"));
        assert_eq!(slug("My  App!"), "my-app");
    }
}
