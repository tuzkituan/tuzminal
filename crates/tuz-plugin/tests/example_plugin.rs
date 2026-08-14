//! Loads the example plugin that ships in `examples/`.
//!
//! The plugin API had unit tests and no users: nothing in the repository had ever
//! been loaded end to end, which is how an extension API stays green while quietly
//! becoming unusable. This test is the missing user. If the manifest format, the
//! handler names, or the `ctx` surface change, the example stops working here rather
//! than in someone's terminal.

use std::path::PathBuf;
use tuz_plugin::Host;
use tuz_plugin_api::Event;

fn examples_dir() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `crates/tuz-plugin`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .canonicalize()
        .expect("the examples directory should exist")
}

#[test]
fn the_shipped_example_is_discovered_and_loads() {
    let dirs = vec![examples_dir()];

    let found = tuz_plugin::discover(&dirs);
    let names: Vec<String> = found
        .iter()
        .filter_map(|f| f.as_ref().ok())
        .map(|(_, m)| m.name.clone())
        .collect();
    for expected in ["clock", "open-in-ide"] {
        assert!(names.contains(&expected.to_owned()), "found {names:?}");
    }

    let mut host = Host::disabled();
    let errors = host.load_all(&dirs, &tuz_config::Plugins::default());
    assert!(errors.is_empty(), "an example failed to load: {errors:?}");
    assert_eq!(host.plugins().len(), names.len());
}

#[test]
fn the_example_registers_its_command_and_keybind_at_startup() {
    // `load_all` dispatches `Startup` and applies the registrations, so by the time
    // it returns the binding must already exist — the keymap is built right after.
    let mut host = Host::disabled();
    host.load_all(&[examples_dir()], &tuz_config::Plugins::default());

    let binds = host.keybinds();
    assert_eq!(
        binds.get("ctrl+shift+m").map(String::as_str),
        Some("clock.hello"),
        "got {binds:?}"
    );
    assert!(host.command_names().iter().any(|c| c == "clock.hello"));
}

#[test]
fn the_example_produces_a_status_segment() {
    let mut host = Host::disabled();
    host.load_all(&[examples_dir()], &tuz_config::Plugins::default());

    host.dispatch(&Event::StatusBarRender);
    let segments = host.status_segments();

    // `os.date("%H:%M")` — the shape matters, the value cannot.
    assert!(
        segments
            .iter()
            .any(|s| s.text.len() == 5 && s.text.contains(':')),
        "no clock segment in {segments:?}"
    );
}

/// The capability that had to be added for the editor buttons to stop being a
/// built-in feature: a status segment you can press.
#[test]
fn a_clickable_segment_reaches_the_plugin_that_published_it() {
    let mut host = Host::disabled();
    host.load_all(&[examples_dir()], &tuz_config::Plugins::default());
    host.dispatch(&Event::StatusBarRender);

    // The clock's segment carries no id and must not be clickable; the editor
    // buttons carry one, qualified by the host with the plugin's name.
    let owned = host.status_segments_with_owner();
    assert!(
        owned
            .iter()
            .any(|(s, owner)| s.text.contains(':') && owner.is_none()),
        "a clock should not be a button"
    );

    let Some((_, Some(id))) = owned.iter().find(|(_, o)| o.is_some()) else {
        // No editor installed on this machine, so there is no button to press.
        eprintln!("skipping: no editors detected");
        return;
    };
    assert!(id.starts_with("open-in-ide."), "got {id}");

    let commands = host.click_status_segment(id);
    assert!(
        commands.iter().any(|c| matches!(
            c,
            tuz_plugin_api::Command::SendText { text, .. } if text.ends_with(" .")
        )),
        "a press should open the shell's own directory, got {commands:?}"
    );
}

#[test]
fn the_example_sends_text_when_its_command_runs() {
    let mut host = Host::disabled();
    host.load_all(&[examples_dir()], &tuz_config::Plugins::default());

    let commands = host.dispatch(&Event::Command {
        name: "hello".to_owned(),
        args: Vec::new(),
    });

    assert!(
        commands.iter().any(|c| matches!(
            c,
            tuz_plugin_api::Command::SendText { text, .. } if text.contains("hello")
        )),
        "got {commands:?}"
    );
}
