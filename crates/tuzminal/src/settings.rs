//! The settings panel.
//!
//! Builds a widget list from the live [`Config`] every frame and turns
//! [`UiAction`]s back into config changes. Because the list is rebuilt from the
//! config rather than retained, the panel cannot show a stale value — even if the
//! setting changed some other way while the panel was open (a config reload, a
//! keybinding, a plugin).
//!
//! # Edits apply live
//!
//! Changing the theme or font size takes effect immediately, because seeing the
//! result while choosing is the whole advantage over editing TOML. That makes the
//! three exits distinct, and they are deliberately not the same:
//!
//! - **Save** writes to `config.toml`.
//! - **Revert** restores the config as it was when the panel opened.
//! - **Escape** closes and keeps unsaved changes for the session, matching how the
//!   existing `increase_font_size` keybinding already behaves.

use tuz_config::{Config, CursorShape, Theme};
use tuz_ui::{Ui, UiAction, Widget};

/// Widget ids. Stable values, not positional, so focus survives a rebuild and so a
/// reordered panel cannot silently rebind a control to a different setting.
mod ids {
    use tuz_ui::WidgetId;

    pub const THEME: WidgetId = WidgetId(1);
    pub const FONT_FAMILY: WidgetId = WidgetId(2);
    pub const FONT_SIZE: WidgetId = WidgetId(3);
    pub const LINE_HEIGHT: WidgetId = WidgetId(4);
    pub const LIGATURES: WidgetId = WidgetId(5);

    pub const PADDING_X: WidgetId = WidgetId(10);
    pub const PADDING_Y: WidgetId = WidgetId(11);
    pub const OPACITY: WidgetId = WidgetId(12);
    pub const ALWAYS_TAB_BAR: WidgetId = WidgetId(13);
    pub const DECORATIONS: WidgetId = WidgetId(14);

    pub const CURSOR_SHAPE: WidgetId = WidgetId(20);
    pub const CURSOR_BLINK: WidgetId = WidgetId(21);

    pub const SCROLLBACK: WidgetId = WidgetId(30);
    pub const VSYNC: WidgetId = WidgetId(31);

    pub const SAVE: WidgetId = WidgetId(90);
    pub const REVERT: WidgetId = WidgetId(91);
    pub const CLOSE: WidgetId = WidgetId(92);
}

/// The cursor shapes offered, in the order they appear in the picker.
const CURSOR_SHAPES: &[(CursorShape, &str)] = &[
    (CursorShape::Block, "block"),
    (CursorShape::Beam, "beam"),
    (CursorShape::Underline, "underline"),
    (CursorShape::HollowBlock, "hollow block"),
];

/// What the application should do after handling an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelOutcome {
    /// Nothing further; the panel stays open.
    Continue,
    /// A setting changed. The caller applies the reload actions it already computed.
    Changed,
    /// Write the config to disk.
    Save,
    /// Close the panel.
    Close,
}

pub struct SettingsPanel {
    pub ui: Ui,
    /// The config as it was when the panel opened, for Revert and for diffing on save.
    snapshot: Config,
    /// Installed monospace families, gathered once at open. Enumerating fonts is not
    /// free, and the list cannot change while the panel is up.
    families: Vec<String>,
    /// Theme names, likewise gathered once.
    themes: Vec<String>,
    dirty: bool,
}

impl SettingsPanel {
    pub fn open(config: &Config, families: Vec<String>, themes: Vec<String>) -> Self {
        Self {
            ui: Ui::new(),
            snapshot: config.clone(),
            families,
            themes,
            dirty: false,
        }
    }

    pub fn snapshot(&self) -> &Config {
        &self.snapshot
    }
    /// Whether there are unsaved changes.
    ///
    /// Part of the panel's public surface for callers that want to warn before
    /// discarding; the widget list reads `self.dirty` directly.
    #[allow(dead_code)]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the panel clean, after a successful save.
    pub fn mark_saved(&mut self, config: &Config) {
        self.snapshot = config.clone();
        self.dirty = false;
    }

