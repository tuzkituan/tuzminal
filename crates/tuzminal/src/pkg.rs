//! The plugin and theme package manager.
//!
//! Deliberately dumb: installation is `git clone` into the data directory, and the
//! registry is a git repository holding an index TOML — homebrew-tap style. There
//! is no server to run, no account to create, and anyone can host a registry by
//! pushing a file. The cost is that updates are a `git pull` rather than a version
//! solve, which for terminal plugins is the right trade.
//!
//! # Trust
//!
//! Installing a plugin runs its code with your privileges. The install path
//! therefore *shows the permissions it asks for and requires confirmation* before
//! anything is written, and says plainly that a Lua plugin is not sandboxed. A
//! package manager that installs silently would make the whole permission system
//! decorative.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use tuz_config::Paths;
use tuz_plugin_api::{Manifest, Runtime};

/// What kind of thing is being installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plugin,
    Theme,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Plugin => "plugin",
            Kind::Theme => "theme",
        }
    }

    /// Where installed items of this kind live.
    fn install_root(self, paths: &Paths) -> PathBuf {
        match self {
            Kind::Plugin => paths.data_dir().join("plugins"),
            Kind::Theme => paths.data_dir().join("themes"),
        }
    }
}

/// Decide whether a source string is a git URL or a registry name.
///
/// A name is looked up in the registry index; anything that looks like a URL is
/// cloned directly, which is what makes "install from my own fork" work without
/// any registry involvement.
fn is_git_url(source: &str) -> bool {
    source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git@")
        || source.starts_with("ssh://")
        || source.starts_with("git://")
}

/// Derive a directory name from a git URL.
///
/// Rejects anything that could escape the install root, because this value becomes
/// a path component.
fn name_from_url(url: &str) -> Result<String> {
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    let last = trimmed
        .rsplit(['/', ':'])
        .next()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("cannot work out a name from `{url}`"))?;

    if last.contains("..") || last.contains('\\') || last.starts_with('.') {
        bail!("`{last}` is not a usable directory name");
    }
    if !last
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        bail!("`{last}` contains characters that are not valid in a directory name");
    }
    Ok(last.to_owned())
}

/// Run a git command, surfacing git's own stderr on failure.
fn git(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let mut command = std::process::Command::new("git");
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }

    let output = command
        .output()
        .context("failed to run `git`; is it installed and on PATH?")?;

    if !output.status.success() {
        // git's message is far more useful than anything we could synthesize.
        bail!(
            "git {} failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Install from a git URL or a registry name.
///
/// `yes` skips the confirmation prompt, for scripted installs.
pub fn install(paths: &Paths, kind: Kind, source: &str, yes: bool) -> Result<()> {
    let url = if is_git_url(source) {
        source.to_owned()
    } else {
        resolve_from_registry(paths, kind, source)?
    };

    let name = name_from_url(&url)?;
    let root = kind.install_root(paths);
    std::fs::create_dir_all(&root).with_context(|| format!("cannot create {}", root.display()))?;

    let target = root.join(&name);
    if target.exists() {
        bail!(
            "{} `{name}` is already installed at {}\nuse `tuzminal {} update {name}` instead",
            kind.label(),
            target.display(),
            kind.label()
        );
    }

    // Clone into a staging directory first, so a plugin that turns out to be
    // invalid or declined never lands in the install root.
    let staging = root.join(format!(".staging-{name}"));
    let _ = std::fs::remove_dir_all(&staging);

    println!("cloning {url}");
    git(
        &["clone", "--depth", "1", &url, &staging.to_string_lossy()],
        None,
    )?;

    let result = finish_install(kind, &staging, &target, &name, yes);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

/// Validate a staged clone and move it into place.
fn finish_install(kind: Kind, staging: &Path, target: &Path, name: &str, yes: bool) -> Result<()> {
    match kind {
        Kind::Plugin => {
            let manifest_path = staging.join("plugin.toml");
            let src = std::fs::read_to_string(&manifest_path)
                .with_context(|| format!("no plugin.toml found in {}", staging.display()))?;
            let manifest = Manifest::parse(&src)
                .with_context(|| format!("{} is not a valid manifest", manifest_path.display()))?;

            if !staging.join(&manifest.entry).is_file() {
                bail!(
                    "the manifest names entry `{}`, which is not in the repository",
                    manifest.entry
                );
            }

            if !confirm_plugin(&manifest, yes)? {
                println!("cancelled; nothing was installed");
                let _ = std::fs::remove_dir_all(staging);
                return Ok(());
            }
        }
        Kind::Theme => {
            // A theme repository may hold several themes, or be a single file.
            let count = std::fs::read_dir(staging)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("toml"))
                        .count()
                })
                .unwrap_or(0);
            if count == 0 {
                bail!("no .toml theme files found in the repository");
            }
            println!("found {count} theme file(s)");
        }
    }

    std::fs::rename(staging, target).with_context(|| {
        format!(
            "failed to move the staged download into {}",
            target.display()
        )
    })?;
    println!("installed {} `{name}`", kind.label());
    Ok(())
}

