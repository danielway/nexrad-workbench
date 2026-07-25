//! Architecture ratchet — enforced at build time (invoked from `build.rs`).
//!
//! Scans every `crate::<module>` reference in `src/` and fails the build when a
//! cross-module dependency edge appears that is neither ALLOWED (intended
//! layering) nor GRANDFATHERED (a known violation being burned down). It also
//! fails when a GRANDFATHERED edge no longer exists, so the table can only
//! shrink — the ratchet never loosens silently.
//!
//! Intended layering (an edge means "may import"; see
//! docs/arch-review-2026-07/README.md, finding F1):
//!
//! ```text
//!   net, data, geo            leaves — import nothing
//!   core                      pure domain + decisions — data, geo, alerts(types)
//!   alerts, mping             feed modules — net, core (+each other's shared bits)
//!   nexrad                    pipeline — core, data, geo, net
//!   state                     app state — core, data, geo, nexrad, alerts, mping
//!   subsystem                 bounded state owners — state and below
//!   app                       imperative shell — everything except ui
//!   ui                        thin render shell — reads every layer
//! ```
//!
//! Comments (`//`, `/* */`) are stripped before scanning, so prose references
//! like ``[`crate::app`]`` do not count as edges.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// Intended, sanctioned edges. Add here only with an architectural reason.
const ALLOWED: &[(&str, &str)] = &[
    // core is the pure base; it may use the key/geo vocabulary and pure alert
    // types + geometry predicates.
    ("core", "data"),
    ("core", "geo"),
    ("core", "alerts"),
    ("core", "mping"), // StormReport is pure feed vocabulary, like alerts types
    // feed modules — may use net + the pure core vocabulary (their own
    // feed-state containers live in core::domain::feeds)
    ("alerts", "net"),
    ("alerts", "core"),
    ("mping", "net"),
    ("mping", "core"),
    ("mping", "alerts"), // shared feed plumbing (channel idiom)
    ("mping", "data"),
    // pipeline
    ("nexrad", "core"),
    ("nexrad", "data"),
    ("nexrad", "geo"),
    ("nexrad", "net"),
    // app state sits above the domain modules
    ("state", "core"),
    ("state", "data"),
    ("state", "geo"),
    ("state", "nexrad"),
    ("state", "alerts"),
    ("state", "mping"),
    // bounded state owners
    ("subsystem", "state"),
    ("subsystem", "core"),
    ("subsystem", "data"),
    ("subsystem", "geo"),
    ("subsystem", "net"),
    ("subsystem", "nexrad"),
    ("subsystem", "alerts"),
    ("subsystem", "mping"),
    // imperative shell
    ("app", "core"),
    ("app", "state"),
    ("app", "subsystem"),
    ("app", "nexrad"),
    ("app", "data"),
    ("app", "geo"),
    ("app", "net"),
    ("app", "alerts"),
    ("app", "mping"),
    // thin render shell — may read every layer (logic direction is enforced by
    // the core/shell standard, not by imports)
    ("ui", "core"),
    ("ui", "state"),
    ("ui", "subsystem"),
    ("ui", "nexrad"),
    ("ui", "data"),
    ("ui", "geo"),
    ("ui", "net"),
    ("ui", "alerts"),
    ("ui", "mping"),
];

/// Known violations of the intended layering, kept only until burned down.
/// Removing the last occurrence of an edge REQUIRES deleting its row here
/// (the build fails on stale rows). Never add to this list.
const GRANDFATHERED: &[(&str, &str, &str)] = &[
    (
        "core",
        "state",
        "misplaced domain types (Scan/Sweep/AppCommand/...) live in state — arch-review F1/A1",
    ),
    (
        "core",
        "subsystem",
        "core decision fns read subsystem containers directly — arch-review F1/A1",
    ),
    (
        "core",
        "nexrad",
        "core::panels uses nexrad::projection::ScanProjection — rehome per A1",
    ),
    (
        "nexrad",
        "state",
        "RenderProcessing/Scan/Sweep etc. live in state — rehome per A1",
    ),
    (
        "alerts",
        "state",
        "manager mutates AlertsState/ErrorContext directly — feed-skeleton rework, Phase D",
    ),
    (
        "mping",
        "state",
        "manager mutates MpingState/ErrorContext directly — feed-skeleton rework, Phase D",
    ),
    (
        "app",
        "ui",
        "geolocation effect executor lives in ui — move executor into app, Phase C2",
    ),
];

