//! The plugins page: what is installed, what is on, and moving plugins in and out.
//!
//! Lists what is **on disk**, not what is loaded. A disabled plugin is never loaded,
//! so a page built from the running host could show you how to turn things off and
//! never how to turn them back on.
//!
//! Import and export are deliberately plain directory copies rather than anything
//! involving the network. `tuzminal plugin install <git-url>` already exists for
//! fetching; what was missing was a way to move a plugin you wrote yourself into
//! place, and a way to get one back out to share it.

use std::path::{Path, PathBuf};
use tuz_config::Config;
use tuz_plugin_api::Manifest;
use tuz_ui::{Ui, Widget, WidgetId};

pub mod ids {
    use tuz_ui::WidgetId;

    /// Import and export controls. Plugin toggles start well above these so a long
    /// list can never collide with them.
    pub const IMPORT: WidgetId = WidgetId(2);
    pub const EXPORT: WidgetId = WidgetId(4);
    pub const CLOSE: WidgetId = WidgetId(9);
    pub const PLUGIN_BASE: u32 = 100;
}

/// One plugin found on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub manifest: Manifest,
    pub directory: PathBuf,
    /// Why it is not running, when it is not. `None` means it loaded.
    pub problem: Option<String>,
}

/// What the app should do after an action on this page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginsOutcome {
    Continue,
    /// Open the system folder chooser, then act on what comes back.
    ChooseFolder(crate::app::FolderPurpose),
    /// The enabled set changed: persist it and reload the host.
    Toggled,
    Close,
}

pub struct PluginsPage {
    pub ui: Ui,
    found: Vec<Installed>,
    /// Where an import lands: the user's own plugin directory, so an imported plugin
    /// shadows an installed one of the same name rather than fighting it.
    install_dir: PathBuf,
    /// Last thing that happened, shown on the page rather than only as a toast so it
    /// stays readable while you decide what to do next.
    message: Option<String>,
}

impl PluginsPage {
    pub fn open(found: Vec<Installed>, install_dir: PathBuf) -> Self {
        Self {
            ui: Ui::new(),
            found,
            install_dir,
            message: None,
        }
    }

    pub fn refresh(&mut self, found: Vec<Installed>) {
        self.found = found;
    }

    /// The last thing that happened, for tests and for a caller that wants to toast
    /// it as well as show it on the page.
    #[allow(dead_code)]
    /// Carry out the action the folder was chosen for.
    ///
    /// Returns whether the installed set changed, so the caller knows to reload the
    /// host. Every outcome, including failure, is reported on the page — a dialog
    /// that closes and does nothing visible is indistinguishable from a crash.
    pub fn folder_chosen(&mut self, purpose: crate::app::FolderPurpose, path: PathBuf) -> bool {
        match purpose {
            crate::app::FolderPurpose::ImportPlugin => {
                let install_dir = self.install_dir.clone();
                match import_plugin(&path, &install_dir) {
                    Ok(name) => {
                        self.message = Some(format!("imported {name}"));
                        true
                    }
                    Err(e) => {
                        self.message = Some(e);
                        false
                    }
                }
            }
            crate::app::FolderPurpose::ExportPlugins => {
                self.message = Some(match export_all(&self.found, &path) {
                    Ok(0) => "everything was already there".to_owned(),
                    Ok(n) => format!("exported {n} plugin(s) to {}", path.display()),
                    Err(e) => e,
                });
                false
            }
        }
    }

    pub fn widgets(&self, config: &Config) -> Vec<Widget> {
        let mut out = vec![Widget::heading("Installed")];

        if self.found.is_empty() {
            out.push(Widget::label(
                "No plugins found. Import one below, or write one — see docs/PLUGINS.md.",
            ));
        }

        for (i, plugin) in self.found.iter().enumerate() {
            let name = &plugin.manifest.name;
            let enabled = is_enabled(config, name);

            // Runtime and version on the label rather than in a second column: the
            // toggle already owns the right-hand side of the row.
            let label = match &plugin.problem {
                Some(problem) => format!("{name}  {}  ({problem})", plugin.manifest.version),
                None => format!(
                    "{name}  {}  {}",
                    plugin.manifest.version,
                    runtime_name(&plugin.manifest)
                ),
            };
            out.push(Widget::toggle(
                WidgetId(ids::PLUGIN_BASE + i as u32),
                label,
                enabled,
            ));
        }

        out.push(Widget::heading(""));
        out.push(Widget::label(
            "Import copies a plugin folder in. Export copies them all out.",
        ));
        out.push(Widget::label(
            "Both open a folder chooser; installing from git is `tuzminal plugin install <url>`.",
        ));

        if let Some(message) = &self.message {
            out.push(Widget::heading(""));
            out.push(Widget::label(message.clone()));
        }

        out
    }

