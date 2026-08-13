//! The `config.toml` schema.
//!
//! Two conventions hold throughout, and both exist to make hand-editing pleasant:
//!
//! - Every field has a `#[serde(default)]`, so any subset of the file is valid
//!   and a user only writes what they want to change.
//! - Every struct is `deny_unknown_fields`, so `familly = "..."` is reported as
//!   a typo instead of silently ignored.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Fully resolved configuration.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub font: Font,
    pub window: Window,
    pub cursor: Cursor,
    pub scrollback: Scrollback,
    pub shell: Shell,
    pub performance: Performance,
    pub plugins: Plugins,

    /// Name of the theme to load, resolved by [`crate::Theme::load`].
    pub theme: String,

    /// Keychord -> action name, holding **only the user's entries**.
    ///
    /// These are layered on top of [`DEFAULT_KEYS`] by
    /// [`effective_keys`](Config::effective_keys) rather than replacing them, so
    /// binding one extra chord does not silently cost the user every default.
    /// Bind a chord to `"none"` to remove a default. Kept as strings so
    /// `tuz-config` stays free of input dependencies.
    pub keys: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font: Font::default(),
            window: Window::default(),
            cursor: Cursor::default(),
            scrollback: Scrollback::default(),
            shell: Shell::default(),
            performance: Performance::default(),
            plugins: Plugins::default(),
            theme: crate::theme::DEFAULT_THEME.to_owned(),
            // Empty because user entries are merged over DEFAULT_KEYS, not
            // substituted for them. See `effective_keys`.
            keys: BTreeMap::new(),
        }
    }
}

/// The built-in keymap.
///
/// Uses `ctrl+shift` throughout because plain `ctrl+<key>` belongs to the program
/// running inside the terminal — binding `ctrl+c` to Copy would break every CLI.
pub const DEFAULT_KEYS: &[(&str, &str)] = &[
    ("ctrl+shift+d", "split_right"),
    ("ctrl+shift+e", "split_down"),
    ("ctrl+shift+w", "close_pane"),
    ("ctrl+shift+t", "new_tab"),
    ("ctrl+shift+h", "focus_left"),
    ("ctrl+shift+j", "focus_down"),
    ("ctrl+shift+k", "focus_up"),
    ("ctrl+shift+l", "focus_right"),
    ("ctrl+shift+left", "focus_left"),
    ("ctrl+shift+down", "focus_down"),
    ("ctrl+shift+up", "focus_up"),
    ("ctrl+shift+right", "focus_right"),
    ("ctrl+tab", "next_tab"),
    ("ctrl+shift+tab", "prev_tab"),
    ("ctrl+shift+c", "copy"),
    ("ctrl+shift+v", "paste"),
    ("ctrl+shift+plus", "increase_font_size"),
    ("ctrl+shift+minus", "decrease_font_size"),
    ("ctrl+shift+0", "reset_font_size"),
    ("ctrl+shift+r", "reload_config"),
    ("ctrl+shift+comma", "open_settings"),
    ("shift+pageup", "scroll_page_up"),
    ("shift+pagedown", "scroll_page_down"),
];

impl Config {
    /// The keymap actually in force: [`DEFAULT_KEYS`] with the user's entries
    /// layered over it.
    ///
    /// Entries whose action is `"none"` are preserved here and removed by the
    /// keymap builder, so unbinding a default works without this crate needing
    /// to know action names.
    pub fn effective_keys(&self) -> BTreeMap<String, String> {
        let mut merged: BTreeMap<String, String> = DEFAULT_KEYS
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        for (chord, action) in &self.keys {
            merged.insert(chord.clone(), action.clone());
        }
        merged
    }
}