/// Show what a plugin is asking for and get consent.
///
/// The Lua warning is not boilerplate: for a Lua plugin the permission list
/// describes intent, not an enforced limit, and a user deciding whether to trust it
/// needs to know that.
fn confirm_plugin(manifest: &Manifest, yes: bool) -> Result<bool> {
    println!();
    println!("  {} {}", manifest.name, manifest.version);
    if !manifest.description.is_empty() {
        println!("  {}", manifest.description);
    }
    println!("  runtime: {:?}", manifest.runtime);

    if manifest.permissions.is_empty() {
        println!("  permissions: none");
    } else {
        println!("  it asks to be able to:");
        for permission in &manifest.permissions {
            println!("    - {}", permission.describe());
        }
    }

    match manifest.runtime {
        Runtime::Wasm => {
            println!("  this plugin is sandboxed: it can only do what is listed above");
        }
        Runtime::Lua => {
            println!();
            println!("  WARNING: Lua plugins are NOT sandboxed. This plugin can do");
            println!("  anything your user account can, regardless of the list above.");
            println!("  Only install it if you trust its source.");
        }
    }
    println!();

    if yes {
        return Ok(true);
    }

    print!("install? [y/N] ");
    use std::io::Write;
    std::io::stdout().flush().ok();

    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("failed to read from stdin")?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Look a name up in the configured registries.
fn resolve_from_registry(paths: &Paths, kind: Kind, name: &str) -> Result<String> {
    let index_path = paths.data_dir().join("registry").join("index.toml");
    let src = std::fs::read_to_string(&index_path).with_context(|| {
        format!(
            "no registry index at {}\nrun `tuzminal registry update` first, or pass a git URL",
            index_path.display()
        )
    })?;

    let index: RegistryIndex =
        toml::from_str(&src).with_context(|| format!("{} is malformed", index_path.display()))?;

    let table = match kind {
        Kind::Plugin => &index.plugins,
        Kind::Theme => &index.themes,
    };
    table
        .get(name)
        .map(|entry| entry.url.clone())
        .with_context(|| {
            format!(
                "no {} named `{name}` in the registry\ntry `tuzminal {} search` to see what is available",
                kind.label(),
                kind.label()
            )
        })
}

/// The registry index format: a name -> URL map per kind.
#[derive(Debug, Default, serde::Deserialize)]
struct RegistryIndex {
    #[serde(default)]
    plugins: std::collections::BTreeMap<String, RegistryEntry>,
    #[serde(default)]
    themes: std::collections::BTreeMap<String, RegistryEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct RegistryEntry {
    url: String,
    #[serde(default)]
    description: String,
}

/// Default registry, clonable by anyone who wants to run their own.
const DEFAULT_REGISTRY: &str = "https://github.com/tuzminal/registry";

/// Clone or update the registry index.
pub fn update_registry(paths: &Paths, url: Option<&str>) -> Result<()> {
    let dir = paths.data_dir().join("registry");
    if dir.join(".git").is_dir() {
        println!("updating the registry");
        git(&["pull", "--ff-only"], Some(&dir))?;
    } else {
        let url = url.unwrap_or(DEFAULT_REGISTRY);
        std::fs::create_dir_all(paths.data_dir())?;
        println!("cloning the registry from {url}");
        git(
            &["clone", "--depth", "1", url, &dir.to_string_lossy()],
            None,
        )?;
    }
    println!("registry is up to date");
    Ok(())
}

/// List what is installed.
pub fn list(paths: &Paths, kind: Kind) -> Result<()> {
    match kind {
        Kind::Plugin => {
            let found = tuz_plugin::discover(paths.plugin_dirs());
            if found.is_empty() {
                println!("no plugins installed");
                return Ok(());
            }
            for entry in found {
                match entry {
                    Ok((path, manifest)) => println!(
                        "{:<24} {:<10} {:?}  {}",
                        manifest.name,
                        manifest.version,
                        manifest.runtime,
                        path.display()
                    ),
                    // A broken plugin should be visible here; it is exactly what the
                    // user is trying to diagnose.
                    Err(e) => println!("{:<24} (broken) {e}", "?"),
                }
            }
        }
        Kind::Theme => {
            let names = tuz_config::Theme::available(paths);
            for name in names {
                println!("{name}");
            }
        }
    }
    Ok(())
}

/// Remove an installed item.
///
/// Only ever deletes inside the install root, and only a direct child of it.
pub fn remove(paths: &Paths, kind: Kind, name: &str) -> Result<()> {
    // The name becomes a path component, so validate before touching the disk.
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.')
    {
        bail!("`{name}` is not a valid {} name", kind.label());
    }

    let root = kind.install_root(paths);
    let target = root.join(name);

    // Belt and braces: confirm the resolved path really is inside the root, so a
    // symlink or a name that slipped past the checks above cannot delete elsewhere.
    let canonical_root = root.canonicalize().unwrap_or(root.clone());
    let canonical_target = match target.canonicalize() {
        Ok(p) => p,
        Err(_) => bail!(
            "{} `{name}` is not installed (nothing at {})",
            kind.label(),
            target.display()
        ),
    };
    if !canonical_target.starts_with(&canonical_root) {
        bail!(
            "refusing to remove {}: it resolves outside {}",
            canonical_target.display(),
            canonical_root.display()
        );
    }

    if canonical_target.is_dir() {
        std::fs::remove_dir_all(&canonical_target)
    } else {
        std::fs::remove_file(&canonical_target)
    }
    .with_context(|| format!("failed to remove {}", canonical_target.display()))?;

    println!("removed {} `{name}`", kind.label());
    Ok(())
}

/// Update one installed item, or all of them.
pub fn update(paths: &Paths, kind: Kind, name: Option<&str>) -> Result<()> {
    let root = kind.install_root(paths);
    let targets: Vec<PathBuf> = match name {
        Some(name) => vec![root.join(name)],
        None => std::fs::read_dir(&root)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.join(".git").is_dir())
                    .collect()
            })
            .unwrap_or_default(),
    };

    if targets.is_empty() {
        println!("nothing to update");
        return Ok(());
    }

    let mut failures = 0;
    for target in targets {
        let label = target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        if !target.join(".git").is_dir() {
            println!("{label}: not a git checkout, skipping");
            continue;
        }
        print!("{label}: ");
        // One failure must not stop the rest; report and continue.
        match git(&["pull", "--ff-only"], Some(&target)) {
            Ok(output) => {
                let summary = output.lines().last().unwrap_or("updated");
                println!("{}", summary.trim());
            }
            Err(e) => {
                failures += 1;
                println!("failed\n  {e}");
            }
        }
    }

    if failures > 0 {
        bail!("{failures} item(s) failed to update");
    }
    Ok(())
}

