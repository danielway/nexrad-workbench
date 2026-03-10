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
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let hash = git(&["rev-parse", "--short=7", "HEAD"]);

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