    /// The actions, in the pinned bar rather than inline with the fields they act on.
    ///
    /// A button sitting under its own text field scrolls away with it; in the footer
    /// it is reachable wherever the list happens to be scrolled, which is the whole
    /// reason the footer exists.
    pub fn footer_widgets(&self) -> Vec<Widget> {
        vec![
            Widget::button(ids::IMPORT, "Import"),
            Widget::button(ids::EXPORT, "Export all"),
            Widget::button(ids::CLOSE, "Close"),
        ]
    }

    /// Apply an action, mutating `config` where the action changes a setting.
    pub fn apply(&mut self, action: tuz_ui::UiAction, config: &mut Config) -> PluginsOutcome {
        use tuz_ui::UiAction as A;

        match action {
            A::Pressed(ids::CLOSE) => PluginsOutcome::Close,

            // Both buttons ask for a folder first. The answer comes back through
            // `folder_chosen`, because the dialog runs on its own thread.
            A::Pressed(ids::IMPORT) => {
                PluginsOutcome::ChooseFolder(crate::app::FolderPurpose::ImportPlugin)
            }
            A::Pressed(ids::EXPORT) => {
                PluginsOutcome::ChooseFolder(crate::app::FolderPurpose::ExportPlugins)
            }

            A::Toggled(id, on) if id.0 >= ids::PLUGIN_BASE => {
                let index = (id.0 - ids::PLUGIN_BASE) as usize;
                let Some(plugin) = self.found.get(index) else {
                    return PluginsOutcome::Continue;
                };
                set_enabled(config, &plugin.manifest.name, on);
                self.message = Some(format!(
                    "{} {}",
                    plugin.manifest.name,
                    if on { "enabled" } else { "disabled" }
                ));
                PluginsOutcome::Toggled
            }

            _ => PluginsOutcome::Continue,
        }
    }
}

fn runtime_name(manifest: &Manifest) -> &'static str {
    match manifest.runtime {
        tuz_plugin_api::Runtime::Lua => "lua",
        tuz_plugin_api::Runtime::Wasm => "wasm",
    }
}

/// Whether `name` is enabled, given the two lists that decide it.
///
/// `load` is an allowlist when non-empty and `disable` always wins — the same rule
/// `Host::load_all` applies, restated here because the page must agree with it or the
/// toggles would show a state the loader disagrees with.
pub fn is_enabled(config: &Config, name: &str) -> bool {
    if config.plugins.disable.iter().any(|n| n == name) {
        return false;
    }
    config.plugins.load.is_empty() || config.plugins.load.iter().any(|n| n == name)
}

/// Turn `name` on or off, editing whichever list is in play.
pub fn set_enabled(config: &mut Config, name: &str, on: bool) {
    config.plugins.disable.retain(|n| n != name);

    if on {
        // With an allowlist in force, enabling means joining it.
        if !config.plugins.load.is_empty() && !config.plugins.load.iter().any(|n| n == name) {
            config.plugins.load.push(name.to_owned());
        }
    } else if config.plugins.load.is_empty() {
        config.plugins.disable.push(name.to_owned());
    } else {
        // Removing from the allowlist is enough, and leaves the config smaller than
        // listing it in both places would.
        config.plugins.load.retain(|n| n != name);
    }
}

