//! Cross-crate consistency checks.
//!
//! `tuz-config` owns the default keymap as strings; `tuz-input` owns the action
//! and chord grammar. Neither depends on the other, which keeps both simple but
//! means a default binding could name an action that does not exist and nothing
//! would notice until a user pressed the key. These tests are the seam that
//! catches it.

use std::collections::HashSet;
use tuz_config::{Config, DEFAULT_KEYS};
use tuz_input::{Action, KeyChord, Keymap};

#[test]
fn every_default_binding_parses_and_names_a_real_action() {
    let built = Keymap::from_config(DEFAULT_KEYS.iter().map(|(k, v)| (*k, *v)), &HashSet::new());

    assert!(
        built.errors.is_empty(),
        "the shipped default keymap is broken: {:?}",
        built.errors
    );
    assert_eq!(
        built.keymap.len(),
        DEFAULT_KEYS.len(),
        "some default bindings collided and were silently overwritten"
    );
}

#[test]
fn default_chords_survive_a_display_round_trip() {
    // If a default cannot be printed and re-parsed, `--list-keys` output is not
    // valid config, and copying a line out of it would fail.
    for (chord_str, _) in DEFAULT_KEYS {
        let parsed = KeyChord::parse(chord_str)
            .unwrap_or_else(|e| panic!("default chord `{chord_str}` does not parse: {e}"));
        let printed = parsed.to_string();
        let reparsed = KeyChord::parse(&printed)
            .unwrap_or_else(|e| panic!("`{printed}` (from `{chord_str}`) does not re-parse: {e}"));
        assert_eq!(parsed, reparsed, "`{chord_str}` printed as `{printed}`");
    }
}

#[test]
fn the_effective_keymap_of_a_default_config_is_usable() {
    let config = Config::default();
    let keys = config.effective_keys();

    let built = Keymap::from_config(
        keys.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        &HashSet::new(),
    );
    assert!(built.errors.is_empty(), "{:?}", built.errors);

    // Spot-check that the bindings a user would reach for first are present.
    for (chord, expected) in [
        ("ctrl+shift+d", Action::SplitRight),
        ("ctrl+shift+t", Action::NewTab),
        ("ctrl+shift+c", Action::Copy),
        ("ctrl+shift+v", Action::Paste),
    ] {
        let c = KeyChord::parse(chord).unwrap();
        assert_eq!(built.keymap.lookup(&c), Some(&expected), "missing {chord}");
    }
}

#[test]
fn a_user_can_unbind_a_default_through_config() {
    // End-to-end for the "none" escape hatch, across both crates.
    let config: Config = toml::from_str(
        r#"
        [keys]
        "ctrl+shift+t" = "none"
        "#,
    )
    .expect("config should parse");

    let keys = config.effective_keys();
    let built = Keymap::from_config(
        keys.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        &HashSet::new(),
    );

    assert!(built.errors.is_empty(), "{:?}", built.errors);
    assert!(
        built
            .keymap
            .lookup(&KeyChord::parse("ctrl+shift+t").unwrap())
            .is_none(),
        "`none` should have removed the default binding"
    );
    // And it removed only that one.
    assert_eq!(built.keymap.len(), DEFAULT_KEYS.len() - 1);
}

#[test]
fn the_example_config_documents_only_real_actions() {
    // The example file is a user's first contact with the config format; every
    // action it mentions must actually exist.
    let config: Config =
        toml::from_str(tuz_config::EXAMPLE_CONFIG).expect("example config must parse");

    for (chord, action) in &config.keys {
        assert!(
            Action::parse(action).is_some(),
            "config.example.toml binds `{chord}` to `{action}`, which is not an action"
        );
    }
}