    /// Build the widget list for the current config.
    pub fn widgets(&self, config: &Config) -> Vec<Widget> {
        let theme_index = self
            .themes
            .iter()
            .position(|t| *t == config.theme)
            .unwrap_or(0);
        // A family the user set by hand may not be in the enumerated list; showing it
        // as the current value anyway is more honest than silently displaying a
        // different font's name.
        let families = if self.families.contains(&config.font.family) {
            self.families.clone()
        } else {
            let mut with_current = vec![config.font.family.clone()];
            with_current.extend(self.families.iter().cloned());
            with_current
        };
        let family_index = families
            .iter()
            .position(|f| *f == config.font.family)
            .unwrap_or(0);

        let cursor_index = CURSOR_SHAPES
            .iter()
            .position(|(shape, _)| *shape == config.cursor.shape)
            .unwrap_or(0);

        vec![
            Widget::heading("Appearance"),
            Widget::select(ids::THEME, "Theme", self.themes.clone(), theme_index),
            Widget::select(ids::FONT_FAMILY, "Font", families, family_index),
            Widget::stepper(
                ids::FONT_SIZE,
                "Font size",
                config.font.size,
                6.0..=72.0,
                1.0,
                1,
            ),
            Widget::stepper(
                ids::LINE_HEIGHT,
                "Line height",
                config.font.line_height,
                0.8..=3.0,
                0.05,
                2,
            ),
            Widget::toggle(ids::LIGATURES, "Ligatures", config.font.ligatures),
            Widget::heading("Window"),
            Widget::stepper(
                ids::PADDING_X,
                "Padding X",
                config.window.padding.x as f32,
                0.0..=64.0,
                2.0,
                0,
            ),
            Widget::stepper(
                ids::PADDING_Y,
                "Padding Y",
                config.window.padding.y as f32,
                0.0..=64.0,
                2.0,
                0,
            ),
            Widget::stepper(
                ids::OPACITY,
                "Opacity",
                config.window.opacity,
                0.2..=1.0,
                0.05,
                2,
            ),
            Widget::toggle(
                ids::ALWAYS_TAB_BAR,
                "Always show tab bar",
                config.window.always_show_tab_bar,
            ),
            Widget::toggle(
                ids::DECORATIONS,
                "Window decorations",
                config.window.decorations,
            ),
            Widget::heading("Cursor"),
            Widget::select(
                ids::CURSOR_SHAPE,
                "Shape",
                CURSOR_SHAPES.iter().map(|(_, n)| (*n).to_owned()).collect(),
                cursor_index,
            ),
            Widget::toggle(ids::CURSOR_BLINK, "Blink", config.cursor.blink),
            Widget::heading("Terminal"),
            Widget::stepper(
                ids::SCROLLBACK,
                "Scrollback lines",
                config.scrollback.lines as f32,
                0.0..=200_000.0,
                1_000.0,
                0,
            ),
            Widget::toggle(ids::VSYNC, "VSync", config.performance.vsync),
            Widget::heading(""),
            // Save is disabled until something changes, so the button itself says
            // whether there is anything to write.
            if self.dirty {
                Widget::button(ids::SAVE, "Save to config.toml")
            } else {
                Widget::disabled_button(ids::SAVE, "Saved")
            },
            if self.dirty {
                Widget::button(ids::REVERT, "Revert")
            } else {
                Widget::disabled_button(ids::REVERT, "Revert")
            },
            Widget::button(ids::CLOSE, "Close"),
        ]
    }