/// Only used by tests, which need the default map in `BTreeMap` form.
#[cfg(test)]
fn default_keys() -> BTreeMap<String, String> {
    [
        ("ctrl+shift+d", "split_right"),
        ("ctrl+shift+e", "split_down"),
        ("ctrl+shift+w", "close_pane"),
        ("ctrl+shift+t", "new_tab"),
        ("ctrl+shift+h", "focus_left"),
        ("ctrl+shift+j", "focus_down"),
        ("ctrl+shift+k", "focus_up"),
        ("ctrl+shift+l", "focus_right"),
        ("ctrl+shift+left", "focus_left"),
        ("ctrl+shift+down", "focus_down"),
        ("ctrl+shift+up", "focus_up"),
        ("ctrl+shift+right", "focus_right"),
        ("ctrl+tab", "next_tab"),
        ("ctrl+shift+tab", "prev_tab"),
        ("ctrl+shift+c", "copy"),
        ("ctrl+shift+v", "paste"),
        ("ctrl+shift+plus", "increase_font_size"),
        ("ctrl+shift+minus", "decrease_font_size"),
        ("ctrl+shift+0", "reset_font_size"),
        ("ctrl+shift+r", "reload_config"),
        ("ctrl+shift+comma", "open_settings"),
        ("shift+pageup", "scroll_page_up"),
        ("shift+pagedown", "scroll_page_down"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect()
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Font {
    /// Primary family name, matched against system fonts.
    pub family: String,
    /// Optional explicit families for the bold/italic/bold-italic faces. When
    /// absent, the matching style of `family` is used.
    pub bold_family: Option<String>,
    pub italic_family: Option<String>,
    pub bold_italic_family: Option<String>,

    /// Point size.
    pub size: f32,

    /// Enable contextual ligatures (`calt`/`liga`), e.g. Fira Code's `=>`.
    pub ligatures: bool,

    /// Extra OpenType features as `("ss01", 1)` style pairs.
    pub features: BTreeMap<String, u32>,

    /// Families tried in order when the primary font lacks a glyph.
    pub fallback: Vec<String>,

    /// Multiplier on the font's natural line height. 1.0 keeps metric height.
    pub line_height: f32,
    /// Multiplier on the natural advance width.
    pub cell_width: f32,

    /// Synthesize bold by thickening when no bold face exists.
    pub synthetic_bold: bool,
    /// Synthesize italic by shearing when no italic face exists.
    pub synthetic_italic: bool,

    /// Force grayscale antialiasing instead of subpixel (LCD) filtering.
    ///
    /// **Currently has no effect**: the renderer only implements grayscale
    /// coverage. Subpixel AA needs three-channel coverage in the glyph atlas and a
    /// dual-source blend in the shader. Kept in the schema so existing configs do
    /// not break when it lands.
    pub grayscale_antialiasing: bool,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            family: "monospace".to_owned(),
            bold_family: None,
            italic_family: None,
            bold_italic_family: None,
            size: 12.0,
            ligatures: false,
            features: BTreeMap::new(),
            // Emoji and CJK are the two fallbacks essentially every user needs,
            // so they are on by default; missing families are skipped silently.
            fallback: vec![
                "Noto Color Emoji".to_owned(),
                "Symbols Nerd Font".to_owned(),
                "Noto Sans CJK JP".to_owned(),
            ],
            line_height: 1.0,
            cell_width: 1.0,
            synthetic_bold: true,
            synthetic_italic: true,
            grayscale_antialiasing: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Padding {
    pub x: u16,
    pub y: u16,
}

impl Default for Padding {
    fn default() -> Self {
        Self { x: 8, y: 8 }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Window {
    pub padding: Padding,
    /// Grow padding so the grid is centered when the window size is not an exact
    /// multiple of the cell size, rather than leaving a gap on one edge.
    pub center_grid: bool,
    /// Window opacity, `0.0..=1.0`. Requires a compositor with alpha support.
    pub opacity: f32,
    pub decorations: bool,
    /// Initial size in cells.
    pub columns: u16,
    pub rows: u16,
    pub title: String,
    /// Let the running program change the window title via OSC 0/2.
    pub dynamic_title: bool,
    /// Pixel width of the divider drawn between splits.
    pub split_divider_width: u16,
    /// Show the tab bar even when only one tab is open.
    pub always_show_tab_bar: bool,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            padding: Padding::default(),
            center_grid: true,
            opacity: 1.0,
            decorations: true,
            columns: 100,
            rows: 30,
            title: "Tuzminal".to_owned(),
            dynamic_title: true,
            split_divider_width: 1,
            always_show_tab_bar: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
    HollowBlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cursor {
    pub shape: CursorShape,
    pub blink: bool,
    /// Blink half-period in milliseconds.
    pub blink_interval_ms: u64,
    /// Stop blinking after this many seconds of no input. 0 disables the timeout.
    pub blink_timeout_secs: u64,
    /// Shape used when the window loses focus.
    pub unfocused_shape: CursorShape,
    /// Let the running program override the shape via DECSCUSR.
    pub allow_program_override: bool,
    /// Cursor thickness as a fraction of cell size, for beam and underline.
    pub thickness: f32,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            blink: true,
            blink_interval_ms: 500,
            blink_timeout_secs: 10,
            unfocused_shape: CursorShape::HollowBlock,
            allow_program_override: true,
            thickness: 0.15,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scrollback {
    /// Maximum retained lines per pane. Memory cost is roughly
    /// `lines * columns * 16` bytes.
    pub lines: u32,
    /// Lines advanced per mouse wheel notch.
    pub scroll_multiplier: u8,
    /// Jump to the bottom when the program writes new output.
    pub scroll_to_bottom_on_output: bool,
    /// Jump to the bottom on keypress.
    pub scroll_to_bottom_on_input: bool,
}

impl Default for Scrollback {
    fn default() -> Self {
        Self {
            lines: 10_000,
            scroll_multiplier: 3,
            scroll_to_bottom_on_output: false,
            scroll_to_bottom_on_input: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Shell {
    /// Program to launch. `None` uses `$SHELL`, then the platform default.
    pub program: Option<String>,
    pub args: Vec<String>,
    /// Working directory for new panes. `None` inherits, `"inherit_pane"` uses
    /// the focused pane's cwd when it can be determined.
    pub working_directory: Option<String>,
    /// Extra environment variables for spawned programs.
    pub env: BTreeMap<String, String>,
    /// Value advertised as `$TERM`. `xterm-256color` is the safe default;
    /// `tuzminal` requires the terminfo entry to be installed.
    pub term: String,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            program: None,
            args: Vec::new(),
            working_directory: None,
            env: BTreeMap::new(),
            term: "xterm-256color".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuBackend {
    /// Let wgpu pick the best available backend.
    Auto,
    Vulkan,
    Metal,
    Dx12,
    Gl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerPreference {
    LowPower,
    HighPerformance,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Performance {
    /// Changing this requires a restart: the wgpu instance is created once.
    pub gpu_backend: GpuBackend,
    /// Integrated GPUs are usually the right choice for a terminal — less power
    /// for identical results.
    pub power_preference: PowerPreference,
    pub vsync: bool,
    /// Frame rate ceiling when `vsync` is off. 0 means unlimited.
    pub max_fps: u16,
    /// Redraw only damaged regions. Disable to diagnose rendering artifacts.
    pub damage_tracking: bool,
}

impl Default for Performance {
    fn default() -> Self {
        Self {
            gpu_backend: GpuBackend::Auto,
            power_preference: PowerPreference::LowPower,
            vsync: true,
            max_fps: 0,
            damage_tracking: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Plugins {
    pub enabled: bool,
    /// Plugins to load by name. Empty means "every plugin found on disk".
    pub load: Vec<String>,
    /// Plugins to skip even if present. Applied after `load`.
    pub disable: Vec<String>,
    /// Wall-clock budget for a single plugin callback. Exceeding it aborts the
    /// call so a misbehaving plugin cannot freeze the terminal.
    pub callback_timeout_ms: u64,
    /// Deadline for the synchronous `on_key` hook specifically. Kept small
    /// because every keystroke waits on it.
    pub key_hook_timeout_ms: u64,
}

impl Default for Plugins {
    fn default() -> Self {
        Self {
            enabled: true,
            load: Vec::new(),
            disable: Vec::new(),
            callback_timeout_ms: 250,
            key_hook_timeout_ms: 5,
        }
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// A single problem found in an otherwise well-formed config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// Dotted path to the offending field, e.g. `font.size`.
    pub field: String,
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl Config {
    /// Check semantic constraints that the type system cannot express.
    ///
    /// Returns *every* problem rather than the first, so a user fixing their
    /// config sees the whole list in one pass.
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errs = Vec::new();
        let mut err = |field: &str, message: String| {
            errs.push(ValidationError {
                field: field.to_owned(),
                message,
            })
        };

        if !(1.0..=400.0).contains(&self.font.size) {
            err(
                "font.size",
                format!("must be between 1 and 400, got {}", self.font.size),
            );
        }
        if self.font.family.trim().is_empty() {
            err("font.family", "must not be empty".to_owned());
        }
        if !(0.1..=10.0).contains(&self.font.line_height) {
            err(
                "font.line_height",
                format!("must be between 0.1 and 10, got {}", self.font.line_height),
            );
        }
        if !(0.1..=10.0).contains(&self.font.cell_width) {
            err(
                "font.cell_width",
                format!("must be between 0.1 and 10, got {}", self.font.cell_width),
            );
        }
        for tag in self.font.features.keys() {
            // OpenType feature tags are exactly four ASCII characters.
            if tag.len() != 4 || !tag.is_ascii() {
                err(
                    "font.features",
                    format!("`{tag}` is not a 4-character OpenType tag"),
                );
            }
        }

        if !(0.0..=1.0).contains(&self.window.opacity) {
            err(
                "window.opacity",
                format!("must be between 0.0 and 1.0, got {}", self.window.opacity),
            );
        }
        if self.window.columns == 0 {
            err("window.columns", "must be at least 1".to_owned());
        }
        if self.window.rows == 0 {
            err("window.rows", "must be at least 1".to_owned());
        }

        if !(0.01..=1.0).contains(&self.cursor.thickness) {
            err(
                "cursor.thickness",
                format!(
                    "must be between 0.01 and 1.0, got {}",
                    self.cursor.thickness
                ),
            );
        }
        if self.cursor.blink && self.cursor.blink_interval_ms < 50 {
            err(
                "cursor.blink_interval_ms",
                format!(
                    "must be at least 50ms, got {}",
                    self.cursor.blink_interval_ms
                ),
            );
        }

        if self.scrollback.lines > 10_000_000 {
            err(
                "scrollback.lines",
                format!(
                    "{} lines would use an unreasonable amount of memory; \
                     the maximum is 10000000",
                    self.scrollback.lines
                ),
            );
        }
        if self.scrollback.scroll_multiplier == 0 {
            err(
                "scrollback.scroll_multiplier",
                "must be at least 1, or scrolling does nothing".to_owned(),
            );
        }

        if self.shell.term.trim().is_empty() {
            err("shell.term", "must not be empty".to_owned());
        }

        if self.theme.trim().is_empty() {
            err("theme", "must not be empty".to_owned());
        }

        if self.plugins.key_hook_timeout_ms > 50 {
            err(
                "plugins.key_hook_timeout_ms",
                format!(
                    "{}ms would add visible input lag; keep it at 50ms or less",
                    self.plugins.key_hook_timeout_ms
                ),
            );
        }

        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }
}

// ---------------------------------------------------------------------------
// Reload diffing
// ---------------------------------------------------------------------------

/// What the application must do to apply a config change.
///
/// Computed by [`Config::diff`] so a reload rebuilds only what actually changed —
/// re-rasterizing the glyph atlas on a keybind edit would be a visible stall.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReloadActions {
    /// Font stack changed: rebuild faces, atlas and cell metrics, then resize
    /// every PTY because the cell size moved.
    pub rebuild_fonts: bool,
    /// Theme name changed: reload the theme file.
    pub reload_theme: bool,
    /// Geometry changed: recompute pane rects and resize PTYs.
    pub relayout: bool,
    /// Keymap changed: rebuild the chord table.
    pub rebind_keys: bool,
    /// Plugin selection or limits changed: restart the plugin host.
    pub reload_plugins: bool,
    /// Surface presentation changed: reconfigure the wgpu surface.
    pub reconfigure_surface: bool,
    /// Scrollback capacity changed: resize history buffers.
    pub resize_scrollback: bool,
    /// Something visual changed: request a redraw.
    pub redraw: bool,
    /// Fields that cannot be applied to a running process. Reported to the user
    /// as "restart to apply" rather than silently ignored.
    pub restart_required: Vec<&'static str>,
}

impl ReloadActions {
    /// True when nothing at all needs doing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl Config {
    /// Determine the minimal work needed to move from `self` to `new`.
    pub fn diff(&self, new: &Config) -> ReloadActions {
        let mut a = ReloadActions::default();

        // Anything affecting glyph rasterization or cell metrics.
        if self.font != new.font {
            a.rebuild_fonts = true;
            a.relayout = true;
            a.redraw = true;
        }

        if self.theme != new.theme {
            a.reload_theme = true;
            a.redraw = true;
        }

        // Geometry that changes pane rects, and therefore the cell grid.
        if self.window.padding != new.window.padding
            || self.window.center_grid != new.window.center_grid
            || self.window.split_divider_width != new.window.split_divider_width
            || self.window.always_show_tab_bar != new.window.always_show_tab_bar
        {
            a.relayout = true;
            a.redraw = true;
        }

        // Purely visual window properties.
        if self.window.opacity != new.window.opacity
            || self.window.decorations != new.window.decorations
            || self.window.title != new.window.title
            || self.window.dynamic_title != new.window.dynamic_title
        {
            a.redraw = true;
        }

        if self.cursor != new.cursor {
            a.redraw = true;
        }

        if self.scrollback.lines != new.scrollback.lines {
            a.resize_scrollback = true;
        }

        if self.keys != new.keys {
            a.rebind_keys = true;
        }

        if self.plugins != new.plugins {
            a.reload_plugins = true;
        }

        if self.performance.vsync != new.performance.vsync
            || self.performance.max_fps != new.performance.max_fps
        {
            a.reconfigure_surface = true;
            a.redraw = true;
        }
        if self.performance.damage_tracking != new.performance.damage_tracking {
            a.redraw = true;
        }

        // The wgpu instance and adapter are chosen once at startup.
        if self.performance.gpu_backend != new.performance.gpu_backend {
            a.restart_required.push("performance.gpu_backend");
        }
        if self.performance.power_preference != new.performance.power_preference {
            a.restart_required.push("performance.power_preference");
        }

        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_toml_yields_defaults() {
        // The whole point of blanket `#[serde(default)]`: an empty file is valid.
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn partial_toml_overrides_only_named_fields() {
        let c: Config = toml::from_str(
            r#"
            theme = "tuz-light"
            [font]
            size = 14.5
            "#,
        )
        .unwrap();

        assert_eq!(c.theme, "tuz-light");
        assert_eq!(c.font.size, 14.5);
        // Untouched fields keep their defaults.
        assert_eq!(c.font.family, Config::default().font.family);
        assert_eq!(c.window.padding, Padding::default());
        assert!(c.keys.is_empty(), "the user specified no keys");
        assert_eq!(c.effective_keys(), default_keys());
    }

    #[test]
    fn user_keys_layer_over_the_defaults_instead_of_replacing_them() {
        // The trap this avoids: adding one binding and silently losing copy,
        // paste, splits and tabs.
        let c: Config = toml::from_str(
            r#"
            [keys]
            "ctrl+shift+x" = "close_pane"
            "#,
        )
        .unwrap();

        let keys = c.effective_keys();
        assert_eq!(keys.get("ctrl+shift+x").unwrap(), "close_pane");
        assert_eq!(
            keys.get("ctrl+shift+c").unwrap(),
            "copy",
            "defaults must survive"
        );
        assert_eq!(keys.len(), DEFAULT_KEYS.len() + 1);
    }

    #[test]
    fn a_user_binding_overrides_the_default_for_the_same_chord() {
        let c: Config = toml::from_str(
            r#"
            [keys]
            "ctrl+shift+t" = "split_right"
            "#,
        )
        .unwrap();

        assert_eq!(
            c.effective_keys().get("ctrl+shift+t").unwrap(),
            "split_right"
        );
        assert_eq!(c.effective_keys().len(), DEFAULT_KEYS.len());
    }

    #[test]
    fn a_default_can_be_unbound_with_none() {
        let c: Config = toml::from_str(
            r#"
            [keys]
            "ctrl+shift+t" = "none"
            "#,
        )
        .unwrap();

        // The marker is preserved for the keymap builder to act on; this crate
        // deliberately does not interpret action names.
        assert_eq!(c.effective_keys().get("ctrl+shift+t").unwrap(), "none");
    }

    #[test]
    fn the_default_keymap_has_no_duplicate_chords() {
        let mut seen = std::collections::HashSet::new();
        for (chord, _) in DEFAULT_KEYS {
            assert!(seen.insert(*chord), "duplicate default binding `{chord}`");
        }
    }

    #[test]
    fn misspelled_field_is_an_error_not_a_silent_ignore() {
        let err = toml::from_str::<Config>("[font]\nfamilly = \"Mono\"\n")
            .expect_err("typo must be rejected");
        assert!(
            err.to_string().contains("familly"),
            "error should name the offending key: {err}"
        );
    }

    #[test]
    fn defaults_are_valid() {
        Config::default()
            .validate()
            .expect("defaults must validate");
    }

    #[test]
    fn default_config_serializes_and_round_trips() {
        let s = toml::to_string(&Config::default()).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back, Config::default());
    }

    #[test]
    fn validation_reports_every_problem_at_once() {
        let mut c = Config::default();
        c.font.size = 0.0;
        c.window.opacity = 5.0;
        c.scrollback.scroll_multiplier = 0;
        c.theme = "  ".to_owned();

        let errs = c.validate().expect_err("should not validate");
        let fields: Vec<&str> = errs.iter().map(|e| e.field.as_str()).collect();
        assert!(fields.contains(&"font.size"), "{fields:?}");
        assert!(fields.contains(&"window.opacity"), "{fields:?}");
        assert!(
            fields.contains(&"scrollback.scroll_multiplier"),
            "{fields:?}"
        );
        assert!(fields.contains(&"theme"), "{fields:?}");
    }

    #[test]
    fn bad_opentype_tag_is_rejected() {
        let mut c = Config::default();
        c.font.features.insert("toolong".to_owned(), 1);
        let errs = c.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.field == "font.features"));
    }

    #[test]
    fn slow_key_hook_is_rejected_as_input_lag() {
        let mut c = Config::default();
        c.plugins.key_hook_timeout_ms = 500;
        let errs = c.validate().unwrap_err();
        assert!(errs
            .iter()
            .any(|e| e.field == "plugins.key_hook_timeout_ms"));
    }

    #[test]
    fn identical_configs_need_no_work() {
        let c = Config::default();
        assert!(c.diff(&c).is_empty());
    }

    #[test]
    fn keybind_change_does_not_rebuild_the_font_atlas() {
        // The regression this guards: treating any config change as "reload
        // everything", which stalls the render loop on a trivial edit.
        let a = Config::default();
        let mut b = a.clone();
        b.keys.insert("ctrl+shift+x".into(), "close_pane".into());

        let d = a.diff(&b);
        assert!(d.rebind_keys);
        assert!(!d.rebuild_fonts);
        assert!(!d.relayout);
        assert!(!d.reload_theme);
    }

    #[test]
    fn font_change_forces_relayout_because_cell_size_moves() {
        let a = Config::default();
        let mut b = a.clone();
        b.font.size += 2.0;

        let d = a.diff(&b);
        assert!(d.rebuild_fonts);
        assert!(d.relayout, "a new cell size must resize every PTY");
        assert!(d.redraw);
    }

    #[test]
    fn padding_change_relayouts_without_touching_fonts() {
        let a = Config::default();
        let mut b = a.clone();
        b.window.padding.x += 4;

        let d = a.diff(&b);
        assert!(d.relayout);
        assert!(!d.rebuild_fonts);
    }

    #[test]
    fn theme_change_is_a_cheap_reload() {
        let a = Config::default();
        let mut b = a.clone();
        b.theme = "tuz-light".to_owned();

        let d = a.diff(&b);
        assert!(d.reload_theme);
        assert!(d.redraw);
        assert!(!d.rebuild_fonts);
        assert!(!d.relayout);
    }

    #[test]
    fn gpu_backend_change_is_reported_as_restart_required() {
        let a = Config::default();
        let mut b = a.clone();
        b.performance.gpu_backend = GpuBackend::Vulkan;

        let d = a.diff(&b);
        assert_eq!(d.restart_required, ["performance.gpu_backend"]);
        // It must not be silently treated as applied.
        assert!(!d.reconfigure_surface);
    }

    #[test]
    fn vsync_change_reconfigures_the_surface_without_a_restart() {
        let a = Config::default();
        let mut b = a.clone();
        b.performance.vsync = false;

        let d = a.diff(&b);
        assert!(d.reconfigure_surface);
        assert!(d.restart_required.is_empty());
    }

    #[test]
    fn scrollback_resize_is_distinct_from_redraw() {
        let a = Config::default();
        let mut b = a.clone();
        b.scrollback.lines = 50_000;

        let d = a.diff(&b);
        assert!(d.resize_scrollback);
        assert!(!d.rebuild_fonts);
    }
}
