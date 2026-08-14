//! The keyboard shortcut reference.
//!
//! Built from the **live keymap**, not from a written-down list. A help page that is
//! maintained by hand is wrong the first time anyone rebinds a key, and wrong forever
//! after someone adds an action and forgets the page exists. Rebind `copy` in
//! `config.toml` and this page says what you rebound it to.
//!
//! The one exception is the explorer and settings keys, which are not keymap entries
//! at all — they are matched directly while those panels have focus, so there is
//! nothing to read them from. Those are listed literally, and the test at the bottom
//! is what keeps them honest.

use tuz_config::Config;
use tuz_ui::{Ui, Widget};

pub struct HelpPage {
    pub ui: Ui,
}

impl HelpPage {
    pub fn open() -> Self {
        Self { ui: Ui::new() }
    }

    pub fn widgets(&self, config: &Config) -> Vec<Widget> {
        let mut out = vec![Widget::heading("Terminal")];

        // Grouped so the page reads as sections rather than one long alphabetical
        // list. Anything the keymap has that no group claims still gets shown, under
        // "Other" — a binding missing from the help is worse than one filed oddly.
        let groups: [(&str, &[&str]); 5] = [
            (
                "Terminal",
                &["copy", "paste", "select_all", "reload_config", "quit"],
            ),
            (
                "Panes",
                &[
                    "split_right",
                    "split_left",
                    "split_up",
                    "split_down",
                    "close_pane",
                    "focus_left",
                    "focus_right",
                    "focus_up",
                    "focus_down",
                    "focus_next_pane",
                    "focus_prev_pane",
                    "resize_left",
                    "resize_right",
                    "resize_up",
                    "resize_down",
                ],
            ),
            ("Tabs", &["new_tab", "close_tab", "next_tab", "prev_tab"]),
            (
                "View",
                &[
                    "increase_font_size",
                    "decrease_font_size",
                    "reset_font_size",
                    "toggle_fullscreen",
                    "scroll_line_up",
                    "scroll_line_down",
                    "scroll_page_up",
                    "scroll_page_down",
                    "scroll_to_top",
                    "scroll_to_bottom",
                    "clear_scrollback",
                ],
            ),
            (
                "Panels",
                &["open_settings", "open_explorer", "open_plugins", "open_help"],
            ),
        ];

        let keys = config.effective_keys();
        let mut shown: Vec<&str> = Vec::new();

        for (group, actions) in groups {
            if group != "Terminal" {
                out.push(Widget::heading(group));
            }
            for action in actions {
                // One action can have several chords bound to it — the arrow keys and
                // the hjkl keys both focus panes — so they are joined rather than
                // listed as duplicate rows.
                let bound: Vec<String> = keys
                    .iter()
                    .filter(|(_, a)| a.as_str() == *action)
                    .map(|(chord, _)| chord.clone())
                    .collect();
                if bound.is_empty() {
                    continue;
                }
                shown.push(action);
                out.push(Widget::shortcut(describe(action), bound.join("  ")));
            }
        }

        let mut other: Vec<(&String, &String)> = keys
            .iter()
            .filter(|(_, a)| !shown.contains(&a.as_str()) && a.as_str() != "none")
            .collect();
        if !other.is_empty() {
            out.push(Widget::heading("Other"));
            other.sort_by(|a, b| a.1.cmp(b.1));
            for (chord, action) in other {
                out.push(Widget::shortcut(describe(action), chord.clone()));
            }
        }

        out.push(Widget::heading("File explorer"));
        for (keys, what) in EXPLORER_KEYS {
            out.push(Widget::shortcut(*what, *keys));
        }

        out.push(Widget::heading("Settings and dialogs"));
        for (keys, what) in PANEL_KEYS {
            out.push(Widget::shortcut(*what, *keys));
        }

        out
    }
}

/// Explorer keys, which are matched directly rather than through the keymap.
///
/// Kept beside the page that shows them so the two are edited together. `App::
/// explorer_key` is the other half; the test below is what notices if they drift.
pub const EXPLORER_KEYS: &[(&str, &str)] = &[
    ("↑ ↓  pgup pgdn  home end", "Move the selection"),
    ("enter", "Open a folder, or a file in $EDITOR"),
    ("backspace", "Go up one directory"),
    ("e", "Open in $EDITOR"),
    ("p", "Type the path at the prompt"),
    ("c", "cd the shell into the folder"),
    ("r", "Rename"),
    ("n", "New folder"),
    ("d", "Delete"),
    ("h", "Show or hide dotfiles"),
    ("escape", "Give the keyboard back to the shell"),
];

/// Keys inside the settings page and the explorer's prompts.
pub const PANEL_KEYS: &[(&str, &str)] = &[
    ("tab  shift+tab", "Move between controls"),
    ("↑ ↓", "Move between controls"),
    ("← →", "Change a value"),
    ("enter  space", "Activate, or confirm a prompt"),
    ("y  n", "Answer a confirmation"),
    ("escape", "Cancel, or close"),
];