/// Copy a plugin folder into `into`, validating it first.
///
/// Returns the plugin's name, or a message fit to show the user.
pub fn import_plugin(source: &Path, into: &Path) -> Result<String, String> {
    let manifest_path = source.join("plugin.toml");
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|_| format!("no plugin.toml in {}", source.display()))?;
    let manifest = Manifest::parse(&text).map_err(|e| format!("bad plugin.toml: {e}"))?;

    if !source.join(&manifest.entry).is_file() {
        return Err(format!("entry file `{}` is missing", manifest.entry));
    }

    let target = into.join(&manifest.name);
    if target.exists() {
        return Err(format!("{} is already installed", manifest.name));
    }
    copy_dir(source, &target).map_err(|e| format!("could not copy: {e}"))?;
    Ok(manifest.name)
}

/// Copy every installed plugin into `target`, skipping ones already there.
pub fn export_all(found: &[Installed], target: &Path) -> Result<usize, String> {
    if target.as_os_str().is_empty() {
        return Err("give a folder to export into".to_owned());
    }
    std::fs::create_dir_all(target).map_err(|e| format!("could not create {target:?}: {e}"))?;

    let mut count = 0;
    for plugin in found {
        let into = target.join(&plugin.manifest.name);
        if into.exists() {
            continue;
        }
        copy_dir(&plugin.directory, &into)
            .map_err(|e| format!("could not export {}: {e}", plugin.manifest.name))?;
        count += 1;
    }
    Ok(count)
}

