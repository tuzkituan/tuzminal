//! Loads the plugins that ship in `plugins/`.
//!
//! The plugin API had unit tests and no users: nothing in the repository had ever
//! been loaded end to end, which is how an extension API stays green while quietly
//! becoming unusable. This test is the missing user. If the manifest format, the
//! handler names, or the `ctx` surface change, the example stops working here rather
//! than in someone's terminal.

use std::path::PathBuf;
use tuz_plugin::Host;
use tuz_plugin_api::{Command, Event, KeyOutcome, KeyPress, PaneId};

fn plugins_dir() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `crates/tuz-plugin`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins")
        .canonicalize()
        .expect("the plugins directory should exist")
}

#[test]
fn the_shipped_example_is_discovered_and_loads() {
    let dirs = vec![plugins_dir()];

    let found = tuz_plugin::discover(&dirs);
    let names: Vec<String> = found
        .iter()
        .filter_map(|f| f.as_ref().ok())
        .map(|(_, m)| m.name.clone())
        .collect();
    for expected in ["clock", "open-in-ide", "suggest"] {
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
    host.load_all(&[plugins_dir()], &tuz_config::Plugins::default());

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
    host.load_all(&[plugins_dir()], &tuz_config::Plugins::default());

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
    host.load_all(&[plugins_dir()], &tuz_config::Plugins::default());
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
    host.load_all(&[plugins_dir()], &tuz_config::Plugins::default());

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

// ---------------------------------------------------------------------------
// `suggest`, the plugin that `input_line` and `set_inline_hint` were added for.
// ---------------------------------------------------------------------------

/// A prompt ending in a marker the plugin recognises as bare.
const PROMPT: &str = "$ ";

/// Deliberately unlike anything in a real history file.
///
/// These tests seed the corpus through the event stream rather than from a fixture,
/// because the plugin reads the developer's own `~/.zsh_history` at startup and a test
/// that asserted on its contents would pass or fail per machine. Distinctive strings
/// keep the real corpus from colliding with the seeded one.
const ALPHA: &str = "zzqq-alpha-beta";

fn loaded() -> Host {
    let mut host = Host::disabled();
    let errors = host.load_all(&[plugins_dir()], &tuz_config::Plugins::default());
    assert!(errors.is_empty(), "a plugin failed to load: {errors:?}");
    host
}

fn input_line(host: &mut Host, text: &str, at_line_end: bool) -> Vec<Command> {
    host.dispatch(&Event::InputLine {
        pane: PaneId(1),
        line: text.to_owned(),
        cursor_col: text.chars().count() as u16,
        at_line_end,
    })
}

fn press(host: &mut Host, chord: &str) -> (KeyOutcome, Vec<Command>) {
    host.on_key(&Event::Key(KeyPress {
        chord: chord.to_owned(),
        modifiers: Default::default(),
    }))
}

/// The last hint published, if any. `None` means the plugin said nothing at all, which
/// is different from publishing an empty hint to withdraw one.
fn hint(commands: &[Command]) -> Option<&str> {
    commands.iter().rev().find_map(|c| match c {
        Command::SetInlineHint { text, .. } => Some(text.as_str()),
        _ => None,
    })
}

/// Type `command` at a bare prompt and submit it, so the session corpus learns it.
fn teach(host: &mut Host, command: &str) {
    input_line(host, PROMPT, true);
    input_line(host, &format!("{PROMPT}{command}"), true);
    press(host, "enter");
}

#[test]
fn suggest_asks_for_the_input_line_but_never_for_all_output() {
    // The double opt-in means a manifest that forgot the permission gets nothing and no
    // error, so assert the grant actually took. And assert the firehose stayed off:
    // `read-input` must not be a back door to `pane_output`.
    let host = loaded();
    let manifest = host
        .plugins()
        .iter()
        .map(|p| &p.manifest)
        .find(|m| m.name == "suggest")
        .expect("suggest should be loaded");

    assert!(manifest.wants_event("input_line"));
    assert!(manifest.wants_event("key"));
    assert!(manifest.wants_event("tab_switch"));
    assert!(manifest.wants_event("pane_closed"));
    assert!(
        !manifest.wants_event("pane_output"),
        "suggest must not receive every byte of output"
    );
}

#[test]
fn a_prefix_typed_after_a_prompt_is_completed_from_history() {
    let mut host = loaded();
    teach(&mut host, ALPHA);

    input_line(&mut host, PROMPT, true);
    let commands = input_line(&mut host, &format!("{PROMPT}zzqq-al"), true);

    assert_eq!(hint(&commands), Some("pha-beta"));
}

#[test]
fn a_bare_prompt_on_its_own_suggests_nothing() {
    // The state a terminal spends most of its time in. A suggestion here would be a
    // guess with nothing to go on, and with the fallback matcher it is the case most
    // likely to produce one — so it is worth asserting it does not.
    let mut host = loaded();
    teach(&mut host, ALPHA);

    let commands = input_line(&mut host, PROMPT, true);
    assert!(
        hint(&commands).unwrap_or("").is_empty(),
        "got {:?}",
        hint(&commands)
    );
}

#[test]
fn a_prompt_containing_a_marker_does_not_truncate_the_typed_text() {
    // The test that fails if the prompt is split on `$ `/`> ` instead of out-lived:
    // `>` is a redirection, and the typed text runs straight through it.
    let mut host = loaded();
    teach(&mut host, "echo zzqq > out-alpha");

    input_line(&mut host, PROMPT, true);
    let commands = input_line(&mut host, &format!("{PROMPT}echo zzqq > out-al"), true);

    assert_eq!(hint(&commands), Some("pha"));
}

#[test]
fn right_at_the_end_of_the_line_is_swallowed_and_types_the_rest() {
    let mut host = loaded();
    teach(&mut host, ALPHA);
    input_line(&mut host, PROMPT, true);
    input_line(&mut host, &format!("{PROMPT}zzqq-al"), true);

    let (outcome, commands) = press(&mut host, "right");

    assert_eq!(outcome, KeyOutcome::Handled);
    assert!(
        commands.iter().any(|c| matches!(
            c,
            Command::SendText { text, .. } if text == "pha-beta"
        )),
        "got {commands:?}"
    );
}

#[test]
fn right_with_no_suggestion_showing_reaches_the_shell() {
    // The bug the user would feel as a broken terminal: a plugin swallowing an arrow
    // key when it had nothing to offer.
    let mut host = loaded();
    input_line(&mut host, PROMPT, true);

    for chord in ["right", "end", "ctrl+e", "alt+right", "ctrl+space"] {
        let (outcome, commands) = press(&mut host, chord);
        assert_eq!(outcome, KeyOutcome::Unhandled, "`{chord}` was swallowed");
        assert!(
            !commands
                .iter()
                .any(|c| matches!(c, Command::SendText { .. })),
            "`{chord}` typed something with no suggestion showing: {commands:?}"
        );
    }
}

#[test]
fn alt_right_takes_one_word_and_leaves_the_rest() {
    let mut host = loaded();
    teach(&mut host, "zzqq-alpha beta gamma");
    input_line(&mut host, PROMPT, true);
    input_line(&mut host, &format!("{PROMPT}zzqq-al"), true);

    let (outcome, commands) = press(&mut host, "alt+right");

    assert_eq!(outcome, KeyOutcome::Handled);
    assert!(
        commands.iter().any(|c| matches!(
            c,
            Command::SendText { text, .. } if text == "pha"
        )),
        "got {commands:?}"
    );
}

#[test]
fn a_cursor_in_the_middle_of_a_line_withdraws_the_suggestion() {
    // Appending to the middle of a command would corrupt it, so the hint has to go —
    // and with it the ability to accept.
    let mut host = loaded();
    teach(&mut host, ALPHA);
    input_line(&mut host, PROMPT, true);
    input_line(&mut host, &format!("{PROMPT}zzqq-al"), true);

    let commands = input_line(&mut host, &format!("{PROMPT}zzqq-a"), false);
    assert_eq!(hint(&commands), Some(""));

    let (outcome, _) = press(&mut host, "right");
    assert_eq!(outcome, KeyOutcome::Unhandled);
}

#[test]
fn a_password_typed_as_an_argument_is_never_learned() {
    // The failure that matters most: a secret in the corpus comes back as ghost text in
    // front of whoever is looking at the screen.
    let mut host = loaded();
    teach(&mut host, "mysql -pzzqqhunter2");

    input_line(&mut host, PROMPT, true);
    let commands = input_line(&mut host, &format!("{PROMPT}mysql -p"), true);

    assert!(
        !hint(&commands).unwrap_or("").contains("zzqqhunter2"),
        "a password reached the screen: {:?}",
        hint(&commands)
    );
}

#[test]
fn a_command_typed_with_a_leading_space_is_never_learned() {
    // The shell's own opt-out, `HISTCONTROL=ignorespace` / `hist_ignore_space`. Users
    // already reach for it before typing a secret.
    let mut host = loaded();
    teach(&mut host, " zzqq-quiet-alpha");

    input_line(&mut host, PROMPT, true);
    let commands = input_line(&mut host, &format!("{PROMPT}zzqq-qu"), true);

    assert!(
        !hint(&commands).unwrap_or("").contains("iet-alpha"),
        "a space-prefixed command was learned: {:?}",
        hint(&commands)
    );
}

#[test]
fn ctrl_r_stops_suggesting_until_the_line_is_finished() {
    // The row becomes the shell's own search UI, which the plugin cannot model. It goes
    // quiet rather than guessing against it.
    let mut host = loaded();
    teach(&mut host, ALPHA);
    input_line(&mut host, PROMPT, true);

    press(&mut host, "ctrl+r");
    let commands = input_line(&mut host, &format!("{PROMPT}zzqq-al"), true);
    // `None` rather than `Some("")` is a pass: nothing had been published yet, so there
    // was nothing to withdraw. The property is that no suggestion is offered.
    assert!(
        hint(&commands).unwrap_or("").is_empty(),
        "suggested while the shell was in its own search UI: {:?}",
        hint(&commands)
    );

    // `ctrl+g` abandons the search, and suggestions come back.
    press(&mut host, "ctrl+g");
    input_line(&mut host, PROMPT, true);
    let commands = input_line(&mut host, &format!("{PROMPT}zzqq-al"), true);
    assert_eq!(hint(&commands), Some("pha-beta"));
}

#[test]
fn switching_tabs_withdraws_the_hint() {
    let mut host = loaded();
    teach(&mut host, ALPHA);
    input_line(&mut host, PROMPT, true);
    input_line(&mut host, &format!("{PROMPT}zzqq-al"), true);

    let commands = host.dispatch(&Event::TabSwitch { index: 1 });
    assert_eq!(hint(&commands), Some(""));

    let (outcome, _) = press(&mut host, "right");
    assert_eq!(outcome, KeyOutcome::Unhandled);
}
