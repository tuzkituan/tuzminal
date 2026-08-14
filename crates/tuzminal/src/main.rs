//! Tuzminal — a fast, modular, GPU-accelerated terminal emulator.

mod app;
mod appicon;
mod bundled;
mod explorer;
mod gpu;
mod help;
mod keys;
mod menu;
mod pkg;
mod plugins;
mod proc;
mod settings;
mod shells;
mod status;

use anyhow::{Context, Result};
use clap::Parser;
use tuz_config::{ConfigManager, Paths, Theme};
use tuz_input::Action;

#[derive(Parser, Debug)]
#[command(
    name = "tuzminal",
    version,
    about = "A fast, modular, GPU-accelerated terminal emulator",
    long_about = None
)]
struct Cli {
    /// Write a commented starter config and exit. Never overwrites an existing
    /// file.
    #[arg(long)]
    init_config: bool,

    /// Validate the config and theme, print any problems, and exit.
    #[arg(long)]
    config_check: bool,

    /// List every action that can be bound to a key, and exit.
    #[arg(long)]
    list_actions: bool,

    /// List available themes and exit.
    #[arg(long)]
    list_themes: bool,

    /// Print the resolved keymap and exit.
    #[arg(long)]
    list_keys: bool,

    /// Increase log verbosity. Repeat for more (-v debug, -vv trace).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Option<Sub>,
}

#[derive(clap::Subcommand, Debug)]
enum Sub {
    /// Manage plugins.
    Plugin {
        #[command(subcommand)]
        action: ItemAction,
    },
    /// Manage themes.
    Theme {
        #[command(subcommand)]
        action: ItemAction,
    },
    /// Manage the package registry.
    Registry {
        #[command(subcommand)]
        action: RegistryAction,
    },
}

#[derive(clap::Subcommand, Debug)]
enum ItemAction {
    /// Install from a git URL or a registry name.
    Install {
        source: String,
        /// Skip the confirmation prompt.
        #[arg(short, long)]
        yes: bool,
    },
    /// List what is installed.
    List,
    /// Remove an installed item.
    Remove { name: String },
    /// Update one item, or everything when no name is given.
    Update { name: Option<String> },
    /// Search the registry.
    Search { query: Option<String> },
}

#[derive(clap::Subcommand, Debug)]
enum RegistryAction {
    /// Clone or pull the registry index.
    Update {
        /// Registry to use instead of the default.
        #[arg(long)]
        url: Option<String>,
    },
}