/// Recursively copy a directory.
///
/// Hand-rolled rather than shelling out to `cp`: a plugin folder is small, and
/// spawning a process to move files would be a new failure mode for no gain.
/// Symlinks are followed rather than recreated, so an exported plugin is
/// self-contained instead of pointing back at a folder the recipient does not have.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    impl PluginsPage {
        /// The last thing that happened, for assertions. The page shows it itself,
        /// so nothing outside the tests needs to read it.
        fn last_message(&self) -> Option<&str> {
            self.message.as_deref()
        }
    }

    fn manifest(name: &str) -> Manifest {
        Manifest {
            name: name.to_owned(),
            version: "0.1.0".to_owned(),
            api_version: tuz_plugin_api::API_VERSION,
            runtime: tuz_plugin_api::Runtime::Lua,
            entry: "init.lua".to_owned(),
            description: String::new(),
            author: String::new(),
            homepage: String::new(),
            permissions: Vec::new(),
            events: Vec::new(),
            config: Default::default(),
        }
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("tuz-plugins-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_plugin(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            format!(
                "name = \"{name}\"\nversion = \"0.1.0\"\napi_version = {}\n\
                 runtime = \"lua\"\nentry = \"init.lua\"\n",
                tuz_plugin_api::API_VERSION
            ),
        )
        .unwrap();
        std::fs::write(dir.join("init.lua"), b"return {}").unwrap();
    }

    // --- the enable/disable rule --------------------------------------------

    #[test]
    fn a_plugin_is_enabled_unless_it_is_disabled() {
        let mut config = Config::default();
        assert!(is_enabled(&config, "anything"));

        config.plugins.disable.push("noisy".to_owned());
        assert!(!is_enabled(&config, "noisy"));
        assert!(is_enabled(&config, "other"));
    }

    #[test]
    fn an_allowlist_excludes_everything_not_on_it() {
        // The rule `Host::load_all` applies. If the page disagreed, its toggles would
        // show a state the loader does not honour.
        let mut config = Config::default();
        config.plugins.load.push("wanted".to_owned());
        assert!(is_enabled(&config, "wanted"));
        assert!(!is_enabled(&config, "unwanted"));
    }

    #[test]
    fn disable_wins_over_the_allowlist() {
        let mut config = Config::default();
        config.plugins.load.push("both".to_owned());
        config.plugins.disable.push("both".to_owned());
        assert!(!is_enabled(&config, "both"));
    }

    #[test]
    fn toggling_off_and_on_returns_to_the_starting_config() {
        let config = Config::default();
        let mut next = config.clone();
        set_enabled(&mut next, "thing", false);
        assert!(!is_enabled(&next, "thing"));

        set_enabled(&mut next, "thing", true);
        assert!(is_enabled(&next, "thing"));
        assert_eq!(next.plugins, config.plugins, "no residue left behind");
    }

    #[test]
    fn toggling_under_an_allowlist_edits_the_allowlist() {
        let mut config = Config::default();
        config.plugins.load = vec!["a".to_owned(), "b".to_owned()];

        set_enabled(&mut config, "a", false);
        assert_eq!(config.plugins.load, vec!["b".to_owned()]);
        // Listing it in both places would say the same thing twice.
        assert!(config.plugins.disable.is_empty());

        set_enabled(&mut config, "a", true);
        assert!(is_enabled(&config, "a"));
    }

    // --- import / export -----------------------------------------------------

    #[test]
    fn importing_copies_the_folder_and_reports_the_name() {
        let tmp = TempDir::new("import");
        let source = tmp.0.join("src/my-plugin");
        write_plugin(&source, "my-plugin");
        let into = tmp.0.join("plugins");
        std::fs::create_dir_all(&into).unwrap();

        assert_eq!(import_plugin(&source, &into), Ok("my-plugin".to_owned()));
        assert!(into.join("my-plugin/plugin.toml").is_file());
        assert!(into.join("my-plugin/init.lua").is_file());
    }

    #[test]
    fn importing_something_that_is_not_a_plugin_says_so() {
        let tmp = TempDir::new("notaplugin");
        let source = tmp.0.join("empty");
        std::fs::create_dir_all(&source).unwrap();

        let err = import_plugin(&source, &tmp.0).unwrap_err();
        assert!(err.contains("plugin.toml"), "{err}");
    }

    #[test]
    fn importing_a_plugin_whose_entry_is_missing_is_refused() {
        // Copying it would put a permanently broken plugin on disk that reports its
        // failure only at the next launch.
        let tmp = TempDir::new("noentry");
        let source = tmp.0.join("broken");
        write_plugin(&source, "broken");
        std::fs::remove_file(source.join("init.lua")).unwrap();

        let err = import_plugin(&source, &tmp.0).unwrap_err();
        assert!(err.contains("entry"), "{err}");
    }

    #[test]
    fn importing_over_an_existing_plugin_is_refused_rather_than_merged() {
        let tmp = TempDir::new("clash");
        let source = tmp.0.join("src/dup");
        write_plugin(&source, "dup");
        let into = tmp.0.join("plugins");
        write_plugin(&into.join("dup"), "dup");

        let err = import_plugin(&source, &into).unwrap_err();
        assert!(err.contains("already installed"), "{err}");
    }

    #[test]
    fn exporting_copies_every_plugin_and_counts_them() {
        let tmp = TempDir::new("export");
        let mut found = Vec::new();
        for name in ["one", "two"] {
            let dir = tmp.0.join("installed").join(name);
            write_plugin(&dir, name);
            found.push(Installed {
                manifest: manifest(name),
                directory: dir,
                problem: None,
            });
        }

        let target = tmp.0.join("backup");
        assert_eq!(export_all(&found, &target), Ok(2));
        assert!(target.join("one/init.lua").is_file());
        assert!(target.join("two/init.lua").is_file());

        // Exporting again skips what is already there rather than failing.
        assert_eq!(export_all(&found, &target), Ok(0));
    }

    #[test]
    fn exporting_without_a_destination_says_so() {
        assert!(export_all(&[], Path::new("")).is_err());
    }

    #[test]
    fn nested_folders_survive_a_copy() {
        // A WASM plugin ships a subdirectory of assets; a flat copy would lose it.
        let tmp = TempDir::new("nested");
        let source = tmp.0.join("deep");
        write_plugin(&source, "deep");
        std::fs::create_dir_all(source.join("assets/icons")).unwrap();
        std::fs::write(source.join("assets/icons/a.txt"), b"x").unwrap();

        let into = tmp.0.join("out");
        std::fs::create_dir_all(&into).unwrap();
        import_plugin(&source, &into).unwrap();
        assert!(into.join("deep/assets/icons/a.txt").is_file());
    }

    // --- the page ------------------------------------------------------------

    #[test]
    fn every_installed_plugin_gets_a_toggle_reflecting_the_config() {
        let mut config = Config::default();
        config.plugins.disable.push("off".to_owned());

        let page = PluginsPage::open(
            vec![
                Installed {
                    manifest: manifest("on"),
                    directory: PathBuf::from("/tmp/on"),
                    problem: None,
                },
                Installed {
                    manifest: manifest("off"),
                    directory: PathBuf::from("/tmp/off"),
                    problem: None,
                },
            ],
            PathBuf::from("/tmp"),
        );

        let toggles: Vec<(String, bool)> = page
            .widgets(&config)
            .into_iter()
            .filter_map(|w| match w {
                Widget::Toggle { label, on, .. } => Some((label, on)),
                _ => None,
            })
            .collect();

        assert_eq!(toggles.len(), 2);
        assert!(toggles[0].0.starts_with("on") && toggles[0].1);
        assert!(toggles[1].0.starts_with("off") && !toggles[1].1);
    }

    #[test]
    fn an_empty_list_explains_itself_rather_than_showing_nothing() {
        let page = PluginsPage::open(Vec::new(), PathBuf::from("/tmp"));
        let widgets = page.widgets(&Config::default());
        assert!(widgets.iter().any(|w| matches!(
            w,
            Widget::Label { text, .. } if text.contains("No plugins")
        )));
    }

    #[test]
    fn toggling_a_row_updates_the_config_and_asks_for_a_reload() {
        let mut config = Config::default();
        let mut page = PluginsPage::open(
            vec![Installed {
                manifest: manifest("thing"),
                directory: PathBuf::from("/tmp/thing"),
                problem: None,
            }],
            PathBuf::from("/tmp"),
        );

        let outcome = page.apply(
            tuz_ui::UiAction::Toggled(WidgetId(ids::PLUGIN_BASE), false),
            &mut config,
        );
        assert_eq!(outcome, PluginsOutcome::Toggled);
        assert!(!is_enabled(&config, "thing"));
        assert!(page.last_message().is_some_and(|m| m.contains("disabled")));
    }

    #[test]
    fn the_buttons_ask_for_a_folder_rather_than_acting_blind() {
        use crate::app::FolderPurpose;
        let mut config = Config::default();
        let mut page = PluginsPage::open(Vec::new(), PathBuf::from("/tmp"));

        assert_eq!(
            page.apply(tuz_ui::UiAction::Pressed(ids::IMPORT), &mut config),
            PluginsOutcome::ChooseFolder(FolderPurpose::ImportPlugin)
        );
        assert_eq!(
            page.apply(tuz_ui::UiAction::Pressed(ids::EXPORT), &mut config),
            PluginsOutcome::ChooseFolder(FolderPurpose::ExportPlugins)
        );
    }

    #[test]
    fn choosing_a_folder_imports_it_and_reports_what_happened() {
        use crate::app::FolderPurpose;
        let tmp = TempDir::new("chosen");
        let source = tmp.0.join("src/picked");
        write_plugin(&source, "picked");
        let install = tmp.0.join("plugins");
        std::fs::create_dir_all(&install).unwrap();

        let mut page = PluginsPage::open(Vec::new(), install.clone());
        assert!(page.folder_chosen(FolderPurpose::ImportPlugin, source));
        assert!(install.join("picked/init.lua").is_file());
        assert!(page.last_message().is_some_and(|m| m.contains("picked")));
    }

    #[test]
    fn a_failed_import_says_so_rather_than_closing_silently() {
        // A dialog that closes and does nothing visible is indistinguishable from a
        // crash, so every outcome has to reach the page.
        use crate::app::FolderPurpose;
        let tmp = TempDir::new("badpick");
        let empty = tmp.0.join("empty");
        std::fs::create_dir_all(&empty).unwrap();

        let mut page = PluginsPage::open(Vec::new(), tmp.0.clone());
        assert!(!page.folder_chosen(FolderPurpose::ImportPlugin, empty));
        assert!(page
            .last_message()
            .is_some_and(|m| m.contains("plugin.toml")));
    }

    #[test]
    fn a_toggle_for_a_row_that_no_longer_exists_does_nothing() {
        // The list is rebuilt on import; a stale click must not index off the end.
        let mut config = Config::default();
        let mut page = PluginsPage::open(Vec::new(), PathBuf::from("/tmp"));
        assert_eq!(
            page.apply(
                tuz_ui::UiAction::Toggled(WidgetId(ids::PLUGIN_BASE + 7), false),
                &mut config
            ),
            PluginsOutcome::Continue
        );
    }
}