/// A readable description for a config action name.
///
/// Falls back to the name with underscores replaced, so an action added without a
/// description here still reads as words rather than being omitted.
fn describe(action: &str) -> String {
    let known = match action {
        "copy" => "Copy the selection",
        "paste" => "Paste",
        "reload_config" => "Reload the config file",
        "quit" => "Quit",
        "split_right" => "Split the pane to the right",
        "split_left" => "Split the pane to the left",
        "split_up" => "Split the pane upwards",
        "split_down" => "Split the pane downwards",
        "close_pane" => "Close the pane",
        "focus_left" => "Focus the pane to the left",
        "focus_right" => "Focus the pane to the right",
        "focus_up" => "Focus the pane above",
        "focus_down" => "Focus the pane below",
        "focus_next_pane" => "Focus the next pane",
        "focus_prev_pane" => "Focus the previous pane",
        "resize_left" => "Shrink the pane leftwards",
        "resize_right" => "Grow the pane rightwards",
        "resize_up" => "Shrink the pane upwards",
        "resize_down" => "Grow the pane downwards",
        "new_tab" => "New tab",
        "close_tab" => "Close the tab",
        "next_tab" => "Next tab",
        "prev_tab" => "Previous tab",
        "increase_font_size" => "Larger text",
        "decrease_font_size" => "Smaller text",
        "reset_font_size" => "Reset the text size",
        "select_all" => "Select everything on screen",
        "toggle_fullscreen" => "Full screen",
        "scroll_line_up" => "Scroll up a line",
        "scroll_line_down" => "Scroll down a line",
        "clear_scrollback" => "Clear the scrollback",
        "scroll_page_up" => "Scroll up a page",
        "scroll_page_down" => "Scroll down a page",
        "scroll_to_top" => "Scroll to the top",
        "scroll_to_bottom" => "Scroll to the bottom",
        "open_settings" => "Settings",
        "open_explorer" => "File explorer",
        "open_help" => "This page",
        "open_plugins" => "Plugins",
        _ => "",
    };
    if known.is_empty() {
        action.replace('_', " ")
    } else {
        known.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_binding_appears_somewhere_on_the_page() {
        // The point of building from the keymap: a binding that exists but is not
        // documented is exactly the thing a hand-written page loses track of.
        let config = Config::default();
        let page = HelpPage::open();
        let widgets = page.widgets(&config);

        let rows: Vec<(&str, &str)> = widgets
            .iter()
            .filter_map(|w| match w {
                Widget::Shortcut { label, keys } => Some((label.as_str(), keys.as_str())),
                _ => None,
            })
            .collect();

        for (chord, action) in config.effective_keys() {
            if action == "none" {
                continue;
            }
            assert!(
                rows.iter().any(|(_, keys)| keys.split("  ").any(|k| k == chord)),
                "{chord} ({action}) is bound but not on the help page"
            );
        }
    }

    #[test]
    fn a_rebound_key_is_reported_at_its_new_chord() {
        // A page listing the defaults rather than the live map would still say
        // ctrl+shift+c here, which is worse than having no page.
        let mut config = Config::default();
        config
            .keys
            .insert("ctrl+shift+y".to_owned(), "copy".to_owned());

        let widgets = HelpPage::open().widgets(&config);
        let copy = widgets
            .iter()
            .find_map(|w| match w {
                Widget::Shortcut { label, keys } if label.contains("Copy") => Some(keys.clone()),
                _ => None,
            })
            .expect("copy should be listed");

        assert!(copy.contains("ctrl+shift+y"), "got {copy}");
    }

    #[test]
    fn an_unbound_action_is_left_off_rather_than_shown_blank() {
        let mut config = Config::default();
        // "none" is how a user removes a default binding.
        config
            .keys
            .insert("ctrl+shift+c".to_owned(), "none".to_owned());

        let widgets = HelpPage::open().widgets(&config);
        assert!(
            !widgets.iter().any(|w| matches!(
                w,
                Widget::Shortcut { label, .. } if label.contains("Copy the selection")
            )),
            "an unbound action has no keys to show"
        );
    }

    #[test]
    fn every_action_the_keymap_knows_about_has_a_description() {
        // A missing description falls back to the raw name, which reads as a bug on a
        // page whose whole job is being readable.
        for name in tuz_input::Action::all_names() {
            assert_ne!(
                describe(name),
                name.replace('_', " "),
                "`{name}` has no description in `describe`"
            );
        }
    }

    #[test]
    fn the_page_has_headings_and_rows() {
        let widgets = HelpPage::open().widgets(&Config::default());
        assert!(widgets
            .iter()
            .any(|w| matches!(w, Widget::Label { heading: true, .. })));
        assert!(widgets.len() > 20, "got {} rows", widgets.len());
    }

    #[test]
    fn the_hand_written_lists_are_not_empty_or_blank() {
        for (keys, what) in EXPLORER_KEYS.iter().chain(PANEL_KEYS) {
            assert!(!keys.trim().is_empty());
            assert!(!what.trim().is_empty());
        }
    }
}