    /// Apply an action to `config`, reporting what the caller should do next.
    ///
    /// Mutates the config directly rather than returning a description of the change:
    /// the caller then runs the existing `Config::diff` to work out what to rebuild,
    /// which keeps one code path for "config changed" no matter what caused it.
    pub fn apply(&mut self, action: UiAction, config: &mut Config) -> PanelOutcome {
        let changed = match action {
            UiAction::Pressed(ids::SAVE) => return PanelOutcome::Save,
            UiAction::Pressed(ids::CLOSE) => return PanelOutcome::Close,
            UiAction::Pressed(ids::REVERT) => {
                *config = self.snapshot.clone();
                self.dirty = false;
                return PanelOutcome::Changed;
            }
            UiAction::Pressed(_) => false,

            UiAction::Toggled(id, on) => match id {
                ids::LIGATURES => set(&mut config.font.ligatures, on),
                ids::ALWAYS_TAB_BAR => set(&mut config.window.always_show_tab_bar, on),
                ids::DECORATIONS => set(&mut config.window.decorations, on),
                ids::CURSOR_BLINK => set(&mut config.cursor.blink, on),
                ids::VSYNC => set(&mut config.performance.vsync, on),
                _ => false,
            },

            UiAction::Selected(id, index) => match id {
                ids::THEME => match self.themes.get(index) {
                    Some(name) => set(&mut config.theme, name.clone()),
                    None => false,
                },
                ids::FONT_FAMILY => {
                    // Rebuilt the same way `widgets` does, so the index means the same
                    // thing on both sides.
                    let families = if self.families.contains(&config.font.family) {
                        self.families.clone()
                    } else {
                        let mut v = vec![config.font.family.clone()];
                        v.extend(self.families.iter().cloned());
                        v
                    };
                    match families.get(index) {
                        Some(name) => set(&mut config.font.family, name.clone()),
                        None => false,
                    }
                }
                ids::CURSOR_SHAPE => match CURSOR_SHAPES.get(index) {
                    Some((shape, _)) => set(&mut config.cursor.shape, *shape),
                    None => false,
                },
                _ => false,
            },

            UiAction::Changed(id, value) => match id {
                ids::FONT_SIZE => set(&mut config.font.size, value),
                ids::LINE_HEIGHT => set(&mut config.font.line_height, value),
                ids::PADDING_X => set(&mut config.window.padding.x, value.round() as u16),
                ids::PADDING_Y => set(&mut config.window.padding.y, value.round() as u16),
                ids::OPACITY => set(&mut config.window.opacity, value),
                ids::SCROLLBACK => set(&mut config.scrollback.lines, value.round() as u32),
                _ => false,
            },
        };

        if changed {
            self.dirty = true;
            PanelOutcome::Changed
        } else {
            PanelOutcome::Continue
        }
    }

    /// The panel's preferred size for a given cell size.
    ///
    /// Derived from the font so the panel scales with the text rather than being a
    /// fixed pixel box that is cramped at large sizes and vast at small ones.
    pub fn preferred_size(cell_width: u32, cell_height: u32) -> (u32, u32) {
        (cell_width * 62, cell_height * 26)
    }
}

/// Assign only if different, reporting whether it changed.
///
/// Used so a click that lands on the value a setting already has does not mark the
/// panel dirty and enable Save for a change that did not happen.
fn set<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        return false;
    }
    *slot = value;
    true
}

