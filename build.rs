use std::process::Command;

fn main() {
    // If NEXRAD_VERSION is already set (e.g. by CI for tagged releases), pass it
    // through and skip git detection.
    if let Ok(version) = std::env::var("NEXRAD_VERSION") {
        println!("cargo:rustc-env=NEXRAD_VERSION={version}");
        println!("cargo:rustc-env=NEXRAD_VERSION_FULL={version}");
        return;
    }

    // Otherwise, derive a version string from git.
    //
    // GitHub Actions checks out PRs as detached HEAD (merge commit), so
    // `git rev-parse --abbrev-ref HEAD` returns "HEAD" and the hash is a
    // synthetic merge commit. Use GITHUB_HEAD_REF (the PR source branch) when
    // available. For the commit hash, prefer PR_HEAD_SHA (the actual PR tip,
    // set explicitly in the workflow) over GITHUB_SHA (which is the merge
    // commit for pull_request events).
    let ci_branch = std::env::var("GITHUB_HEAD_REF")
        .ok()
        .filter(|s| !s.is_empty());
    let ci_hash = std::env::var("PR_HEAD_SHA")
        .or_else(|_| std::env::var("GITHUB_SHA"))
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s[..7].to_string());

    let branch =
        ci_branch.or_else(|| git(&["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD"));
    let hash = ci_hash.or_else(|| git(&["rev-parse", "--short=7", "HEAD"]));

    match (branch.as_deref(), hash.as_deref()) {
        (Some(branch), Some(hash)) => {
            let display = format!("{branch} ({hash})");
            println!("cargo:rustc-env=NEXRAD_VERSION={display}");
            println!("cargo:rustc-env=NEXRAD_VERSION_FULL={display}");
        }
        _ => {
            println!("cargo:rustc-env=NEXRAD_VERSION=dev");
            println!("cargo:rustc-env=NEXRAD_VERSION_FULL=dev");
        }
    }
}

fn git(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let s = String::from_utf8(o.stdout).ok()?;
            let trimmed = s.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
}
