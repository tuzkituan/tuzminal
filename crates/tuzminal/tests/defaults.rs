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

/// The strip must be visible on first launch, because the buttons live in it.
///
/// This shipped broken: the strip hid itself with a single tab to save a row, and
/// since the new-tab, split and settings buttons are all drawn in that strip, a fresh
/// launch had **no visible controls at all** — which is precisely the problem the
/// buttons were added to solve. Keyboard shortcuts still worked, so nothing was
/// broken enough to fail a test; it was just invisible.
#[test]
fn a_default_config_shows_the_tab_strip_so_the_buttons_are_reachable() {
    let config = Config::default();
    assert!(
        config.window.always_show_tab_bar,
        "the strip must default to visible, or a fresh install has no clickable controls"
    );
}

#[test]
fn the_strip_reserves_height_and_places_buttons_for_a_single_tab() {
    use tuz_layout::{CellSize, ChromeButton, Layout, LayoutOptions, Rect};

    // What the app builds for a default config with one tab open.
    let buttons = vec![
        ChromeButton::Settings,
        ChromeButton::SplitDown,
        ChromeButton::SplitRight,
        ChromeButton::NewTab,
    ];
    let opts = LayoutOptions {
        tab_bar_height: 26,
        tab_width: 200,
        min_tab_width: 70,
        buttons: buttons.clone(),
        cell: CellSize {
            width: 10,
            height: 20,
        },
        ..LayoutOptions::default()
    };

    let (layout, _) = Layout::new();
    let frame = layout.compute(Rect::from_size(1000, 600), &opts);

    assert!(frame.tab_bar.height > 0, "the strip must be drawn");
    assert_eq!(
        frame.actions.len(),
        buttons.len(),
        "every button needs a rect, or it cannot be clicked"
    );

    // And each one is inside the strip, so hit-testing can find it.
    for (button, rect) in &frame.actions {
        assert!(
            frame.tab_at(rect.center_x(), rect.center_y()).is_none(),
            "{button:?} overlaps a tab, which would steal its clicks"
        );
        assert_eq!(
            frame.action_at(rect.center_x(), rect.center_y()),
            Some(*button),
            "{button:?} is not hit-testable at its own rect"
        );
    }
}
