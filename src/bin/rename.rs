//! Renames the template. The first thing anyone does with a fork, and the one
//! job where doing it by hand is genuinely error-prone.
//!
//!     cargo run --bin rename -- <crate-name> "<Product Name>" <identifier>
//!     cargo run --bin rename -- notes-app "Notes" com.example.notes
//!
//! It edits three files and eight lines. It deliberately does **not** sweep
//! the repository: `README.md` is prose about this template, and the
//! `com.example.*` strings throughout the tests are fixtures — `com.example` is
//! the domain reserved for examples, and a fixture that looks like real identity
//! is worse than one that obviously does not.
//!
//! The two files hold the same identity twice, because `cargo-packager` can only
//! read `[package.metadata.packager]` out of `Cargo.toml` while the running app
//! reads `app.toml`. Nothing can interpolate between them, so instead of
//! centralising they are written together here and a test refuses to let them
//! drift (`config::tests::the_two_manifests_agree_on_identity`).
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [crate_name, product, identifier] = args.as_slice() else {
        eprintln!(
            "usage: cargo run --bin rename -- <crate-name> \"<Product Name>\" <identifier>\n\
             \n\
             example:\n\
             \x20 cargo run --bin rename -- notes-app \"Notes\" com.example.notes\n\
             \n\
             crate-name  the binary and package name: lowercase, digits, - or _\n\
             product     the display name: window title, tray tooltip, installer\n\
             identifier  reverse-domain; names the data directory and macOS bundle"
        );
        return ExitCode::FAILURE;
    };

    if let Err(e) = check(crate_name, product, identifier) {
        eprintln!("rename: {e}");
        return ExitCode::FAILURE;
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    match apply(root, crate_name, product, identifier) {
        Ok(changed) => {
            for line in &changed {
                println!("  {line}");
            }
            println!(
                "\nrenamed to {product} ({crate_name}, {identifier})\n\
                 \n\
                 next:\n\
                 \x20 cargo run --bin gen_icons   # if you want the placeholder art regenerated\n\
                 \x20 cargo test                  # the identity test proves both files agree\n\
                 \n\
                 not touched, on purpose: README.md is about the template, and the\n\
                 com.example.* strings in tests are fixtures."
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rename: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Mirrors the rules the app enforces at startup — cargo's own package-name
/// rules, and `AppConfig::validate`'s reverse-domain check. Duplicated rather
/// than imported because this crate has no library target, so a binary cannot
/// reach `crate::config`. `cargo test` after a rename is the real gate.
fn check(crate_name: &str, product: &str, identifier: &str) -> Result<(), String> {
    if crate_name.is_empty()
        || !crate_name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(format!(
            "crate name {crate_name:?} must be lowercase ASCII, digits, '-' or '_'"
        ));
    }
    if product.trim().is_empty() {
        return Err("the product name must not be empty".into());
    }
    let parts: Vec<&str> = identifier.split('.').collect();
    if parts.len() < 2
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    {
        return Err(format!(
            "identifier {identifier:?} must be reverse-domain: two or more \
             dot-separated parts of ASCII alphanumerics and '-'"
        ));
    }
    Ok(())
}

fn apply(
    root: &Path,
    crate_name: &str,
    product: &str,
    identifier: &str,
) -> Result<Vec<String>, String> {
    let mut changed = Vec::new();

    let cargo_path = root.join("Cargo.toml");
    let cargo = read(&cargo_path)?;
    // Captured before anything is rewritten: this is what Cargo.lock still calls
    // the package, and the lock edit below has to match the OLD name.
    let previous_name = package_name(&cargo)?;
    if previous_name == crate_name {
        return Err(format!("already named {crate_name:?}"));
    }
    let cargo = replace_key(
        &cargo,
        "name",
        &quoted(crate_name),
        Section::Package,
        &mut changed,
    )?;
    let cargo = replace_key(
        &cargo,
        "default-run",
        &quoted(crate_name),
        Section::Package,
        &mut changed,
    )?;
    let cargo = replace_key(
        &cargo,
        "product-name",
        &quoted(product),
        Section::Packager,
        &mut changed,
    )?;
    let cargo = replace_key(
        &cargo,
        "identifier",
        &quoted(identifier),
        Section::Packager,
        &mut changed,
    )?;
    // `binaries` names the packaged executable and must track the crate name, or
    // cargo-packager looks for a file that is no longer built.
    let cargo = replace_line_starting(
        &cargo,
        "binaries = [",
        &format!(
            "binaries = [{{ path = {}, main = true }}]",
            quoted(crate_name)
        ),
        &mut changed,
    )?;
    write(&cargo_path, &cargo)?;

    let app_path = root.join("app.toml");
    let app = read(&app_path)?;
    let app = replace_aligned(&app, "name", &quoted(product), &mut changed)?;
    let app = replace_aligned(&app, "identifier", &quoted(identifier), &mut changed)?;
    write(&app_path, &app)?;

    // Cargo.lock records the package by name, so renaming the crate without it
    // makes every `--locked` build fail with "cannot update the lock file" --
    // which is every CI command in this repo, and the release workflow. Found by
    // running the rename and then `cargo test --locked`.
    //
    // The lock is edited rather than regenerated: `cargo update` would need the
    // network and could move dependency versions, which a rename has no business
    // doing.
    let lock_path = root.join("Cargo.lock");
    let lock = read(&lock_path)?;
    let old = format!("name = {}", quoted(&previous_name));
    let new = format!("name = {}", quoted(crate_name));
    let lock = replace_package_entry(&lock, &old, &new, &mut changed)?;
    write(&lock_path, &lock)?;

    Ok(changed)
}

/// The `[package].name` as it stands in the manifest text handed in. Called
/// before any rewriting, because `Cargo.lock` still records the OLD name and the
/// lock edit has to match on it.
fn package_name(cargo: &str) -> Result<String, String> {
    let mut in_package = false;
    for line in cargo.lines() {
        let t = line.trim_start();
        if t.starts_with('[') {
            in_package = t.starts_with("[package]");
        }
        if in_package {
            if let Some(rest) = t.strip_prefix("name = ") {
                return Ok(rest.trim().trim_matches('"').to_string());
            }
        }
    }
    Err("no [package].name in Cargo.toml".into())
}

/// Rewrites the crate's own `[[package]]` entry in `Cargo.lock`. Scoped to the
/// entry whose name matches, so a dependency that happens to share the name is
/// untouched.
fn replace_package_entry(
    lock: &str,
    old: &str,
    new: &str,
    changed: &mut Vec<String>,
) -> Result<String, String> {
    let mut out = Vec::new();
    let mut hits = 0;
    for line in lock.lines() {
        if line == old && hits == 0 {
            changed.push(format!("Cargo.lock: {old} -> {new}"));
            out.push(new.to_string());
            hits += 1;
            continue;
        }
        out.push(line.to_string());
    }
    if hits == 0 {
        return Err(format!("no `{old}` entry in Cargo.lock"));
    }
    Ok(out.join("\n") + "\n")
}

/// Which table a key must be found in. `identifier` appears in both manifests
/// and `name` appears three times in `Cargo.toml` alone, so an unqualified
/// search would rewrite a dependency.
#[derive(Clone, Copy, PartialEq)]
enum Section {
    Package,
    Packager,
}

impl Section {
    fn header(self) -> &'static str {
        match self {
            Section::Package => "[package]",
            Section::Packager => "[package.metadata.packager]",
        }
    }
}

fn quoted(s: &str) -> String {
    format!("{:?}", s)
}

fn read(p: &Path) -> Result<String, String> {
    std::fs::read_to_string(p).map_err(|e| format!("cannot read {}: {e}", p.display()))
}

fn write(p: &Path, s: &str) -> Result<(), String> {
    std::fs::write(p, s).map_err(|e| format!("cannot write {}: {e}", p.display()))
}

/// Replaces `key = <value>` inside one table, leaving any trailing comment.
fn replace_key(
    text: &str,
    key: &str,
    value: &str,
    section: Section,
    changed: &mut Vec<String>,
) -> Result<String, String> {
    let mut out = Vec::new();
    let mut in_section = false;
    let mut hits = 0;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_section = trimmed.starts_with(section.header());
        }
        if in_section && trimmed.starts_with(&format!("{key} = ")) && hits == 0 {
            let comment = line.find('#').map(|i| &line[i..]).unwrap_or("");
            let rebuilt = if comment.is_empty() {
                format!("{key} = {value}")
            } else {
                format!("{key} = {value}  {comment}")
            };
            changed.push(format!("{}: {} -> {value}", section.header(), key));
            out.push(rebuilt);
            hits += 1;
            continue;
        }
        out.push(line.to_string());
    }
    if hits == 0 {
        return Err(format!("no `{key}` found in {}", section.header()));
    }
    Ok(out.join("\n") + "\n")
}