/// Print the registry's contents.
pub fn search(paths: &Paths, kind: Kind, query: Option<&str>) -> Result<()> {
    let index_path = paths.data_dir().join("registry").join("index.toml");
    let src = std::fs::read_to_string(&index_path).with_context(|| {
        format!(
            "no registry index at {}\nrun `tuzminal registry update` first",
            index_path.display()
        )
    })?;
    let index: RegistryIndex = toml::from_str(&src)?;

    let table = match kind {
        Kind::Plugin => &index.plugins,
        Kind::Theme => &index.themes,
    };

    let query = query.map(|q| q.to_lowercase());
    let mut shown = 0;
    for (name, entry) in table {
        if let Some(q) = &query {
            if !name.to_lowercase().contains(q) && !entry.description.to_lowercase().contains(q) {
                continue;
            }
        }
        println!("{name:<24} {}", entry.description);
        shown += 1;
    }
    if shown == 0 {
        println!("nothing matched");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_urls_are_distinguished_from_registry_names() {
        assert!(is_git_url("https://github.com/a/b"));
        assert!(is_git_url("git@github.com:a/b.git"));
        assert!(is_git_url("ssh://git@host/a/b"));
        assert!(!is_git_url("statusbar"));
        assert!(!is_git_url("my-theme"));
    }

    #[test]
    fn names_are_derived_from_the_last_url_segment() {
        assert_eq!(
            name_from_url("https://github.com/a/cool-plugin").unwrap(),
            "cool-plugin"
        );
        assert_eq!(
            name_from_url("https://github.com/a/cool-plugin.git").unwrap(),
            "cool-plugin"
        );
        assert_eq!(
            name_from_url("https://github.com/a/cool-plugin/").unwrap(),
            "cool-plugin"
        );
        assert_eq!(
            name_from_url("git@github.com:a/thing.git").unwrap(),
            "thing"
        );
    }

    #[test]
    fn a_derived_name_is_always_a_single_safe_component() {
        // The invariant that matters: whatever comes back must be one path
        // component that cannot escape the install root when joined to it.
        //
        // Note that traversal *earlier* in the URL is harmless, because only the
        // last segment is used: `https://host/a/../../etc` yields "etc", and
        // `<root>/etc` is inside the root. An earlier version of this test asserted
        // such URLs were rejected, which was the wrong property.
        let root = Path::new("/data/plugins");
        for url in [
            "https://host/a/../../etc",
            "https://github.com/user/repo.git",
            "git@github.com:user/repo",
            "https://host/a/b_c-1.2",
        ] {
            let name = name_from_url(url).unwrap_or_else(|e| panic!("`{url}`: {e}"));
            assert!(
                !name.contains('/') && !name.contains('\\') && name != "..",
                "`{url}` produced `{name}`"
            );
            assert!(
                root.join(&name).starts_with(root),
                "`{name}` escapes the install root"
            );
        }
    }

    #[test]
    fn names_that_are_not_usable_directories_are_rejected() {
        for bad in [
            "https://host/a/..",
            "https://host/a/.hidden",
            "https://host/a/we;ird",
            "https://host/a/sp ace",
            "https://host/a/quo\"te",
        ] {
            assert!(
                name_from_url(bad).is_err(),
                "`{bad}` should be rejected, got {:?}",
                name_from_url(bad)
            );
        }
    }

    #[test]
    fn install_roots_are_under_the_data_directory() {
        let paths = Paths::new(PathBuf::from("/cfg"), PathBuf::from("/data"));
        assert_eq!(
            Kind::Plugin.install_root(&paths),
            PathBuf::from("/data/plugins")
        );
        assert_eq!(
            Kind::Theme.install_root(&paths),
            PathBuf::from("/data/themes")
        );
    }

    #[test]
    fn removing_a_name_with_a_path_separator_is_refused() {
        let paths = Paths::new(PathBuf::from("/cfg"), PathBuf::from("/data"));
        for bad in ["../../etc/passwd", "sub/dir", "", ".hidden", "a\\b"] {
            let err = remove(&paths, Kind::Plugin, bad).unwrap_err();
            assert!(
                err.to_string().contains("not a valid"),
                "`{bad}` gave: {err}"
            );
        }
    }

    #[test]
    fn removing_something_absent_says_so_rather_than_succeeding() {
        let paths = Paths::new(
            PathBuf::from("/nonexistent/cfg"),
            PathBuf::from("/nonexistent/data"),
        );
        let err = remove(&paths, Kind::Plugin, "ghost").unwrap_err();
        assert!(err.to_string().contains("not installed"), "{err}");
    }

    #[test]
    fn a_registry_index_parses() {
        let index: RegistryIndex = toml::from_str(
            r#"
[plugins.statusbar]
url = "https://github.com/tuzminal/statusbar"
description = "A configurable status bar"

[themes.dracula]
url = "https://github.com/tuzminal/theme-dracula"
"#,
        )
        .unwrap();

        assert_eq!(
            index.plugins["statusbar"].url,
            "https://github.com/tuzminal/statusbar"
        );
        assert!(index.plugins["statusbar"]
            .description
            .contains("status bar"));
        // A missing description is allowed.
        assert!(index.themes["dracula"].description.is_empty());
    }

    #[test]
    fn an_empty_registry_index_is_valid() {
        let index: RegistryIndex = toml::from_str("").unwrap();
        assert!(index.plugins.is_empty() && index.themes.is_empty());
    }

    #[test]
    fn a_missing_registry_tells_the_user_how_to_fix_it() {
        let paths = Paths::new(
            PathBuf::from("/nonexistent/cfg"),
            PathBuf::from("/nonexistent/data"),
        );
        let err = resolve_from_registry(&paths, Kind::Plugin, "whatever").unwrap_err();
        let text = format!("{err:#}");
        assert!(
            text.contains("registry update"),
            "the error should name the fix: {text}"
        );
    }
}