/// Theme names available on this system, for the picker.
pub fn theme_names(paths: &tuz_config::Paths) -> Vec<String> {
    let names = Theme::available(paths);
    if names.is_empty() {
        // Should not happen — built-ins are always present — but an empty picker
        // would be inert rather than obviously broken.
        vec![tuz_config::DEFAULT_THEME.to_owned()]
    } else {
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuz_ui::WidgetId;

    fn panel(config: &Config) -> SettingsPanel {
        SettingsPanel::open(
            config,
            vec!["Source Code Pro".to_owned(), "Fira Code".to_owned()],
            vec!["tuz-dark".to_owned(), "tuz-light".to_owned()],
        )
    }

    fn find(widgets: &[Widget], id: WidgetId) -> &Widget {
        widgets
            .iter()
            .find(|w| w.id() == Some(id))
            .expect("widget should be present")
    }

    #[test]
    fn the_panel_reflects_the_config_it_was_given() {
        let config = Config {
            theme: "tuz-light".to_owned(),
            font: tuz_config::Font {
                size: 16.0,
                ligatures: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let p = panel(&config);
        let widgets = p.widgets(&config);

        match find(&widgets, ids::THEME) {
            Widget::Select { options, index, .. } => {
                assert_eq!(options[*index], "tuz-light");
            }
            other => panic!("expected a select, got {other:?}"),
        }
        match find(&widgets, ids::FONT_SIZE) {
            Widget::Stepper { value, .. } => assert_eq!(*value, 16.0),
            other => panic!("expected a stepper, got {other:?}"),
        }
        match find(&widgets, ids::LIGATURES) {
            Widget::Toggle { on, .. } => assert!(*on),
            other => panic!("expected a toggle, got {other:?}"),
        }
    }

    #[test]
    fn a_hand_set_font_family_is_shown_even_if_not_enumerated() {
        // Otherwise the picker would display someone else's font as the current one.
        let config = Config {
            font: tuz_config::Font {
                family: "Some Font I Installed Manually".to_owned(),
                ..Default::default()
            },
            ..Default::default()
        };

        let p = panel(&config);
        let widgets = p.widgets(&config);
        match find(&widgets, ids::FONT_FAMILY) {
            Widget::Select { options, index, .. } => {
                assert_eq!(options[*index], "Some Font I Installed Manually");
            }
            other => panic!("expected a select, got {other:?}"),
        }
    }

    #[test]
    fn selecting_a_theme_updates_the_config() {
        let mut config = Config::default();
        let mut p = panel(&config);

        let outcome = p.apply(UiAction::Selected(ids::THEME, 1), &mut config);
        assert_eq!(outcome, PanelOutcome::Changed);
        assert_eq!(config.theme, "tuz-light");
        assert!(p.is_dirty());
    }

    #[test]
    fn selecting_the_value_already_set_does_not_mark_the_panel_dirty() {
        // Enabling Save for a change that did not happen would be misleading.
        let mut config = Config {
            theme: "tuz-dark".to_owned(),
            ..Default::default()
        };
        let mut p = panel(&config);

        let outcome = p.apply(UiAction::Selected(ids::THEME, 0), &mut config);
        assert_eq!(outcome, PanelOutcome::Continue);
        assert!(!p.is_dirty());
    }

    #[test]
    fn steppers_write_through_with_the_right_rounding() {
        let mut config = Config::default();
        let mut p = panel(&config);

        p.apply(UiAction::Changed(ids::FONT_SIZE, 15.5), &mut config);
        assert_eq!(config.font.size, 15.5);

        // Integer settings round rather than truncate, so 7.6 becomes 8 not 7.
        p.apply(UiAction::Changed(ids::PADDING_X, 7.6), &mut config);
        assert_eq!(config.window.padding.x, 8);

        p.apply(UiAction::Changed(ids::SCROLLBACK, 25_000.0), &mut config);
        assert_eq!(config.scrollback.lines, 25_000);
    }

    #[test]
    fn every_toggle_is_wired_to_a_real_setting() {
        // Guards the hand-written match: a toggle the panel shows but does not handle
        // would look like a control that does nothing.
        let mut config = Config::default();
        let mut p = panel(&config);

        for (id, read) in [
            (
                ids::LIGATURES,
                (|c: &Config| c.font.ligatures) as fn(&Config) -> bool,
            ),
            (ids::ALWAYS_TAB_BAR, |c: &Config| {
                c.window.always_show_tab_bar
            }),
            (ids::DECORATIONS, |c: &Config| c.window.decorations),
            (ids::CURSOR_BLINK, |c: &Config| c.cursor.blink),
            (ids::VSYNC, |c: &Config| c.performance.vsync),
        ] {
            let before = read(&config);
            let outcome = p.apply(UiAction::Toggled(id, !before), &mut config);
            assert_eq!(outcome, PanelOutcome::Changed, "toggle {id:?} did nothing");
            assert_eq!(
                read(&config),
                !before,
                "toggle {id:?} did not write through"
            );
        }
    }

    #[test]
    fn every_widget_the_panel_shows_is_handled() {
        // The other half of the same guard: walk the actual widget list and assert
        // each interactive widget produces a change when driven.
        let base = Config::default();
        let p = panel(&base);
        let widgets = p.widgets(&base);

        for widget in &widgets {
            let Some(id) = widget.id() else { continue };
            if !widget.is_interactive() {
                continue;
            }
            // Buttons have their own outcomes, checked separately.
            if matches!(id, ids::SAVE | ids::REVERT | ids::CLOSE) {
                continue;
            }

            let mut config = base.clone();
            let mut panel = panel(&base);
            let outcome = match widget {
                Widget::Toggle { on, .. } => panel.apply(UiAction::Toggled(id, !on), &mut config),
                Widget::Select { index, options, .. } => {
                    let next = (index + 1) % options.len().max(1);
                    panel.apply(UiAction::Selected(id, next), &mut config)
                }
                Widget::Stepper {
                    value,
                    step,
                    min,
                    max,
                    ..
                } => {
                    // Step away from whichever limit the default sits at. Opacity
                    // defaults to its maximum, so always stepping up would be a
                    // legitimate no-op and this test would fail for the wrong reason.
                    let next = if value + step <= *max {
                        value + step
                    } else {
                        (value - step).max(*min)
                    };
                    panel.apply(UiAction::Changed(id, next), &mut config)
                }
                _ => continue,
            };
            assert_eq!(
                outcome,
                PanelOutcome::Changed,
                "widget {id:?} is shown but not wired to anything"
            );
        }
    }

    #[test]
    fn save_and_close_report_themselves_rather_than_changing_config() {
        let mut config = Config::default();
        let mut p = panel(&config);
        let before = config.clone();

        assert_eq!(
            p.apply(UiAction::Pressed(ids::SAVE), &mut config),
            PanelOutcome::Save
        );
        assert_eq!(
            p.apply(UiAction::Pressed(ids::CLOSE), &mut config),
            PanelOutcome::Close
        );
        assert_eq!(config, before);
    }

    #[test]
    fn revert_restores_the_config_from_when_the_panel_opened() {
        let mut config = Config::default();
        let mut p = panel(&config);

        p.apply(UiAction::Changed(ids::FONT_SIZE, 24.0), &mut config);
        p.apply(UiAction::Selected(ids::THEME, 1), &mut config);
        assert!(p.is_dirty());

        let outcome = p.apply(UiAction::Pressed(ids::REVERT), &mut config);
        assert_eq!(outcome, PanelOutcome::Changed);
        assert_eq!(config, Config::default());
        assert!(!p.is_dirty(), "reverting leaves nothing to save");
    }

    #[test]
    fn save_is_disabled_until_something_changes() {
        let mut config = Config::default();
        let mut p = panel(&config);

        let widgets = p.widgets(&config);
        assert!(
            !find(&widgets, ids::SAVE).is_interactive(),
            "nothing to save yet"
        );

        p.apply(UiAction::Changed(ids::FONT_SIZE, 20.0), &mut config);
        let widgets = p.widgets(&config);
        assert!(
            find(&widgets, ids::SAVE).is_interactive(),
            "now it can save"
        );
    }

    #[test]
    fn marking_saved_clears_dirty_and_moves_the_baseline() {
        let mut config = Config::default();
        let mut p = panel(&config);
        p.apply(UiAction::Changed(ids::FONT_SIZE, 20.0), &mut config);

        p.mark_saved(&config);
        assert!(!p.is_dirty());
        // Reverting now returns to the saved state, not the pre-edit one.
        p.apply(UiAction::Pressed(ids::REVERT), &mut config);
        assert_eq!(config.font.size, 20.0);
    }

    #[test]
    fn cursor_shapes_round_trip_through_the_picker() {
        let mut config = Config::default();
        let mut p = panel(&config);

        for (index, (shape, _)) in CURSOR_SHAPES.iter().enumerate() {
            p.apply(UiAction::Selected(ids::CURSOR_SHAPE, index), &mut config);
            assert_eq!(config.cursor.shape, *shape);
        }
    }

    #[test]
    fn an_out_of_range_selection_is_ignored_rather_than_panicking() {
        // Reachable if the option list shrinks between building and clicking.
        let mut config = Config::default();
        let mut p = panel(&config);
        assert_eq!(
            p.apply(UiAction::Selected(ids::THEME, 99), &mut config),
            PanelOutcome::Continue
        );
        assert_eq!(config.theme, Config::default().theme);
    }

    #[test]
    fn an_unknown_widget_id_is_ignored() {
        let mut config = Config::default();
        let mut p = panel(&config);
        assert_eq!(
            p.apply(UiAction::Toggled(WidgetId(9999), true), &mut config),
            PanelOutcome::Continue
        );
    }

    #[test]
    fn every_setting_the_panel_writes_stays_valid() {
        // The panel must never be able to produce a config the loader would reject.
        let base = Config::default();
        let p = panel(&base);

        for widget in p.widgets(&base) {
            let Some(id) = widget.id() else { continue };
            let Widget::Stepper { min, max, .. } = widget else {
                continue;
            };
            for value in [min, max] {
                let mut config = base.clone();
                let mut panel = panel(&base);
                panel.apply(UiAction::Changed(id, value), &mut config);
                config.validate().unwrap_or_else(|errors| {
                    panic!("{id:?} at {value} produced an invalid config: {errors:?}")
                });
            }
        }
    }

    #[test]
    fn the_panel_size_scales_with_the_font() {
        let small = SettingsPanel::preferred_size(7, 15);
        let large = SettingsPanel::preferred_size(14, 30);
        assert!(large.0 > small.0 && large.1 > small.1);
    }

    #[test]
    fn theme_names_are_never_empty() {
        let names = theme_names(&tuz_config::Paths::for_test());
        assert!(!names.is_empty());
        assert!(names.contains(&tuz_config::DEFAULT_THEME.to_owned()));
    }
}