fn replace_line_starting(
    text: &str,
    prefix: &str,
    replacement: &str,
    changed: &mut Vec<String>,
) -> Result<String, String> {
    let mut out = Vec::new();
    let mut hits = 0;
    for line in text.lines() {
        if line.trim_start().starts_with(prefix) && hits == 0 {
            changed.push(format!("Cargo.toml: {prefix}…"));
            out.push(replacement.to_string());
            hits += 1;
            continue;
        }
        out.push(line.to_string());
    }
    if hits == 0 {
        return Err(format!("no line starting `{prefix}` found"));
    }
    Ok(out.join("\n") + "\n")
}

/// `app.toml` aligns its trailing comments in a column; a plain replace would
/// ragged them. Preserves the column when the new value still fits.
fn replace_aligned(
    text: &str,
    key: &str,
    value: &str,
    changed: &mut Vec<String>,
) -> Result<String, String> {
    let mut out = Vec::new();
    let mut hits = 0;
    for line in text.lines() {
        if line.starts_with(&format!("{key} = ")) && hits == 0 {
            let rebuilt = match line.find('#') {
                Some(col) => {
                    let head = format!("{key} = {value}");
                    let pad = col.saturating_sub(head.len()).max(1);
                    format!("{head}{}{}", " ".repeat(pad), &line[col..])
                }
                None => format!("{key} = {value}"),
            };
            changed.push(format!("app.toml: {key} -> {value}"));
            out.push(rebuilt);
            hits += 1;
            continue;
        }
        out.push(line.to_string());
    }
    if hits == 0 {
        return Err(format!("no `{key}` found in app.toml"));
    }
    Ok(out.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO: &str = r#"[package]
name = "hitch"
version = "0.1.8"
default-run = "hitch"

[dependencies]
name = "not-the-package-name"

[package.metadata.packager]
product-name = "Hitch"
identifier = "co.doublenot.hitch"
binaries = [{ path = "hitch", main = true }]
"#;

    #[test]
    fn a_key_is_replaced_only_inside_its_own_table() {
        // `name` appears in [package] and again in [dependencies]. An
        // unqualified search-and-replace would rewrite the dependency, which is
        // the whole reason this is scoped rather than a sed.
        let mut ch = Vec::new();
        let out = replace_key(CARGO, "name", "\"notes\"", Section::Package, &mut ch).unwrap();
        assert!(out.contains("name = \"notes\"\nversion"));
        assert!(
            out.contains("name = \"not-the-package-name\""),
            "the dependency was rewritten:\n{out}"
        );
    }

    #[test]
    fn the_same_key_in_two_tables_resolves_by_section() {
        let mut ch = Vec::new();
        let out = replace_key(
            CARGO,
            "identifier",
            "\"com.x.y\"",
            Section::Packager,
            &mut ch,
        )
        .unwrap();
        assert!(out.contains("identifier = \"com.x.y\""));
        assert_eq!(ch.len(), 1);
    }

    #[test]
    fn a_missing_key_is_an_error_rather_than_a_silent_no_op() {
        // Silently doing nothing would leave a half-renamed project that builds
        // and misbehaves later.
        let mut ch = Vec::new();
        assert!(replace_key(CARGO, "nonesuch", "\"x\"", Section::Package, &mut ch).is_err());
    }

    #[test]
    fn app_toml_comment_alignment_survives() {
        let app = "name = \"Hitch\"                          # window title\n\
                   url = \"\"                                # target URL\n";
        let mut ch = Vec::new();
        let out = replace_aligned(app, "name", "\"Notes\"", &mut ch).unwrap();
        let cols: Vec<usize> = out
            .lines()
            .filter(|l| l.contains('#'))
            .map(|l| l.find('#').unwrap())
            .collect();
        assert_eq!(cols, vec![40, 40], "comments no longer line up:\n{out}");
    }

    #[test]
    fn a_long_name_keeps_one_space_rather_than_overlapping_the_comment() {
        let app = "name = \"Hitch\"                          # window title\n";
        let mut ch = Vec::new();
        let long = "\"A Rather Long Product Name That Overruns The Column\"";
        let out = replace_aligned(app, "name", long, &mut ch).unwrap();
        assert!(out.contains(&format!("{long} #")), "got: {out}");
    }

    #[test]
    fn the_previous_package_name_is_read_from_the_package_table() {
        // The lock edit matches on the OLD name, so reading this from the wrong
        // table would leave Cargo.lock stale and every --locked build broken.
        assert_eq!(package_name(CARGO).unwrap(), "hitch");
    }

    #[test]
    fn only_the_crates_own_lock_entry_is_rewritten() {
        let lock = "[[package]]\nname = \"hitch\"\nversion = \"0.1.8\"\n\n\
                    [[package]]\nname = \"serde\"\nversion = \"1.0\"\n";
        let mut ch = Vec::new();
        let out =
            replace_package_entry(lock, "name = \"hitch\"", "name = \"notes\"", &mut ch).unwrap();
        assert!(out.contains("name = \"notes\""));
        assert!(out.contains("name = \"serde\""), "a dependency was touched");
    }

    #[test]
    fn identifiers_and_crate_names_are_validated() {
        assert!(check("notes-app", "Notes", "com.example.notes").is_ok());
        assert!(
            check("Notes", "Notes", "com.example.notes").is_err(),
            "uppercase crate"
        );
        assert!(
            check("notes app", "Notes", "com.example.notes").is_err(),
            "space"
        );
        assert!(
            check("notes", "", "com.example.notes").is_err(),
            "empty product"
        );
        assert!(
            check("notes", "Notes", "single").is_err(),
            "not reverse-domain"
        );
        assert!(check("notes", "Notes", "com..notes").is_err(), "empty part");
    }
}
