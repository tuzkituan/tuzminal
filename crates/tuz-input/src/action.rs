//! Actions a keychord can trigger.
//!
//! Actions are named in config as snake_case strings. Names are resolved eagerly
//! at load time so a typo is reported then, rather than producing a key that
//! silently does nothing.

use std::fmt;

/// Something the terminal can be asked to do.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    // --- panes ---
    SplitRight,
    SplitLeft,
    SplitUp,
    SplitDown,
    ClosePane,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    /// Cycle focus through the panes of the active tab.
    FocusNextPane,
    FocusPrevPane,
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,

    // --- tabs ---
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    /// Jump to a tab by 1-based position.
    SelectTab(u8),

    // --- clipboard ---
    Copy,
    Paste,
    SelectAll,

    // --- appearance ---
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
    ToggleFullscreen,

    // --- scrolling ---
    ScrollLineUp,
    ScrollLineDown,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToTop,
    ScrollToBottom,
    ClearScrollback,

    // --- application ---
    ReloadConfig,
    OpenSettings,
    OpenExplorer,
    OpenHelp,
    Quit,

    /// Send a literal byte string to the focused pane. Not nameable in config's
    /// simple `chord = "action"` form; produced by plugins.
    SendText(String),

    /// A command registered by a plugin, addressed by name.
    Plugin(String),
}

/// Every action nameable directly in config, paired with its config name.
///
/// Single source of truth for parsing, display and the `--list-actions` help, so
/// the three can never disagree.
const NAMED_ACTIONS: &[(&str, Action)] = &[
    ("split_right", Action::SplitRight),
    ("split_left", Action::SplitLeft),
    ("split_up", Action::SplitUp),
    ("split_down", Action::SplitDown),
    ("close_pane", Action::ClosePane),
    ("focus_left", Action::FocusLeft),
    ("focus_right", Action::FocusRight),
    ("focus_up", Action::FocusUp),
    ("focus_down", Action::FocusDown),
    ("focus_next_pane", Action::FocusNextPane),
    ("focus_prev_pane", Action::FocusPrevPane),
    ("resize_left", Action::ResizeLeft),
    ("resize_right", Action::ResizeRight),
    ("resize_up", Action::ResizeUp),
    ("resize_down", Action::ResizeDown),
    ("new_tab", Action::NewTab),
    ("close_tab", Action::CloseTab),
    ("next_tab", Action::NextTab),
    ("prev_tab", Action::PrevTab),
    ("copy", Action::Copy),
    ("paste", Action::Paste),
    ("select_all", Action::SelectAll),
    ("increase_font_size", Action::IncreaseFontSize),
    ("decrease_font_size", Action::DecreaseFontSize),
    ("reset_font_size", Action::ResetFontSize),
    ("toggle_fullscreen", Action::ToggleFullscreen),
    ("scroll_line_up", Action::ScrollLineUp),
    ("scroll_line_down", Action::ScrollLineDown),
    ("scroll_page_up", Action::ScrollPageUp),
    ("scroll_page_down", Action::ScrollPageDown),
    ("scroll_to_top", Action::ScrollToTop),
    ("scroll_to_bottom", Action::ScrollToBottom),
    ("clear_scrollback", Action::ClearScrollback),
    ("reload_config", Action::ReloadConfig),
    ("open_settings", Action::OpenSettings),
    ("open_explorer", Action::OpenExplorer),
    ("open_help", Action::OpenHelp),
    ("quit", Action::Quit),
];

/// The config value that unbinds a chord, including a default one.
pub const UNBIND: &str = "none";

impl Action {
    /// Resolve a config action name.
    ///
    /// Returns `None` for unrecognized names; the caller decides whether that is
    /// a typo or a plugin-registered command.
    pub fn parse(s: &str) -> Option<Action> {
        let s = s.trim();

        // `select_tab_3` and friends carry their argument in the name, which
        // keeps the config format a flat string map.
        if let Some(n) = s.strip_prefix("select_tab_") {
            let n: u8 = n.parse().ok()?;
            if (1..=99).contains(&n) {
                return Some(Action::SelectTab(n));
            }
            return None;
        }

        NAMED_ACTIONS
            .iter()
            .find(|(name, _)| *name == s)
            .map(|(_, a)| a.clone())
    }

    /// All directly nameable action names, for help output and error messages.
    pub fn all_names() -> impl Iterator<Item = &'static str> {
        NAMED_ACTIONS.iter().map(|(n, _)| *n)
    }

    /// The config name for this action, when it has one.
    pub fn name(&self) -> Option<String> {
        if let Action::SelectTab(n) = self {
            return Some(format!("select_tab_{n}"));
        }
        NAMED_ACTIONS
            .iter()
            .find(|(_, a)| a == self)
            .map(|(n, _)| (*n).to_owned())
    }

    /// True for actions the terminal handles itself, as opposed to plugin
    /// commands that must be dispatched to the plugin host.
    pub fn is_builtin(&self) -> bool {
        !matches!(self, Action::Plugin(_))
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Plugin(name) => write!(f, "plugin:{name}"),
            Action::SendText(s) => write!(f, "send_text({s:?})"),
            other => match other.name() {
                Some(n) => write!(f, "{n}"),
                None => write!(f, "{other:?}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_action_names() {
        assert_eq!(Action::parse("split_right"), Some(Action::SplitRight));
        assert_eq!(Action::parse("close_pane"), Some(Action::ClosePane));
        assert_eq!(Action::parse("quit"), Some(Action::Quit));
    }

    #[test]
    fn tolerates_surrounding_whitespace() {
        assert_eq!(Action::parse("  copy  "), Some(Action::Copy));
    }

    #[test]
    fn parses_indexed_tab_selection() {
        assert_eq!(Action::parse("select_tab_1"), Some(Action::SelectTab(1)));
        assert_eq!(Action::parse("select_tab_9"), Some(Action::SelectTab(9)));
        assert_eq!(Action::parse("select_tab_0"), None, "tabs are 1-based");
        assert_eq!(Action::parse("select_tab_abc"), None);
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        // The caller reports these as typos rather than binding a dead key.
        assert_eq!(Action::parse("split_diagonally"), None);
        assert_eq!(Action::parse(""), None);
    }

    #[test]
    fn every_named_action_round_trips() {
        // Guards the parse/display tables against drifting apart.
        for name in Action::all_names() {
            let action = Action::parse(name)
                .unwrap_or_else(|| panic!("`{name}` is listed but does not parse"));
            assert_eq!(
                action.name().as_deref(),
                Some(name),
                "`{name}` does not print back as itself"
            );
            assert_eq!(action.to_string(), name);
        }
    }

    #[test]
    fn action_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for name in Action::all_names() {
            assert!(seen.insert(name), "duplicate action name `{name}`");
        }
    }

    #[test]
    fn indexed_tab_selection_round_trips() {
        let a = Action::SelectTab(4);
        assert_eq!(a.name().as_deref(), Some("select_tab_4"));
        assert_eq!(Action::parse("select_tab_4"), Some(a));
    }

    #[test]
    fn plugin_actions_are_distinguishable_from_builtins() {
        let p = Action::Plugin("my-plugin.toggle".to_owned());
        assert!(!p.is_builtin());
        assert!(Action::Quit.is_builtin());
        assert_eq!(p.to_string(), "plugin:my-plugin.toggle");
        // A plugin command name is not confusable with a builtin.
        assert_eq!(p.name(), None);
    }
}