pub fn run() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=tools/arch_check.rs");

    let src = Path::new("src");
    if !src.is_dir() {
        return; // e.g. cargo publish verification from another cwd
    }

    let modules = top_level_modules(src);
    let mut edges: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    scan_dir(src, src, &modules, &mut edges);

    let allowed: BTreeSet<(&str, &str)> = ALLOWED.iter().copied().collect();
    let grandfathered: BTreeMap<(&str, &str), &str> = GRANDFATHERED
        .iter()
        .map(|(f, t, why)| ((*f, *t), *why))
        .collect();

    let mut errors = String::new();

    for ((from, to), sites) in &edges {
        let key = (from.as_str(), to.as_str());
        if allowed.contains(&key) || grandfathered.contains_key(&key) {
            continue;
        }
        errors.push_str(&format!(
            "\n  new dependency edge `{from} -> {to}` ({} site{}):\n",
            sites.len(),
            if sites.len() == 1 { "" } else { "s" }
        ));
        for site in sites.iter().take(8) {
            errors.push_str(&format!("      {site}\n"));
        }
        if sites.len() > 8 {
            errors.push_str(&format!("      ... and {} more\n", sites.len() - 8));
        }
    }

    for (from, to) in grandfathered.keys() {
        if !edges.contains_key(&(from.to_string(), to.to_string())) {
            errors.push_str(&format!(
                "\n  grandfathered edge `{from} -> {to}` no longer occurs — \
                 delete its row from GRANDFATHERED in tools/arch_check.rs \
                 (the ratchet only tightens)\n"
            ));
        }
    }

    if !errors.is_empty() {
        panic!(
            "\n\narchitecture check failed (tools/arch_check.rs):\n{errors}\n\
             Either fix the dependency direction, or — only with a real \
             architectural justification — add the edge to ALLOWED. Never add \
             to GRANDFATHERED.\n\n"
        );
    }
}

fn top_level_modules(src: &Path) -> BTreeSet<String> {
    let mut mods = BTreeSet::new();
    for entry in fs::read_dir(src).expect("read src/").flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            mods.insert(name);
        } else if let Some(stem) = name.strip_suffix(".rs") {
            if stem != "lib" && stem != "main" {
                mods.insert(stem.to_string());
            }
        }
    }
    mods
}

fn scan_dir(
    dir: &Path,
    src: &Path,
    modules: &BTreeSet<String>,
    edges: &mut BTreeMap<(String, String), Vec<String>>,
) {
    for entry in fs::read_dir(dir).expect("read dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, src, modules, edges);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let rel = path
            .strip_prefix(src)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let top = rel
            .split('/')
            .next()
            .unwrap()
            .trim_end_matches(".rs")
            .to_string();
        if top == "lib" || top == "main" {
            continue; // crate root wires all modules together by design
        }
        let text = fs::read_to_string(&path).expect("read source file");
        scan_file(&text, &rel, &top, modules, edges);
    }
}

fn scan_file(
    text: &str,
    rel: &str,
    top: &str,
    modules: &BTreeSet<String>,
    edges: &mut BTreeMap<(String, String), Vec<String>>,
) {
    let mut in_block_comment = false;
    for (lineno, raw) in text.lines().enumerate() {
        let line = strip_comments(raw, &mut in_block_comment);
        let bytes = line.as_bytes();
        let mut i = 0;
        while let Some(pos) = line[i..].find("crate::") {
            let start = i + pos;
            // reject `xcrate::` / `::crate::` style false matches
            let boundary_ok = start == 0
                || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
            i = start + "crate::".len();
            if !boundary_ok {
                continue;
            }
            let rest = &line[i..];
            let target: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if modules.contains(&target) && target != top {
                edges
                    .entry((top.to_string(), target))
                    .or_default()
                    .push(format!("src/{rel}:{}", lineno + 1));
            }
        }
    }
}

/// Strip `//` line comments and `/* */` block comments. String literals are
/// not tracked — a `crate::` inside a string would be counted, which is
/// acceptable for a policy check (none exist today).
fn strip_comments(line: &str, in_block: &mut bool) -> String {
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if *in_block {
            if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                *in_block = false;
                i += 2;
            } else {
                i += 1;
            }
        } else if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            break; // rest of line is a comment
        } else if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            *in_block = true;
            i += 2;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}