fn run_item_action(paths: &Paths, kind: pkg::Kind, action: ItemAction) -> Result<()> {
    match action {
        ItemAction::Install { source, yes } => pkg::install(paths, kind, &source, yes),
        ItemAction::List => pkg::list(paths, kind),
        ItemAction::Remove { name } => pkg::remove(paths, kind, &name),
        ItemAction::Update { name } => pkg::update(paths, kind, name.as_deref()),
        ItemAction::Search { query } => pkg::search(paths, kind, query.as_deref()),
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    let paths = Paths::discover().context("could not determine configuration directories")?;

    // Written before anything reads the plugin directories, so the bundled plugins are
    // discovered on the very first launch rather than the second. Existing ones are
    // never touched.
    if let Some(dir) = paths.plugin_dirs().first() {
        if std::fs::create_dir_all(dir).is_ok() {
            bundled::install_missing(dir);
        }
    }

    // Subcommand-ish flags all terminate without opening a window.
    if cli.init_config {
        return init_config(paths);
    }
    if cli.list_actions {
        for name in Action::all_names() {
            println!("{name}");
        }
        println!("select_tab_<n>");
        return Ok(());
    }
    if cli.list_themes {
        for name in Theme::available(&paths) {
            println!("{name}");
        }
        return Ok(());
    }
    if cli.config_check {
        return config_check(paths);
    }
    if cli.list_keys {
        return list_keys(paths);
    }

    // Subcommands manage installed content and never open a window.
    if let Some(command) = cli.command {
        return match command {
            Sub::Plugin { action } => run_item_action(&paths, pkg::Kind::Plugin, action),
            Sub::Theme { action } => run_item_action(&paths, pkg::Kind::Theme, action),
            Sub::Registry {
                action: RegistryAction::Update { url },
            } => pkg::update_registry(&paths, url.as_deref()),
        };
    }

    app::App::run(paths)
}

fn init_logging(verbosity: u8) {
    let default = match verbosity {
        0 => "warn,tuzminal=info",
        1 => "info,tuzminal=debug,tuz_config=debug",
        _ => "debug,tuzminal=trace",
    };
    // RUST_LOG still wins, so a user debugging wgpu can ask for it directly.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default)).init();
}

fn init_config(paths: Paths) -> Result<()> {
    let mgr = ConfigManager::load(paths);
    match mgr.write_example_config() {
        Ok(path) => {
            println!("wrote {}", path.display());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Refusing is the correct behavior, so report it plainly and exit
            // non-zero rather than pretending to have written anything.
            anyhow::bail!("{}\nremove or rename it first if you want a fresh copy", e)
        }
        Err(e) => Err(e).context("failed to write the example config"),
    }
}

/// Validate config and theme, printing every problem found.
fn config_check(paths: Paths) -> Result<()> {
    let config_file = paths.config_file();
    let mgr = ConfigManager::load(paths);

    if let Some(err) = mgr.last_error() {
        eprintln!("{}: {}", config_file.display(), err);
        anyhow::bail!("configuration is not valid");
    }

    if !config_file.exists() {
        println!(
            "no config file at {} (using defaults)",
            config_file.display()
        );
    } else {
        println!("{}: ok", config_file.display());
    }
    println!("theme `{}`: ok", mgr.theme().name);

    // Bad keybindings do not stop the terminal from starting, but `--config-check`
    // exists precisely to surface them, so they count as failures here.
    let built = keymap_of(&mgr);
    if built.errors.is_empty() {
        println!("{} keybindings: ok", built.keymap.len());
        Ok(())
    } else {
        for err in &built.errors {
            eprintln!("keybinding: {err}");
        }
        anyhow::bail!("{} keybinding problem(s)", built.errors.len())
    }
}

fn list_keys(paths: Paths) -> Result<()> {
    let mgr = ConfigManager::load(paths);
    let built = keymap_of(&mgr);
    for (chord, action) in built.keymap.iter_sorted() {
        println!("{chord:<24} {action}");
    }
    for err in &built.errors {
        eprintln!("keybinding: {err}");
    }
    Ok(())
}

/// Build the keymap exactly as the running app does.
///
/// Plugins are loaded here too: without them `--list-keys` and `--config-check`
/// would disagree with the terminal about what is bound, and a plugin-registered
/// action would look like an unknown-action error.
fn keymap_of(mgr: &ConfigManager) -> tuz_input::BuiltKeymap {
    let cfg = mgr.config().plugins.clone();
    let mut host = if cfg.enabled {
        let mut host = tuz_plugin::Host::new(
            std::time::Duration::from_millis(cfg.callback_timeout_ms),
            std::time::Duration::from_millis(cfg.key_hook_timeout_ms),
        );
        let dirs: Vec<std::path::PathBuf> = mgr.paths().plugin_dirs().to_vec();
        for error in host.load_all(&dirs, &cfg) {
            eprintln!("plugin: {error}");
        }
        host
    } else {
        tuz_plugin::Host::disabled()
    };

    let mut keys = std::collections::BTreeMap::new();
    for (chord, command) in host.keybinds() {
        keys.insert(chord.clone(), command.clone());
    }
    keys.extend(mgr.config().effective_keys());

    let plugin_actions: std::collections::HashSet<String> =
        host.command_names().into_iter().collect();
    // Silence the unused-mut warning while keeping `host` mutable for load_all.
    let _ = &mut host;

    tuz_input::Keymap::from_config(
        keys.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        &plugin_actions,
    )
}
