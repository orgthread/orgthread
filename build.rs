//! Hands the commit of this build to the code, so that the screen can name it.
//! A build outside a git repository says `unknown`.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let changed = git(&["status", "--porcelain", "--untracked-files=no"])
        .is_some_and(|list| !list.is_empty());
    let mark = if changed { "+" } else { "" };
    println!("cargo:rustc-env=ORGTHREAD_COMMIT={commit}{mark}");
    // A new commit or a changed file must give a new line on the screen.
    for name in ["HEAD", "index", "refs/heads"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    for path in ["src", "tests", "Cargo.toml", "README.md", "build.rs"] {
        println!("cargo:rerun-if-changed={path}");
    }
}
