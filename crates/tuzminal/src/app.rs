//! The winit application: window lifecycle, event dispatch, rendering and input.
//!
//! This is the layer that owns everything mutable and connects the pure crates:
//!
//! ```text
//!   winit events ──► App ──► tuz-input   (chord -> action)
//!                     │  └──► tuz-core    (key bytes -> PTY)
//!                     │
//!   PTY events ──────►│──► tuz-layout  (pane rects)
//!                     │──► tuz-core    (grid snapshot)
//!                     │──► tuz-font    (glyphs)
//!                     └──► tuz-render  (instances -> GPU)
//! ```
//!
//! # Redraw policy
//!
//! The loop sleeps until something happens. A PTY that produces output wakes it
//! through an `EventLoopProxy`; a blinking cursor schedules a `WaitUntil`. Nothing
//! polls, so an idle terminal uses no CPU — which is the difference between a
//! terminal you leave open all day and one you close.

use crate::gpu::{FrameOutcome, Gpu};
use crate::keys;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tuz_config::{ConfigManager, Paths, ReloadOutcome};
use tuz_core::{
    encode_paste, MouseButton, MouseReporting, PaneEvent, Session, TermEvent, TermSize,
};
use tuz_font::FontSystem;
use tuz_input::{Action, Keymap};
use tuz_layout::{
    Branch, CellSize, ChromeButton, CloseOutcome, Direction, Layout, LayoutOptions, PaneId, Rect,
};
use tuz_plugin::Host as PluginHost;
use tuz_plugin_api::{Command as PluginCommand, Event as PluginEvent, KeyOutcome};
use tuz_render::{build_pane, ColorSpace, Instance, PaneGeometry, Renderer};
use tuz_ui::{UiKey, Widget};

use crate::settings::{PanelOutcome, SettingsPanel};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

/// A message shown briefly over the terminal.
struct Notification {
    text: String,
    level: tuz_plugin_api::NotifyLevel,
    shown_at: Instant,
}

/// How long a toast stays fully opaque before fading.
const TOAST_HOLD: Duration = Duration::from_secs(4);
/// How long the fade itself takes.
const TOAST_FADE: Duration = Duration::from_millis(600);
/// Cap on stacked toasts. A plugin in a loop must not fill the window.
const MAX_TOASTS: usize = 4;

/// A tab being dragged along the strip.
#[derive(Debug, Clone, Copy)]
struct TabDrag {
    /// Where the tab started, so a drag that goes nowhere can be told from a click.
    origin: usize,
    /// Where it currently is; the reorder is applied as the pointer crosses tabs
    /// rather than on release, so the strip previews the result.
    current: usize,
    /// Pointer x when the drag began, to require real movement before it counts as a
    /// drag rather than a click that wobbled.
    start_x: i32,
    /// Set once movement passed the threshold.
    active: bool,
}

/// How far the pointer must move before a press becomes a drag.
///
/// Without this every click on a tab is a one-pixel drag, and a click that happens to
/// wobble would reorder the strip.
const DRAG_THRESHOLD: i32 = 6;

/// Fraction of a split adjusted per keyboard resize action.
const RESIZE_STEP: f32 = 0.02;

/// How close to a divider a click counts as grabbing it. A 1px divider is
/// impossible to hit otherwise.
const DIVIDER_GRAB: u32 = 4;

/// Vertical inset inside the tab and status strips.
const CHROME_PADDING: u32 = 9;

/// How close together two title-bar presses must be to count as a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// How far apart they may be, in pixels, and still count. Without some slack a
/// double-click fails whenever the pointer drifts by one pixel between presses.
const DOUBLE_CLICK_SLOP: i32 = 4;

/// Preferred and minimum tab widths in pixels.
const TAB_WIDTH: u32 = 200;
const MIN_TAB_WIDTH: u32 = 70;

/// Cell size assumed before fonts load, only used to pick an initial window size.
const BOOTSTRAP_CELL: CellSize = CellSize {
    width: 8,
    height: 17,
};

/// Events pushed into the loop from other threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserEvent {
    /// A watched config or theme file changed.
    ConfigChanged,
    /// A PTY produced output or an event.
    Wakeup,
}

pub struct App {
    settings: ConfigManager,
    keymap: Keymap,
    layout: Layout,
    sessions: HashMap<PaneId, Session>,

    fonts: Option<FontSystem>,
    renderer: Option<Renderer>,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,

    /// Channel PTY threads report on, plus the sender handed to new sessions.
    events_tx: crossbeam_channel::Sender<PaneEvent>,
    events_rx: crossbeam_channel::Receiver<PaneEvent>,
    /// Wakes the event loop from a PTY thread.
    waker: Arc<dyn Fn() + Send + Sync>,
    proxy: EventLoopProxy<UserEvent>,

    /// Reused between frames so a steady-state redraw allocates nothing.
    instances: Vec<Instance>,
    /// Geometry from the last layout pass, for hit-testing mouse events.
    frame: Option<tuz_layout::Frame>,

    modifiers: ModifiersState,
    mouse: (f64, f64),
    /// Set while dragging a selection in this pane.
    selecting: Option<PaneId>,
    /// Set while dragging a split divider.
    dragging: Option<Vec<Branch>>,
    /// The chrome button under the pointer, for hover highlighting.
    hovered_button: Option<ChromeButton>,
    /// When and where the title bar was last pressed, for double-click detection.
    last_title_click: Option<(Instant, i32, i32)>,
    /// The tab under the pointer, so only that tab shows a close button.
    hovered_tab: Option<usize>,
    /// True when the pointer is over the hovered tab's close button.
    hovered_close: bool,
    /// The settings panel, when open. `None` means closed.
    panel: Option<SettingsPanel>,
    /// The panel's scrollable body from the last frame, for wheel scrolling and for
    /// clipping rows to it.
    panel_body: Option<Rect>,
    /// Tab being dragged, and where it would land if released now.
    dragging_tab: Option<TabDrag>,
    /// Transient on-screen messages, newest last.
    toasts: Vec<Notification>,

    /// Cursor blink phase and when it last flipped.
    blink_on: bool,
    blink_at: Instant,
    /// Last input time, for the blink timeout.
    last_input: Instant,

    /// Titles reported by each pane's program, for tab labels.
    titles: HashMap<PaneId, String>,
    /// Panes that produced output since their tab was last focused, so an inactive
    /// tab can show an activity dot.
    activity: std::collections::HashSet<PaneId>,

    clipboard: Option<arboard::Clipboard>,
    /// Loaded plugins. Runs inline on this thread under a per-callback budget; see
    /// `tuz_plugin::PluginRuntime` for why it is not on its own thread.
    plugins: PluginHost,
    exit_requested: bool,
}

impl App {
    /// Build the event loop, spawn the first shell, and run until exit.
    pub fn run(paths: Paths) -> Result<()> {
        let mut settings = ConfigManager::load(paths);
        if let Some(err) = settings.last_error() {
            log::warn!("configuration problem, using defaults:\n{err}");
        }

        let event_loop = EventLoop::<UserEvent>::with_user_event()
            .build()
            .context("failed to create the event loop")?;
        event_loop.set_control_flow(ControlFlow::Wait);

        let proxy = event_loop.create_proxy();

        if let Err(e) = settings.watch() {
            log::warn!("live config reload unavailable: {e}");
        }
        if let Some(rx) = settings.change_receiver().cloned() {
            let proxy = proxy.clone();
            std::thread::Builder::new()
                .name("tuz-config-forward".into())
                .spawn(move || {
                    while rx.recv().is_ok() {
                        if proxy.send_event(UserEvent::ConfigChanged).is_err() {
                            break;
                        }
                    }
                })
                .context("failed to spawn the config forwarding thread")?;
        }

        // Plugins load before the keymap so the commands they register can be
        // bound by name in config.
        let plugins = if settings.config().plugins.enabled {
            let cfg = settings.config().plugins.clone();
            let mut host = PluginHost::new(
                Duration::from_millis(cfg.callback_timeout_ms),
                Duration::from_millis(cfg.key_hook_timeout_ms),
            );
            let dirs: Vec<std::path::PathBuf> = settings.paths().plugin_dirs().to_vec();
            for error in host.load_all(&dirs, &cfg) {
                log::warn!("plugin: {error}");
            }
            host
        } else {
            log::debug!("plugins are disabled in config");
            PluginHost::disabled()
        };

        let keymap = build_keymap(&settings, &plugins);
        let (layout, _first_pane) = Layout::new();
        let (events_tx, events_rx) = crossbeam_channel::unbounded();

        let waker: Arc<dyn Fn() + Send + Sync> = {
            let proxy = proxy.clone();
            Arc::new(move || {
                // Failure means the loop is gone; the PTY thread will notice when
                // its channel closes.
                let _ = proxy.send_event(UserEvent::Wakeup);
            })
        };

        let clipboard = match arboard::Clipboard::new() {
            Ok(c) => Some(c),
            Err(e) => {
                // Not fatal: everything except copy and paste still works.
                log::warn!("clipboard unavailable: {e}");
                None
            }
        };

        let mut app = App {
            settings,
            keymap,
            layout,
            sessions: HashMap::new(),
            fonts: None,
            renderer: None,
            window: None,
            gpu: None,
            events_tx,
            events_rx,
            waker,
            proxy,
            instances: Vec::with_capacity(8192),
            frame: None,
            modifiers: ModifiersState::empty(),
            mouse: (0.0, 0.0),
            selecting: None,
            dragging: None,
            hovered_button: None,
            last_title_click: None,
            hovered_tab: None,
            hovered_close: false,
            panel: None,
            panel_body: None,
            dragging_tab: None,
            toasts: Vec::new(),
            blink_on: true,
            blink_at: Instant::now(),
            last_input: Instant::now(),
            titles: HashMap::new(),
            activity: std::collections::HashSet::new(),
            clipboard,
            plugins,
            exit_requested: false,
        };

        event_loop.run_app(&mut app).context("event loop failed")?;
        Ok(())
    }

    // --- geometry ---------------------------------------------------------

    fn cell_size(&self) -> CellSize {
        match &self.fonts {
            Some(f) => {
                let m = f.metrics();
                CellSize {
                    width: m.width,
                    height: m.height,
                }
            }
            None => BOOTSTRAP_CELL,
        }
    }

    /// Height of the tab bar, or zero when it should be hidden.
    ///
    /// Hidden with a single tab unless the user asked otherwise: a permanent strip
    /// costs a row of terminal for no information when there is nothing to switch
    /// between.
    fn tab_bar_height(&self) -> u32 {
        let cfg = self.settings.config();
        if self.layout.tab_count() <= 1 && !cfg.window.always_show_tab_bar {
            return 0;
        }
        // Sized from the font so the strip scales with the text rather than being a
        // fixed pixel height that looks wrong at other sizes.
        self.cell_size().height + CHROME_PADDING * 2
    }

    /// Height of the status bar, or zero when no plugin is contributing anything.
    fn status_bar_height(&self) -> u32 {
        if self.plugins.status_segments().is_empty() {
            return 0;
        }
        self.cell_size().height + CHROME_PADDING
    }

    fn layout_options(&self) -> LayoutOptions {
        let cfg = self.settings.config();
        LayoutOptions {
            padding_x: cfg.window.padding.x,
            padding_y: cfg.window.padding.y,
            center_grid: cfg.window.center_grid,
            divider_width: cfg.window.split_divider_width as u32,
            tab_bar_height: self.tab_bar_height(),
            status_bar_height: self.status_bar_height(),
            tab_width: TAB_WIDTH,
            min_tab_width: MIN_TAB_WIDTH,
            buttons: self.chrome_buttons(),
            cell: self.cell_size(),
        }
    }

    /// Buttons for the tab strip, in right-to-left order.
    ///
    /// The corner radius actually in effect.
    ///
    /// Zero with decorations on: the compositor owns the window's outline then, and
    /// rounding ours inside its square frame would just carve holes in the corners.
    fn corner_radius(cfg: &tuz_config::Config) -> f32 {
        if cfg.window.decorations {
            0.0
        } else {
            cfg.window.corner_radius.max(0.0)
        }
    }

    /// The stretch of title bar occupied by neither a tab nor a button.
    ///
    /// This is both where the window title is drawn and what drags the window, so the
    /// two are deliberately the same rect: whatever looks like empty bar is grabbable.
    fn title_area(frame: &tuz_layout::Frame) -> Option<Rect> {
        if frame.tab_bar.height == 0 {
            return None;
        }
        let bar = frame.tab_bar;

        // Left edge: past the last tab, and past new-tab if it follows them.
        let mut left = frame.tabs.last().map(|t| t.right()).unwrap_or(bar.x);
        let mut right = bar.right();
        for (button, rect) in &frame.actions {
            if button.leading() {
                left = left.max(rect.right());
            } else {
                right = right.min(rect.x);
            }
        }

        (right > left).then(|| Rect::new(left, bar.y, (right - left) as u32, bar.height))
    }

    /// Window controls appear only without decorations: with them on, the compositor
    /// already draws a set and ours would sit beside a duplicate.
    fn chrome_buttons(&self) -> Vec<ChromeButton> {
        let mut buttons = Vec::with_capacity(7);
        if !self.settings.config().window.decorations {
            buttons.push(ChromeButton::Close);
            buttons.push(ChromeButton::Maximize);
            buttons.push(ChromeButton::Minimize);
        }
        buttons.push(ChromeButton::Settings);
        buttons.push(ChromeButton::SplitDown);
        buttons.push(ChromeButton::SplitRight);
        buttons.push(ChromeButton::NewTab);
        buttons
    }

    /// The label for a tab: its explicit title, else the focused pane's program
    /// title, else a positional fallback.
    fn tab_title(&self, index: usize) -> String {
        let Some(tab) = self.layout.tabs().get(index) else {
            return String::new();
        };
        if let Some(title) = &tab.title {
            return title.clone();
        }
        if let Some(title) = self.titles.get(&tab.focus()) {
            if !title.is_empty() {
                return title.clone();
            }
        }
        // Numbered from 1, matching how `select_tab_<n>` is written in config.
        format!("{}", index + 1)
    }

    /// Recompute pane geometry and push the new grid sizes to every PTY.
    fn relayout(&mut self) {
        // Nothing to lay out once the last tab is gone, and `Layout` indexes its
        // active tab unconditionally, so asking would panic.
        if self.layout.is_empty() {
            self.frame = None;
            return;
        }
        let (w, h) = self.gpu.as_ref().map_or((1, 1), |g| g.size());
        let opts = self.layout_options();
        let frame = self.layout.compute(Rect::from_size(w, h), &opts);

        let cell = self.cell_size();
        for pane in &frame.panes {
            if let Some(session) = self.sessions.get_mut(&pane.pane) {
                session.resize(TermSize::new(
                    pane.cols,
                    pane.rows,
                    cell.width as u16,
                    cell.height as u16,
                ));
            }
        }
        self.frame = Some(frame);
    }

    /// Start a shell for a pane that does not have one yet.
    fn ensure_session(&mut self, pane: PaneId) {
        if self.sessions.contains_key(&pane) {
            return;
        }
        // Use the pane's real grid if layout has run; otherwise the configured
        // default, which the next relayout corrects.
        let cell = self.cell_size();
        let size = self
            .frame
            .as_ref()
            .and_then(|f| f.pane(pane))
            .map(|g| TermSize::new(g.cols, g.rows, cell.width as u16, cell.height as u16))
            .unwrap_or_else(|| {
                let cfg = self.settings.config();
                TermSize::new(
                    cfg.window.columns,
                    cfg.window.rows,
                    cell.width as u16,
                    cell.height as u16,
                )
            });

        match Session::spawn(
            pane,
            self.settings.config(),
            size,
            self.events_tx.clone(),
            self.waker.clone(),
        ) {
            Ok(session) => {
                self.sessions.insert(pane, session);
            }
            Err(e) => {
                log::error!("{pane}: failed to start a shell: {e}");
                // Remove the pane again rather than leaving a permanently blank
                // one the user cannot interact with.
                if matches!(self.layout.close_pane(pane), CloseOutcome::Emptied) {
                    self.exit_requested = true;
                }
            }
        }
    }

    fn focused_session(&self) -> Option<&Session> {
        if self.layout.is_empty() {
            return None;
        }
        self.sessions.get(&self.layout.active_pane())
    }

    /// Terminal modes of the focused pane, for key and mouse encoding.
    fn focused_mode(&self) -> tuz_core::TermMode {
        self.focused_session()
            .map(|s| *s.term().lock().mode())
            .unwrap_or_else(tuz_core::TermMode::empty)
    }

    // --- rendering --------------------------------------------------------

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn redraw(&mut self) {
        // The window is closing; the layout has no active tab left to query.
        if self.layout.is_empty() {
            return;
        }

        // Give plugins a chance to refresh their status segments before the frame is
        // built. Skipped entirely when nothing is loaded, so the common case pays
        // nothing.
        if !self.plugins.is_empty() {
            let had_status = !self.plugins.status_segments().is_empty();
            let commands = self.plugins.dispatch(&PluginEvent::StatusBarRender);
            self.apply_plugin_commands(commands);
            // A segment appearing or disappearing changes the reserved height, which
            // changes every pane's grid.
            if had_status != !self.plugins.status_segments().is_empty() {
                self.relayout();
            }
        }
        let Some(frame) = self.frame.clone() else {
            return;
        };
        if self.gpu.is_none() || self.fonts.is_none() || self.renderer.is_none() {
            return;
        }

        // Everything derived from `&self` is read before the `&mut` field borrows
        // below, because a method call like `self.cell_size()` borrows all of
        // `self` and would conflict with holding `&mut self.gpu`.
        let cell = self.cell_size();
        let active = self.layout.active_pane();
        let blink_on = self.blink_on;
        let active_tab = self.layout.active_index();

        // Chrome text is gathered here, before the `&mut` field borrows below, since
        // building a label needs `&self`.
        let tab_titles: Vec<String> = (0..self.layout.tab_count())
            .map(|i| self.tab_title(i))
            .collect();

        // What the system title bar would have shown, for the strip to show instead:
        // whatever the focused program set, falling back to the configured name.
        let window_title: String = self
            .settings
            .config()
            .window
            .dynamic_title
            .then(|| self.titles.get(&self.layout.active_pane()).cloned())
            .flatten()
            .unwrap_or_else(|| self.settings.config().window.title.clone());

        // A maximized window is flush with the screen edges, and rounding there just
        // punches holes showing the desktop through the corners.
        let maximized = self.window.as_ref().map(|w| w.is_maximized()).unwrap_or(false);
        let tab_activity: Vec<bool> = self
            .layout
            .tabs()
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                // A tab shows activity only when it is not the one you are looking at.
                i != active_tab && tab.panes().iter().any(|p| self.activity.contains(p))
            })
            .collect();
        let status_items = self.plugins.status_segments();
        // Built before the `&mut` borrows below, and fade computed here so the
        // renderer stays free of timing logic.
        let toasts: Vec<tuz_render::Toast<'_>> = self
            .toasts
            .iter()
            .map(|t| {
                let elapsed = t.shown_at.elapsed();
                let opacity = if elapsed < TOAST_HOLD {
                    1.0
                } else {
                    let into_fade = (elapsed - TOAST_HOLD).as_secs_f32();
                    1.0 - (into_fade / TOAST_FADE.as_secs_f32()).clamp(0.0, 1.0)
                };
                tuz_render::Toast {
                    text: &t.text,
                    accent: match t.level {
                        tuz_plugin_api::NotifyLevel::Error => {
                            theme_error_color(self.settings.theme())
                        }
                        tuz_plugin_api::NotifyLevel::Warn => self.settings.theme().normal.yellow,
                        tuz_plugin_api::NotifyLevel::Info => self.settings.theme().normal.blue,
                    },
                    opacity,
                }
            })
            .collect();
        let hovered_button = self.hovered_button;
        let hovered_tab = self.hovered_tab;
        let hovered_close = self.hovered_close;
        // Widgets are built here, before the `&mut` field borrows, because building a
        // row needs to read the config.
        let panel_widgets: Option<Vec<Widget>> = self
            .panel
            .as_ref()
            .map(|panel| panel.widgets(self.settings.config()));
        let srgb = self
            .gpu
            .as_ref()
            .map(|g| g.surface_format().is_srgb())
            .unwrap_or(false);

        // Disjoint field borrows: the compiler tracks these independently, which
        // is what lets the renderer read fonts while the GPU is borrowed mutably.
        let App {
            gpu,
            fonts,
            renderer,
            settings,
            sessions,
            instances,
            ..
        } = self;
        let gpu = gpu.as_mut().expect("checked above");
        let fonts = fonts.as_mut().expect("checked above");
        let renderer = renderer.as_mut().expect("checked above");

        let cfg = settings.config();
        let theme = settings.theme();
        let colors = ColorSpace {
            srgb,
            opacity: cfg.window.opacity,
        };

        // Build instances for every visible pane, remembering each pane's range so
        // it can be drawn under its own scissor rect.
        instances.clear();
        let mut ranges: Vec<(tuz_layout::Rect, std::ops::Range<u32>)> = Vec::new();

        // With rounded corners the surface is cleared to fully transparent and the
        // window's own background is this quad, so the pixels outside the curve stay
        // transparent and the compositor shows what is behind. Clearing to the
        // background color instead would paint square corners no later quad can undo.
        let radius = if maximized { 0.0 } else { Self::corner_radius(cfg) };
        if radius > 0.0 {
            let window = Rect::from_size(gpu.size().0, gpu.size().1);
            instances.push(Instance::rounded(
                0.0,
                0.0,
                window.width as f32,
                window.height as f32,
                colors.convert(theme.background),
                radius,
                tuz_render::instance::FLAG_ROUND_TOP | tuz_render::instance::FLAG_ROUND_BOTTOM,
            ));
            ranges.push((window, 0..1));
        }

        for geom in &frame.panes {
            let Some(session) = sessions.get(&geom.pane) else {
                continue;
            };
            let focused = geom.pane == active;

            let snapshot = {
                // Hold the terminal lock only for the copy, never across
                // rasterization or GPU work.
                let term = session.term().lock();
                // An unfocused pane shows a steady cursor: blinking every split at
                // once is visually noisy and hides which one has focus.
                tuz_core::snapshot(&term, theme, cfg, focused, blink_on || !focused)
            };

            let start = instances.len() as u32;
            build_pane(
                instances,
                &snapshot,
                fonts,
                PaneGeometry {
                    origin: (geom.content.x as f32, geom.content.y as f32),
                    cell_width: cell.width as f32,
                    cell_height: cell.height as f32,
                },
                colors,
            );
            let end = instances.len() as u32;
            if end > start {
                ranges.push((geom.rect, start..end));
            }
        }

        // Split dividers, drawn unclipped over the whole window.
        let divider_start = instances.len() as u32;
        for divider in &frame.dividers {
            instances.push(Instance::solid(
                divider.rect.x as f32,
                divider.rect.y as f32,
                divider.rect.width as f32,
                divider.rect.height as f32,
                colors.convert(theme.split_divider()),
            ));
        }
        // Chrome shares the divider range: both are drawn unclipped over the whole
        // window, so they need no scissor of their own.
        if frame.tab_bar.height > 0 {
            let labels: Vec<tuz_render::TabLabel<'_>> = tab_titles
                .iter()
                .enumerate()
                .map(|(i, title)| tuz_render::TabLabel {
                    title,
                    active: i == active_tab,
                    has_activity: tab_activity.get(i).copied().unwrap_or(false),
                    // Only the hovered tab offers a close button: a permanent × on
                    // every tab is noise, and a stray click costs a running shell.
                    show_close: hovered_tab == Some(i),
                    close_hovered: hovered_tab == Some(i) && hovered_close,
                })
                .collect();
            tuz_render::draw_tab_bar(
                instances,
                fonts,
                frame.tab_bar,
                &frame.tabs,
                &frame.tab_close,
                &labels,
                theme,
                colors,
                radius,
            );

            // Without decorations the strip is the title bar, so it carries the
            // window title. With them, the compositor already shows it above us and a
            // second copy is just noise.
            if !cfg.window.decorations {
                if let Some(area) = Self::title_area(&frame) {
                    tuz_render::chrome::draw_window_title(
                        instances,
                        fonts,
                        area,
                        &window_title,
                        theme,
                        colors,
                    );
                }
            }

            tuz_render::chrome::draw_chrome_buttons(
                instances,
                fonts,
                &frame.actions,
                hovered_button,
                theme,
                colors,
            );

            // After the strip, so it overlaps the tabs below rather than being
            // painted over by them.
            if let Some(button) = hovered_button {
                if let Some((_, anchor)) = frame.actions.iter().find(|(b, _)| *b == button) {
                    let window = Rect::from_size(gpu.size().0, gpu.size().1);
                    tuz_render::draw_tooltip(
                        instances, fonts, button, *anchor, window, theme, colors,
                    );
                }
            }
        }

        if frame.status_bar.height > 0 {
            let items: Vec<tuz_render::StatusItem<'_>> = status_items
                .iter()
                .map(|segment| tuz_render::StatusItem {
                    text: &segment.text,
                    foreground: segment.foreground.as_deref(),
                    background: segment.background.as_deref(),
                })
                .collect();
            tuz_render::draw_status_bar(instances, fonts, frame.status_bar, &items, theme, colors);
        }
        // The panel goes last so it sits over terminal content and chrome alike.
        let mut panel_body: Option<Rect> = None;
        let mut widget_start = 0u32;
        let mut widget_end = 0u32;
        if let (Some(widgets), Some(panel)) = (panel_widgets, self.panel.as_mut()) {
            let (w, h) = SettingsPanel::preferred_size(cell.width, cell.height);
            let window = Rect::from_size(gpu.size().0, gpu.size().1);
            let rect = tuz_render::center_panel(window, w, h);

            tuz_render::draw_panel_frame(instances, window, rect, theme, colors);
            let body = tuz_render::draw_panel_title(
                instances,
                fonts,
                rect,
                "Tuzminal Settings",
                theme,
                colors,
            );
            panel.ui.layout(&widgets, body, cell.height);
            panel_body = Some(body);

            // Rows are clipped to the body, so a scrolled list cannot draw over the
            // title bar or spill outside the panel.
            widget_start = instances.len() as u32;
            tuz_render::draw_widgets(instances, fonts, &panel.ui, theme, colors);
            widget_end = instances.len() as u32;

            tuz_render::draw_scrollbar(instances, &panel.ui, body, theme, colors);
        }
        if !toasts.is_empty() {
            let window = Rect::from_size(gpu.size().0, gpu.size().1);
            tuz_render::draw_toasts(instances, fonts, &toasts, window, theme, colors);
        }
        let chrome_end = instances.len() as u32;

        let (width, height) = gpu.size();
        renderer.set_viewport(gpu.queue(), width, height);
        // Uploaded after instance building, because rasterizing glyphs during that
        // pass is what dirties the atlas.
        renderer.upload_atlas(gpu.device(), gpu.queue(), fonts.atlas_mut());
        renderer.upload_instances(gpu.device(), gpu.queue(), instances);

        // A rounded window paints its own background quad, so the surface must clear
        // to nothing at all — anything else fills the corners back in.
        let clear = if radius > 0.0 {
            wgpu::Color::TRANSPARENT
        } else {
            gpu.resolve_color(theme.background, cfg.window.opacity)
        };
        let outcome = gpu.render(clear, |pass| {
            for (rect, range) in &ranges {
                // Clip to the pane so an overhanging glyph — an italic descender,
                // a wide emoji in the last column — cannot bleed into a neighbour.
                pass.set_scissor_rect(
                    rect.x.max(0) as u32,
                    rect.y.max(0) as u32,
                    rect.width.min(width),
                    rect.height.min(height),
                );
                renderer.draw(pass, range.clone());
            }
            if chrome_end > divider_start {
                pass.set_scissor_rect(0, 0, width, height);
                match panel_body {
                    // Split around the widget range so only it is clipped: the panel
                    // frame, title and scrollbar must draw unclipped.
                    Some(body) if widget_end > widget_start => {
                        renderer.draw(pass, divider_start..widget_start);
                        pass.set_scissor_rect(
                            body.x.max(0) as u32,
                            body.y.max(0) as u32,
                            body.width.min(width),
                            body.height.min(height),
                        );
                        renderer.draw(pass, widget_start..widget_end);
                        pass.set_scissor_rect(0, 0, width, height);
                        renderer.draw(pass, widget_end..chrome_end);
                    }
                    _ => renderer.draw(pass, divider_start..chrome_end),
                }
            }
        });

        // The field borrows end here, so `self` is usable again.
        self.panel_body = panel_body;

        match outcome {
            FrameOutcome::Presented | FrameOutcome::Skipped => {}
            FrameOutcome::Redraw => self.request_redraw(),
            FrameOutcome::Fatal => {
                log::error!("the GPU device is unusable; exiting");
                self.exit_requested = true;
            }
        }
    }

    // --- config -----------------------------------------------------------

    fn reload_config(&mut self) {
        match self.settings.reload() {
            ReloadOutcome::Unchanged => log::debug!("config unchanged"),
            ReloadOutcome::Failed(e) => {
                log::warn!("config reload failed, keeping previous settings:\n{e}")
            }
            ReloadOutcome::Applied(actions) => {
                log::info!("config reloaded");

                if actions.rebind_keys {
                    self.keymap = build_keymap(&self.settings, &self.plugins);
                }
                if actions.rebuild_fonts {
                    self.rebuild_fonts();
                }
                if actions.resize_scrollback {
                    let lines = self.settings.config().scrollback.lines;
                    for session in self.sessions.values() {
                        session.set_scrollback(lines);
                    }
                }
                if actions.reconfigure_surface {
                    if let Some(gpu) = self.gpu.as_mut() {
                        gpu.reconfigure(self.settings.config());
                    }
                }
                if actions.relayout || actions.rebuild_fonts {
                    self.relayout();
                }
                if let Some(w) = &self.window {
                    let cfg = self.settings.config();
                    if !cfg.window.dynamic_title {
                        w.set_title(&cfg.window.title);
                    }
                    w.set_decorations(cfg.window.decorations);
                }
                for field in &actions.restart_required {
                    log::warn!("`{field}` only takes effect after a restart");
                }
                self.request_redraw();
            }
        }
    }

    /// Reload fonts after a size or family change.
    fn rebuild_fonts(&mut self) {
        let scale = self
            .window
            .as_ref()
            .map(|w| w.scale_factor())
            .unwrap_or(1.0);

        match FontSystem::new(&self.settings.config().font, scale) {
            Ok(fonts) => {
                self.fonts = Some(fonts);
                // The atlas is a different object now, so the renderer's texture
                // must be rebuilt to match.
                if let (Some(gpu), Some(fonts)) = (self.gpu.as_ref(), self.fonts.as_ref()) {
                    self.renderer = Some(Renderer::new(
                        gpu.device(),
                        gpu.surface_format(),
                        fonts.atlas(),
                    ));
                }
            }
            // Keeping the old font is much better than a terminal that renders
            // nothing because the user typo'd a family name.
            Err(e) => log::warn!("keeping the previous font; could not load the new one: {e}"),
        }
    }

    // --- actions ----------------------------------------------------------

    /// Perform a bound action. Returns false to request application exit.
    fn dispatch(&mut self, action: Action) -> bool {
        use Action::*;

        match action {
            Quit => return false,
            ReloadConfig => {
                self.reload_config();
                self.notify_plugins(PluginEvent::ConfigReload);
            }
            OpenSettings => {
                // Toggles: pressing the binding again closes it, which is what every
                // other panel-style UI does.
                if self.panel.is_some() {
                    self.close_settings();
                } else {
                    self.open_settings();
                }
            }

            SplitRight => self.split(Direction::Right),
            SplitLeft => self.split(Direction::Left),
            SplitUp => self.split(Direction::Up),
            SplitDown => self.split(Direction::Down),

            ClosePane => return self.close_pane(self.layout.active_pane()),

            FocusLeft => self.focus(Direction::Left),
            FocusRight => self.focus(Direction::Right),
            FocusUp => self.focus(Direction::Up),
            FocusDown => self.focus(Direction::Down),
            FocusNextPane | FocusPrevPane => {
                let panes = self.layout.visible_panes();
                if panes.len() > 1 {
                    let current = panes
                        .iter()
                        .position(|p| *p == self.layout.active_pane())
                        .unwrap_or(0);
                    let next = if action == FocusNextPane {
                        (current + 1) % panes.len()
                    } else {
                        (current + panes.len() - 1) % panes.len()
                    };
                    self.layout.focus_pane(panes[next]);
                    self.request_redraw();
                }
            }

            ResizeLeft => self.resize_split(Direction::Left),
            ResizeRight => self.resize_split(Direction::Right),
            ResizeUp => self.resize_split(Direction::Up),
            ResizeDown => self.resize_split(Direction::Down),

            NewTab => {
                let pane = self.layout.new_tab();
                // Two passes: the first sizes the new pane, and going from one tab to
                // two makes the strip appear, which shrinks every pane's grid.
                self.relayout();
                self.ensure_session(pane);
                self.relayout();
                self.request_redraw();
            }
            CloseTab => {
                let idx = self.layout.active_index();
                if let Some(panes) = self.layout.close_tab(idx) {
                    for pane in panes {
                        self.drop_session(pane);
                    }
                }
                if self.layout.is_empty() {
                    return false;
                }
                self.relayout();
                self.request_redraw();
            }
            NextTab => {
                self.layout.next_tab();
                self.clear_activity_for_active_tab();
                self.relayout();
                self.request_redraw();
            }
            PrevTab => {
                self.layout.prev_tab();
                self.clear_activity_for_active_tab();
                self.relayout();
                self.request_redraw();
            }
            SelectTab(n) => {
                if self.layout.select_tab((n as usize).saturating_sub(1)) {
                    self.clear_activity_for_active_tab();
                    self.relayout();
                    self.request_redraw();
                }
            }

            Copy => self.copy_selection(),
            Paste => self.paste(),
            SelectAll => {
                if let Some(session) = self.focused_session() {
                    session.select_all();
                }
                self.request_redraw();
            }

            IncreaseFontSize => self.adjust_font_size(1.0),
            DecreaseFontSize => self.adjust_font_size(-1.0),
            ResetFontSize => {
                let default = tuz_config::Font::default().size;
                self.settings.modify(|c| c.font.size = default);
                self.rebuild_fonts();
                self.relayout();
                self.request_redraw();
            }
            ToggleFullscreen => {
                if let Some(w) = &self.window {
                    let next = match w.fullscreen() {
                        Some(_) => None,
                        None => Some(winit::window::Fullscreen::Borderless(None)),
                    };
                    w.set_fullscreen(next);
                }
            }

            ScrollLineUp => self.scroll(1),
            ScrollLineDown => self.scroll(-1),
            ScrollPageUp => self.scroll_page(1),
            ScrollPageDown => self.scroll_page(-1),
            ScrollToTop => {
                if let Some(s) = self.focused_session() {
                    s.scroll_to_top();
                }
                self.request_redraw();
            }
            ScrollToBottom => {
                if let Some(s) = self.focused_session() {
                    s.scroll_to_bottom();
                }
                self.request_redraw();
            }
            ClearScrollback => {
                if let Some(s) = self.focused_session() {
                    s.clear_scrollback();
                }
                self.request_redraw();
            }

            SendText(text) => {
                if let Some(s) = self.focused_session() {
                    s.write(text.into_bytes());
                }
            }
            Plugin(name) => {
                // Strip the `plugin.` prefix the host added when registering.
                let bare = name.split_once('.').map(|(_, c)| c).unwrap_or(&name);
                self.notify_plugins(PluginEvent::Command {
                    name: bare.to_owned(),
                    args: Vec::new(),
                });
            }
        }
        true
    }

    /// Notify plugins of an event and apply whatever they ask for.
    fn notify_plugins(&mut self, event: PluginEvent) {
        if self.plugins.is_empty() {
            return;
        }
        let commands = self.plugins.dispatch(&event);
        self.apply_plugin_commands(commands);
    }

    /// Translate plugin commands into terminal actions.
    ///
    /// Every command is re-checked here rather than trusted: a plugin naming a pane
    /// that no longer exists, or asking to close the last one, must be handled the
    /// same way a keybinding would be.
    fn apply_plugin_commands(&mut self, commands: Vec<PluginCommand>) {
        for command in commands {
            match command {
                // Registrations are handled by the host itself at load time.
                PluginCommand::RegisterCommand { .. }
                | PluginCommand::RegisterKeybind { .. }
                | PluginCommand::SetStatusSegments { .. } => {}

                PluginCommand::Split { direction } => self.split(to_direction(direction)),
                PluginCommand::NewTab => {
                    let pane = self.layout.new_tab();
                    self.relayout();
                    self.ensure_session(pane);
                    self.relayout();
                    self.request_redraw();
                }
                PluginCommand::ClosePane { pane } => {
                    let target = pane
                        .map(|p| PaneId(p.0))
                        .unwrap_or_else(|| self.layout.active_pane());
                    if !self.close_pane(target) {
                        self.exit_requested = true;
                    }
                }
                PluginCommand::Focus { direction } => self.focus(to_direction(direction)),
                PluginCommand::FocusPane { pane } => {
                    self.layout.focus_pane(PaneId(pane.0));
                    self.request_redraw();
                }
                PluginCommand::SelectTab { index } => {
                    if self.layout.select_tab(index as usize) {
                        self.relayout();
                        self.request_redraw();
                    }
                }
                PluginCommand::Resize { direction, delta } => {
                    if self
                        .layout
                        .resize_active(to_direction(direction), delta)
                        .is_some()
                    {
                        self.relayout();
                        self.request_redraw();
                    }
                }
                PluginCommand::SendText { pane, text } => {
                    let target = pane
                        .map(|p| PaneId(p.0))
                        .unwrap_or_else(|| self.layout.active_pane());
                    if let Some(session) = self.sessions.get(&target) {
                        session.write(text.into_bytes());
                    }
                }
                PluginCommand::Notify { message, level } => {
                    match level {
                        tuz_plugin_api::NotifyLevel::Error => log::error!("plugin: {message}"),
                        tuz_plugin_api::NotifyLevel::Warn => log::warn!("plugin: {message}"),
                        tuz_plugin_api::NotifyLevel::Info => log::info!("plugin: {message}"),
                    }
                    self.notify(message, level);
                }
                PluginCommand::SetConfigOverlay { toml } => {
                    match self.settings.apply_overlay(&toml) {
                        Ok(actions) => {
                            log::debug!("applied a plugin config overlay");
                            self.apply_reload_actions(&actions);
                        }
                        // Reported rather than silently ignored: a plugin author
                        // needs to know their overlay was rejected, and the user
                        // needs to know why their setting did not change.
                        Err(e) => {
                            log::warn!("rejected a plugin config overlay: {e}");
                            self.notify(
                                format!("Plugin config overlay rejected: {e}"),
                                tuz_plugin_api::NotifyLevel::Warn,
                            );
                        }
                    }
                }
                PluginCommand::ReloadConfig => self.reload_config(),
                PluginCommand::Quit => self.exit_requested = true,
                // `Command` is non_exhaustive, so unknown variants are ignored
                // rather than breaking the build on an API addition.
                _ => log::debug!("unhandled plugin command"),
            }
        }
    }

    /// Forget activity for the tab now on screen: you are looking at it.
    fn clear_activity_for_active_tab(&mut self) {
        for pane in self.layout.visible_panes() {
            self.activity.remove(&pane);
        }
    }

    /// Open the settings panel, gathering the option lists once.
    fn open_settings(&mut self) {
        if self.panel.is_some() {
            return;
        }
        // Enumerating fonts is not free and the list cannot change while the panel is
        // up, so it is gathered here rather than every frame.
        let families = self
            .fonts
            .as_ref()
            .map(|f| f.monospace_families())
            .unwrap_or_default();
        let themes = crate::settings::theme_names(self.settings.paths());

        self.panel = Some(SettingsPanel::open(
            self.settings.config(),
            families,
            themes,
        ));
        self.request_redraw();
    }

    fn close_settings(&mut self) {
        // Closing keeps any unsaved changes for the session, matching how the
        // font-size keybindings already behave.
        self.panel = None;
        self.request_redraw();
    }

    /// Apply a panel action and rebuild whatever the config change requires.
    fn handle_panel_action(&mut self, action: tuz_ui::UiAction) {
        let Some(mut panel) = self.panel.take() else {
            return;
        };

        let mut next = self.settings.config().clone();
        let outcome = panel.apply(action, &mut next);

        match outcome {
            PanelOutcome::Continue => {}
            PanelOutcome::Changed => {
                // Routed through `modify` so the same validation and diffing that
                // guards keybindings and config reloads applies here too.
                let actions = self.settings.modify(|c| *c = next);
                self.apply_reload_actions(&actions);
            }
            PanelOutcome::Save => match self.settings.save(panel.snapshot()) {
                Ok(path) => {
                    log::info!("saved settings to {}", path.display());
                    let saved = self.settings.config().clone();
                    panel.mark_saved(&saved);
                }
                Err(e) => log::error!("could not save settings: {e}"),
            },
            PanelOutcome::Close => {
                self.panel = None;
                self.request_redraw();
                return;
            }
        }

        self.panel = Some(panel);
        self.request_redraw();
    }

    /// Rebuild whatever a config change requires.
    ///
    /// Extracted from `reload_config` so a panel edit and a file edit take exactly the
    /// same path — otherwise the two drift and one of them forgets to resize PTYs.
    fn apply_reload_actions(&mut self, actions: &tuz_config::ReloadActions) {
        if actions.rebind_keys {
            self.keymap = build_keymap(&self.settings, &self.plugins);
        }
        if actions.rebuild_fonts {
            self.rebuild_fonts();
        }
        if actions.resize_scrollback {
            let lines = self.settings.config().scrollback.lines;
            for session in self.sessions.values() {
                session.set_scrollback(lines);
            }
        }
        if actions.reconfigure_surface {
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.reconfigure(self.settings.config());
            }
        }
        if let Some(w) = &self.window {
            let cfg = self.settings.config();
            if !cfg.window.dynamic_title {
                w.set_title(&cfg.window.title);
            }
            w.set_decorations(cfg.window.decorations);
        }
        // Always relayout: the tab strip and status bar heights depend on config, so
        // a change with no `relayout` flag can still move every pane.
        self.relayout();
        for field in &actions.restart_required {
            log::warn!("`{field}` only takes effect after a restart");
        }
    }

    /// Show a transient message.
    fn notify(&mut self, text: String, level: tuz_plugin_api::NotifyLevel) {
        self.toasts.push(Notification {
            text,
            level,
            shown_at: Instant::now(),
        });
        // Oldest first out, so a burst shows the most recent rather than the first
        // four and then nothing.
        while self.toasts.len() > MAX_TOASTS {
            self.toasts.remove(0);
        }
        self.request_redraw();
    }

    /// Drop expired toasts, returning when the next one changes appearance.
    ///
    /// Returned so the event loop can sleep exactly until the next fade step instead
    /// of polling, which is the same approach the cursor blink uses.
    fn update_toasts(&mut self) -> Option<Instant> {
        let lifetime = TOAST_HOLD + TOAST_FADE;
        let before = self.toasts.len();
        self.toasts.retain(|t| t.shown_at.elapsed() < lifetime);
        if self.toasts.len() != before {
            self.request_redraw();
        }

        let next = self.toasts.iter().map(|t| t.shown_at + lifetime).min();

        // While any toast is fading it needs a frame per step, so wake sooner.
        if self
            .toasts
            .iter()
            .any(|t| t.shown_at.elapsed() >= TOAST_HOLD)
        {
            return Some(Instant::now() + Duration::from_millis(50));
        }
        next
    }

    /// Act on a tab strip button.
    /// Handle a left press on the empty stretch of the title bar.
    ///
    /// A second press within [`DOUBLE_CLICK`] and a few pixels toggles maximize;
    /// otherwise the press begins a window drag. The order matters: `drag_window`
    /// hands the pointer to the compositor and we stop seeing events, so the
    /// double-click has to be decided first.
    fn press_title_bar(&mut self, x: i32, y: i32) {
        if self.settings.config().window.decorations {
            return;
        }
        let now = Instant::now();
        let double = self
            .last_title_click
            .map(|(at, px, py)| {
                now.duration_since(at) < DOUBLE_CLICK
                    && (x - px).abs() <= DOUBLE_CLICK_SLOP
                    && (y - py).abs() <= DOUBLE_CLICK_SLOP
            })
            .unwrap_or(false);

        if double {
            self.last_title_click = None;
            if let Some(w) = &self.window {
                w.set_maximized(!w.is_maximized());
            }
            return;
        }

        self.last_title_click = Some((now, x, y));
        if let Some(w) = &self.window {
            // Failure is normal rather than exceptional: some platforms and some
            // compositors refuse, and the only sane response is to leave the window
            // where it is.
            if let Err(e) = w.drag_window() {
                log::debug!("compositor declined a window drag: {e}");
            }
        }
    }

    fn press_chrome_button(&mut self, button: ChromeButton) {
        match button {
            ChromeButton::NewTab => {
                self.dispatch(Action::NewTab);
            }
            ChromeButton::Settings => self.open_settings(),
            ChromeButton::SplitRight => self.split(Direction::Right),
            ChromeButton::SplitDown => self.split(Direction::Down),
            ChromeButton::Minimize => {
                if let Some(w) = &self.window {
                    w.set_minimized(true);
                }
            }
            ChromeButton::Maximize => {
                if let Some(w) = &self.window {
                    w.set_maximized(!w.is_maximized());
                }
            }
            ChromeButton::Close => self.exit_requested = true,
        }
    }

    fn split(&mut self, dir: Direction) {
        if let Some(pane) = self.layout.split(dir) {
            // Layout first so the new session is spawned with the right grid, then
            // again so the resized sibling's PTY learns its new size.
            self.relayout();
            self.ensure_session(pane);
            self.relayout();
            self.request_redraw();
        }
    }

    /// Close a pane and reap its shell. Returns false when the app should exit.
    fn close_pane(&mut self, pane: PaneId) -> bool {
        let outcome = self.layout.close_pane(pane);
        self.drop_session(pane);

        match outcome {
            CloseOutcome::Emptied => {
                // Set immediately rather than relying on the caller: several event
                // handlers run before the loop next checks, and any of them
                // touching the now-empty layout would panic.
                self.exit_requested = true;
                false
            }
            CloseOutcome::NotFound => true,
            _ => {
                self.relayout();
                self.request_redraw();
                true
            }
        }
    }

    /// Shut down a pane's shell, waiting for the PTY thread to finish.
    fn drop_session(&mut self, pane: PaneId) {
        if let Some(mut session) = self.sessions.remove(&pane) {
            session.shutdown();
        }
    }

    fn focus(&mut self, dir: Direction) {
        let Some(frame) = self.frame.clone() else {
            return;
        };
        if self.layout.focus_direction(dir, &frame) {
            // Both panes change appearance: one gains the focused cursor, the
            // other loses it.
            self.request_redraw();
        }
    }

    fn resize_split(&mut self, dir: Direction) {
        if self.layout.resize_active(dir, RESIZE_STEP).is_some() {
            self.relayout();
            self.request_redraw();
        }
    }

    fn scroll(&mut self, lines: i32) {
        if let Some(s) = self.focused_session() {
            s.scroll(lines);
        }
        self.request_redraw();
    }

    fn scroll_page(&mut self, pages: i32) {
        if let Some(s) = self.focused_session() {
            s.scroll_page(pages);
        }
        self.request_redraw();
    }

    fn adjust_font_size(&mut self, delta: f32) {
        let actions = self.settings.modify(|c| c.font.size += delta);
        if actions.is_empty() {
            return;
        }
        self.rebuild_fonts();
        self.relayout();
        self.request_redraw();
    }

    fn copy_selection(&mut self) {
        let Some(text) = self.focused_session().and_then(|s| s.selection_text()) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        match self.clipboard.as_mut() {
            Some(clipboard) => {
                if let Err(e) = clipboard.set_text(text) {
                    log::warn!("copy failed: {e}");
                }
            }
            None => log::warn!("cannot copy: no clipboard available"),
        }
    }

    fn paste(&mut self) {
        let Some(clipboard) = self.clipboard.as_mut() else {
            log::warn!("cannot paste: no clipboard available");
            return;
        };
        let text = match clipboard.get_text() {
            Ok(t) => t,
            Err(e) => {
                log::debug!("nothing to paste: {e}");
                return;
            }
        };
        let mode = self.focused_mode();
        if let Some(session) = self.focused_session() {
            // Bracketed paste when the program asked for it, and the terminator is
            // stripped so pasted text cannot inject keystrokes.
            session.write(encode_paste(&text, mode));
        }
    }

    // --- input ------------------------------------------------------------

    fn on_key(&mut self, event: &winit::event::KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        self.last_input = Instant::now();
        // Any keystroke restarts the blink cycle visible, so typing never happens
        // against an invisible cursor.
        self.blink_on = true;
        self.blink_at = Instant::now();

        let Some(chord) = keys::chord_from(&event.logical_key, self.modifiers) else {
            return;
        };

        // While the panel is open it owns the keyboard. The binding that opened it
        // still works, so the same chord toggles it shut.
        if self.panel.is_some() {
            if self.keymap.lookup(&chord) == Some(&Action::OpenSettings) {
                self.close_settings();
                return;
            }
            // A printable character with no ctrl/alt/super goes into a focused text
            // field. Checked before `panel_key` so a plain `d` types rather than
            // being mistaken for a navigation key.
            if !self.modifiers.control_key()
                && !self.modifiers.alt_key()
                && !self.modifiers.super_key()
            {
                if let Some(text) = event.text.as_deref() {
                    let mut edited = None;
                    if let Some(panel) = self.panel.as_mut() {
                        for c in text.chars() {
                            if let Some(action) = panel.ui.type_char(c) {
                                edited = Some(action);
                            }
                        }
                    }
                    if let Some(action) = edited {
                        self.handle_panel_action(action);
                        return;
                    }
                }
            }

            if let Some(key) = panel_key(&chord) {
                let response = match self.panel.as_mut() {
                    Some(panel) => panel.ui.key(key),
                    None => return,
                };
                // Tabbing to a row below the fold must bring it into view, or the
                // focus ring moves somewhere invisible and the key looks dead.
                if let (Some(panel), Some(body)) = (self.panel.as_mut(), self.panel_body) {
                    panel.ui.scroll_to_focus(body);
                }
                match response {
                    tuz_ui::KeyResponse::Close => self.close_settings(),
                    tuz_ui::KeyResponse::Action(action) => self.handle_panel_action(action),
                    tuz_ui::KeyResponse::Consumed => self.request_redraw(),
                }
            }
            // Anything else is swallowed rather than reaching the shell, so typing at
            // a settings panel cannot run commands behind it.
            return;
        }

        // Plugins get first refusal on real key presses, so a plugin can implement
        // its own modal input. Auto-repeat is excluded: a held key must not run a
        // plugin handler dozens of times a second.
        if !event.repeat && !self.plugins.is_empty() {
            let plugin_event = PluginEvent::Key(tuz_plugin_api::KeyPress {
                chord: chord.to_string(),
                modifiers: tuz_plugin_api::Modifiers {
                    ctrl: chord.mods.ctrl(),
                    shift: chord.mods.shift(),
                    alt: chord.mods.alt(),
                    super_key: chord.mods.super_key(),
                },
            });
            let (outcome, commands) = self.plugins.on_key(&plugin_event);
            self.apply_plugin_commands(commands);
            if outcome == KeyOutcome::Handled {
                self.request_redraw();
                return;
            }
        }

        // A bound chord belongs to the terminal, not the program. Auto-repeat is
        // allowed through for keys that go to the PTY but not for actions, where a
        // held key would spawn a hundred splits.
        if !event.repeat {
            if let Some(action) = self.keymap.lookup(&chord).cloned() {
                log::trace!("{chord} -> {action}");
                if !self.dispatch(action) {
                    self.exit_requested = true;
                }
                return;
            }
        } else if self.keymap.lookup(&chord).is_some() {
            return;
        }

        // Encoded from the raw key, never the normalized chord: see
        // `keys::bytes_for_key` for why that distinction matters.
        let mode = self.focused_mode();
        let mods = keys::modifiers_from(self.modifiers);
        let Some(bytes) =
            keys::bytes_for_key(&event.logical_key, event.text.as_deref(), mods, mode)
        else {
            return;
        };

        let scroll_on_input = self.settings.config().scrollback.scroll_to_bottom_on_input;
        if let Some(session) = self.focused_session() {
            if scroll_on_input {
                // Typing while scrolled back should jump to the prompt, or the
                // user types into output they cannot see.
                session.scroll_to_bottom();
            }
            session.write(bytes);
        }
        self.request_redraw();
    }

    fn on_mouse_button(&mut self, button: winit::event::MouseButton, state: ElementState) {
        use winit::event::MouseButton as WButton;

        let Some(frame) = self.frame.clone() else {
            return;
        };
        let (x, y) = (self.mouse.0 as i32, self.mouse.1 as i32);
        let pressed = state == ElementState::Pressed;

        let button = match button {
            WButton::Left => MouseButton::Left,
            WButton::Right => MouseButton::Right,
            WButton::Middle => MouseButton::Middle,
            _ => return,
        };

        if !pressed {
            self.selecting = None;
            self.dragging = None;
            if let Some(drag) = self.dragging_tab.take() {
                if drag.active && drag.current != drag.origin {
                    log::debug!("moved tab {} to {}", drag.origin, drag.current);
                    self.request_redraw();
                }
            }
        }

        // The panel is modal: a click inside it goes to a widget, and a click outside
        // it dismisses, which is what every overlay does.
        if self.panel.is_some() {
            if !pressed {
                return;
            }
            let inside = self
                .panel
                .as_ref()
                .map(|p| {
                    p.ui.hit(x, y).is_some() || p.ui.placed().iter().any(|w| w.rect.contains(x, y))
                })
                .unwrap_or(false);

            if inside {
                if let Some(action) = self.panel.as_mut().and_then(|p| p.ui.click(x, y)) {
                    self.handle_panel_action(action);
                } else {
                    self.request_redraw();
                }
            }
            return;
        }

        if pressed {
            // Buttons come before tabs: a close button sits inside its tab, so
            // checking the tab first would swallow the click.
            if let Some(button) = frame.action_at(x, y) {
                log::debug!("chrome button: {}", button.describe());
                self.press_chrome_button(button);
                if self.exit_requested {
                    return;
                }
                self.request_redraw();
                return;
            }
            // Only the hovered tab shows a close button, so only it can be clicked.
            if let Some(index) = frame.tab_close_at(x, y) {
                if self.hovered_tab == Some(index) {
                    if let Some(panes) = self.layout.close_tab(index) {
                        for pane in panes {
                            self.drop_session(pane);
                        }
                    }
                    if self.layout.is_empty() {
                        self.exit_requested = true;
                        return;
                    }
                    self.hovered_tab = None;
                    self.relayout();
                    self.request_redraw();
                    return;
                }
            }

            // A press on a tab may become a drag; the reorder only happens once the
            // pointer has actually moved.
            if let Some(index) = frame.tab_at(x, y) {
                self.dragging_tab = Some(TabDrag {
                    origin: index,
                    current: index,
                    start_x: x,
                    active: false,
                });
            }

            // A click on the tab strip selects that tab and goes no further; letting
            // it fall through would also start a selection in the pane below.
            if let Some(index) = frame.tab_at(x, y) {
                if self.layout.select_tab(index) {
                    self.clear_activity_for_active_tab();
                    self.relayout();
                    self.request_redraw();
                }
                return;
            }
            if frame.is_chrome(x, y) {
                // Empty title bar: drag moves the window, double-click maximizes it —
                // what a system title bar does, and the reason this area is left
                // clear of buttons.
                if button == MouseButton::Left {
                    self.press_title_bar(x, y);
                }
                return;
            }

            // A divider grab takes precedence: it sits between panes, so a click
            // there is never meant for a terminal.
            if let Some(divider) = frame.divider_at(x, y, DIVIDER_GRAB) {
                self.dragging = Some(divider.path.clone());
                return;
            }
            if let Some(pane) = frame.pane_at(x, y) {
                self.layout.focus_pane(pane);
            }
        }

        let Some(pane) = frame.pane_at(x, y) else {
            return;
        };
        let cell = self.cell_size();
        let Some((col, row)) = frame.cell_at(pane, x, y, cell) else {
            return;
        };

        let mods = keys::modifiers_from(self.modifiers);
        let mode = self.sessions.get(&pane).map(|s| *s.term().lock().mode());
        let reporting = mode
            .map(MouseReporting::from_mode)
            .unwrap_or(MouseReporting {
                click: false,
                drag: false,
                motion: false,
                sgr: false,
            });

        // Shift always forces terminal-side selection: it is the escape hatch
        // every terminal implements for selecting text inside a mouse-aware
        // program like vim or htop.
        let program_wants_it = reporting.wants_mouse() && !mods.shift();

        if program_wants_it {
            if let Some(bytes) = tuz_core::encode_mouse(button, pressed, col, row, mods, reporting)
            {
                if let Some(session) = self.sessions.get(&pane) {
                    session.write(bytes);
                }
            }
            return;
        }

        if button == MouseButton::Left && pressed {
            self.start_selection(pane, col, row);
        }
        // Middle click pastes the primary selection, the X11 convention.
        if button == MouseButton::Middle && pressed {
            self.paste();
        }
        self.request_redraw();
    }

    fn start_selection(&mut self, pane: PaneId, col: u16, row: u16) {
        use alacritty_selection::{Selection, SelectionType};
        let Some(session) = self.sessions.get(&pane) else {
            return;
        };

        let mut term = session.term().lock();
        let point = viewport_point(&term, col, row);
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            point,
            alacritty_index::Side::Left,
        ));
        drop(term);
        self.selecting = Some(pane);
    }

    fn on_mouse_move(&mut self, x: f64, y: f64) {
        self.mouse = (x, y);

        // Hover state changes what is drawn, so it must request a redraw — but only
        // when it actually changes, or the window repaints continuously while the
        // pointer moves.
        if self.update_hover(x as i32, y as i32) {
            self.request_redraw();
        }

        if self.dragging_tab.is_some() {
            self.drag_tab(x as i32, y as i32);
            return;
        }

        if let Some(path) = self.dragging.clone() {
            self.drag_divider(&path, x, y);
            return;
        }

        let Some(pane) = self.selecting else {
            return;
        };
        let Some(frame) = self.frame.clone() else {
            return;
        };
        let cell = self.cell_size();
        let Some((col, row)) = frame.cell_at(pane, x as i32, y as i32, cell) else {
            return;
        };

        if let Some(session) = self.sessions.get(&pane) {
            let mut term = session.term().lock();
            let point = viewport_point(&term, col, row);
            if let Some(selection) = term.selection.as_mut() {
                selection.update(point, alacritty_index::Side::Right);
            }
        }
        self.request_redraw();
    }

    /// Recompute what the pointer is over, reporting whether anything changed.
    fn update_hover(&mut self, x: i32, y: i32) -> bool {
        if let Some(panel) = self.panel.as_mut() {
            // Chrome hover is meaningless while the panel covers it.
            let mut changed = panel.ui.set_pointer(x, y);
            changed |= self.hovered_button.take().is_some();
            changed |= self.hovered_tab.take().is_some();
            return changed;
        }

        let Some(frame) = self.frame.as_ref() else {
            return false;
        };

        let button = frame.action_at(x, y);
        let tab = frame.tab_at(x, y);
        let close = frame.tab_close_at(x, y).is_some();

        let changed =
            button != self.hovered_button || tab != self.hovered_tab || close != self.hovered_close;

        self.hovered_button = button;
        self.hovered_tab = tab;
        self.hovered_close = close;
        changed
    }

    /// Reorder tabs as a drag passes over them.
    ///
    /// Applied live rather than on release so the strip previews where the tab will
    /// land — a drag that only commits at the end gives no feedback about what it is
    /// about to do.
    fn drag_tab(&mut self, x: i32, y: i32) {
        let Some(mut drag) = self.dragging_tab else {
            return;
        };

        if !drag.active {
            if (x - drag.start_x).abs() < DRAG_THRESHOLD {
                return;
            }
            drag.active = true;
        }

        let Some(frame) = self.frame.as_ref() else {
            return;
        };
        // Clamped to the strip's vertical band so a drag that wanders into the panes
        // still reorders rather than stopping dead.
        let y = y.clamp(frame.tab_bar.y, frame.tab_bar.bottom() - 1);

        let target = match frame.tab_at(x, y) {
            Some(index) => index,
            // Past the last tab means the end; before the first means the start.
            None if x >= frame.tab_bar.right() => self.layout.tab_count().saturating_sub(1),
            None if x <= frame.tab_bar.x => 0,
            None => drag.current,
        };

        if target != drag.current && self.layout.move_tab(drag.current, target) {
            drag.current = target;
            self.dragging_tab = Some(drag);
            self.relayout();
            self.request_redraw();
            return;
        }
        self.dragging_tab = Some(drag);
    }

    /// Move a split divider to follow the pointer.
    fn drag_divider(&mut self, path: &[Branch], x: f64, y: f64) {
        let Some(frame) = self.frame.clone() else {
            return;
        };
        let Some(divider) = frame.dividers.iter().find(|d| d.path == path) else {
            return;
        };

        // The ratio is relative to the parent split, whose extent is not stored,
        // so it is recovered from the two panes the divider separates.
        let (w, h) = self.gpu.as_ref().map_or((1, 1), |g| g.size());
        let ratio = match divider.axis {
            tuz_layout::Axis::Horizontal => x / w.max(1) as f64,
            tuz_layout::Axis::Vertical => y / h.max(1) as f64,
        };

        if self.layout.set_split_ratio(path, ratio as f32).is_some() {
            self.relayout();
            self.request_redraw();
        }
    }

    fn on_scroll(&mut self, delta: MouseScrollDelta) {
        // The panel owns the wheel while it is open, or scrolling over a settings
        // list would scroll the terminal hidden behind it.
        if self.panel.is_some() {
            let cell_height = self.cell_size().height.max(1) as f64;
            let lines = match delta {
                MouseScrollDelta::LineDelta(_, y) => -(y as f64 * 3.0 * cell_height),
                MouseScrollDelta::PixelDelta(pos) => -pos.y,
            };
            if let Some(panel) = self.panel.as_mut() {
                if panel.ui.scroll_by(lines.round() as i32) {
                    self.request_redraw();
                }
            }
            return;
        }

        let Some(frame) = self.frame.clone() else {
            return;
        };
        let (x, y) = (self.mouse.0 as i32, self.mouse.1 as i32);
        let Some(pane) = frame.pane_at(x, y) else {
            return;
        };

        let cfg = self.settings.config();
        let multiplier = cfg.scrollback.scroll_multiplier as f64;
        let cell_height = self.cell_size().height.max(1) as f64;

        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => (y as f64 * multiplier).round() as i32,
            // Pixel deltas come from touchpads; convert through the cell height so
            // a physical swipe scrolls the same distance at any font size.
            MouseScrollDelta::PixelDelta(pos) => (pos.y / cell_height).round() as i32,
        };
        if lines == 0 {
            return;
        }

        let Some(session) = self.sessions.get(&pane) else {
            return;
        };
        let mode = *session.term().lock().mode();
        let reporting = MouseReporting::from_mode(mode);
        let mods = keys::modifiers_from(self.modifiers);

        // In the alternate screen a mouse-aware program owns scrolling; scrolling
        // our own scrollback there would be meaningless because there is none.
        if reporting.wants_mouse() && !mods.shift() {
            let button = if lines > 0 {
                MouseButton::WheelUp
            } else {
                MouseButton::WheelDown
            };
            let cell = self.cell_size();
            if let Some((col, row)) = frame.cell_at(pane, x, y, cell) {
                for _ in 0..lines.abs() {
                    if let Some(bytes) =
                        tuz_core::encode_mouse(button, true, col, row, mods, reporting)
                    {
                        session.write(bytes);
                    }
                }
            }
            return;
        }

        session.scroll(lines);
        self.request_redraw();
    }

    // --- PTY events -------------------------------------------------------

    /// Drain everything the PTY threads reported.
    fn drain_pty_events(&mut self) {
        let mut redraw = false;
        let mut exited: Vec<PaneId> = Vec::new();
        let mut bells: Vec<PaneId> = Vec::new();
        let mut titles: Vec<(PaneId, String)> = Vec::new();

        while let Ok(PaneEvent { pane, event }) = self.events_rx.try_recv() {
            match event {
                TermEvent::Wakeup => {
                    redraw = true;
                    // Output in a pane the user is not looking at is what the tab
                    // activity dot reports.
                    if !self.layout.visible_panes().contains(&pane) {
                        self.activity.insert(pane);
                    }
                }

                TermEvent::Title(title) => {
                    if self.settings.config().window.dynamic_title
                        && pane == self.layout.active_pane()
                    {
                        if let Some(w) = &self.window {
                            w.set_title(&title);
                        }
                    }
                    self.titles.insert(pane, title.clone());
                    titles.push((pane, title));
                }
                TermEvent::ResetTitle => {
                    if let Some(w) = &self.window {
                        w.set_title(&self.settings.config().window.title);
                    }
                }

                TermEvent::PtyWrite(text) => {
                    if let Some(session) = self.sessions.get(&pane) {
                        session.write(text.into_bytes());
                    }
                }

                TermEvent::ClipboardStore(_, text) => {
                    if let Some(clipboard) = self.clipboard.as_mut() {
                        let _ = clipboard.set_text(text);
                    }
                }
                TermEvent::ClipboardLoad(_, format) => {
                    // The program asked for the clipboard; the formatter wraps it
                    // in whatever escape sequence it expects back.
                    let text = self
                        .clipboard
                        .as_mut()
                        .and_then(|c| c.get_text().ok())
                        .unwrap_or_default();
                    if let Some(session) = self.sessions.get(&pane) {
                        session.write(format(&text).into_bytes());
                    }
                }
                TermEvent::ColorRequest(index, format) => {
                    let theme = self.settings.theme();
                    let color = theme.indexed_color(index as u8);
                    let rgb = alacritty_color::Rgb {
                        r: color.r,
                        g: color.g,
                        b: color.b,
                    };
                    if let Some(session) = self.sessions.get(&pane) {
                        session.write(format(rgb).into_bytes());
                    }
                }
                TermEvent::TextAreaSizeRequest(format) => {
                    if let Some(session) = self.sessions.get(&pane) {
                        let size = session.size();
                        let ws = alacritty_event::WindowSize {
                            num_lines: size.screen_lines as u16,
                            num_cols: size.columns as u16,
                            cell_width: size.cell_width,
                            cell_height: size.cell_height,
                        };
                        session.write(format(ws).into_bytes());
                    }
                }

                TermEvent::Bell => {
                    log::trace!("{pane}: bell");
                    bells.push(pane);
                }
                TermEvent::CursorBlinkingChange => redraw = true,
                TermEvent::MouseCursorDirty => {}

                TermEvent::ChildExit(code) => {
                    log::debug!("{pane}: shell exited with {code:?}");
                    exited.push(pane);
                }
                TermEvent::Exit => exited.push(pane),
            }
        }

        // Closing panes mutates the layout, so it happens after draining rather
        // than inside the loop.
        for pane in exited {
            if let Some(session) = self.sessions.get_mut(&pane) {
                session.mark_child_exited();
            }
            self.titles.remove(&pane);
            self.activity.remove(&pane);
            if !self.close_pane(pane) {
                self.exit_requested = true;
            }
            redraw = true;
        }

        // Plugin notifications happen after draining, because a plugin command can
        // mutate the layout and doing that mid-drain would invalidate the loop.
        for pane in bells {
            self.notify_plugins(PluginEvent::Bell {
                pane: tuz_plugin_api::PaneId(pane.0),
            });
        }
        for (pane, title) in titles {
            self.notify_plugins(PluginEvent::TitleChange {
                pane: tuz_plugin_api::PaneId(pane.0),
                title,
            });
        }

        if redraw {
            self.request_redraw();
        }
    }

    /// Advance the cursor blink, returning when the next flip is due.
    fn update_blink(&mut self) -> Option<Instant> {
        let cfg = &self.settings.config().cursor;
        if !cfg.blink {
            self.blink_on = true;
            return None;
        }

        // Stop blinking after a period of inactivity so an idle terminal is not
        // waking the CPU twice a second forever.
        if cfg.blink_timeout_secs > 0
            && self.last_input.elapsed() > Duration::from_secs(cfg.blink_timeout_secs)
        {
            if !self.blink_on {
                self.blink_on = true;
                self.request_redraw();
            }
            return None;
        }

        let interval = Duration::from_millis(cfg.blink_interval_ms.max(50));
        if self.blink_at.elapsed() >= interval {
            self.blink_on = !self.blink_on;
            self.blink_at = Instant::now();
            self.request_redraw();
        }
        Some(self.blink_at + interval)
    }
}

/// Translate a chord into a panel key, if the panel understands it.
///
/// Only these reach the UI; everything else is swallowed while the panel is open so
/// stray typing cannot leak into the shell behind it.
fn panel_key(chord: &tuz_input::KeyChord) -> Option<UiKey> {
    use tuz_input::{Key, NamedKey};

    Some(match chord.key {
        Key::Named(NamedKey::Escape) => UiKey::Escape,
        Key::Named(NamedKey::Tab) if chord.mods.shift() => UiKey::ShiftTab,
        Key::Named(NamedKey::Tab) => UiKey::Tab,
        Key::Named(NamedKey::Up) => UiKey::Up,
        Key::Named(NamedKey::Down) => UiKey::Down,
        Key::Named(NamedKey::Left) => UiKey::Left,
        Key::Named(NamedKey::Right) => UiKey::Right,
        Key::Named(NamedKey::Enter) => UiKey::Activate,
        // Space activates a button or toggle, but must be typable in a text field;
        // `type_char` runs first and consumes it there.
        Key::Named(NamedKey::Space) => UiKey::Activate,
        Key::Named(NamedKey::Backspace) => UiKey::Backspace,
        Key::Named(NamedKey::Delete) => UiKey::Delete,
        Key::Named(NamedKey::Home) => UiKey::Home,
        Key::Named(NamedKey::End) => UiKey::End,
        _ => return None,
    })
}

/// The theme's error colour for a toast accent.
fn theme_error_color(theme: &tuz_config::Theme) -> tuz_config::Rgba {
    theme.normal.red
}

/// Translate a plugin direction into the layout's own.
fn to_direction(direction: tuz_plugin_api::Direction) -> Direction {
    match direction {
        tuz_plugin_api::Direction::Left => Direction::Left,
        tuz_plugin_api::Direction::Right => Direction::Right,
        tuz_plugin_api::Direction::Up => Direction::Up,
        tuz_plugin_api::Direction::Down => Direction::Down,
    }
}

/// Map a viewport cell to a grid point, accounting for scrollback offset.
fn viewport_point(
    term: &alacritty_term::Term<tuz_core::EventProxy>,
    col: u16,
    row: u16,
) -> alacritty_index::Point {
    let offset = term.grid().display_offset();
    alacritty_index::Point::new(
        alacritty_index::Line(row as i32 - offset as i32),
        alacritty_index::Column(col as usize),
    )
}

// Short aliases for the `alacritty_terminal` paths used above. Importing them
// through `tuz_core` would mean re-exporting a large surface for three call sites.
use alacritty_terminal::event as alacritty_event;
use alacritty_terminal::index as alacritty_index;
use alacritty_terminal::selection as alacritty_selection;
use alacritty_terminal::term as alacritty_term;
use alacritty_terminal::vte::ansi as alacritty_color;

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let cfg = self.settings.config();
        let width =
            cfg.window.columns as u32 * BOOTSTRAP_CELL.width + cfg.window.padding.x as u32 * 2;
        let height =
            cfg.window.rows as u32 * BOOTSTRAP_CELL.height + cfg.window.padding.y as u32 * 2;

        let attrs = Window::default_attributes()
            .with_title(&cfg.window.title)
            .with_decorations(cfg.window.decorations)
            // Rounded corners need an alpha channel just as much as opacity does:
            // the corner pixels are transparent, and on an opaque surface they would
            // come out black instead.
            .with_transparent(cfg.window.opacity < 1.0 || cfg.window.corner_radius > 0.0)
            .with_inner_size(winit::dpi::LogicalSize::new(width, height));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create a window: {e}");
                event_loop.exit();
                return;
            }
        };

        let gpu = match Gpu::new(window.clone(), self.settings.config()) {
            Ok(gpu) => gpu,
            Err(e) => {
                log::error!("GPU initialization failed: {e:#}");
                event_loop.exit();
                return;
            }
        };

        let fonts = match FontSystem::new(&self.settings.config().font, window.scale_factor()) {
            Ok(f) => f,
            Err(e) => {
                log::error!("font initialization failed: {e}");
                event_loop.exit();
                return;
            }
        };

        let renderer = Renderer::new(gpu.device(), gpu.surface_format(), fonts.atlas());

        log::info!(
            "tuzminal ready on {} ({:?}), cell {}x{}",
            gpu.adapter_info().name,
            gpu.adapter_info().backend,
            fonts.metrics().width,
            fonts.metrics().height
        );

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.fonts = Some(fonts);
        self.renderer = Some(renderer);

        // Layout first so the shell starts at the correct size and never has to
        // redraw for a resize it could have known about.
        self.relayout();
        let first = self.layout.active_pane();
        self.ensure_session(first);
        self.relayout();
        self.request_redraw();
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::ConfigChanged => self.reload_config(),
            UserEvent::Wakeup => self.drain_pty_events(),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size.width, size.height);
                }
                self.relayout();
                // Painted here and now rather than queued. `request_redraw` defers to
                // the next loop iteration, which leaves the content one frame behind
                // the border the compositor is already drawing at the new size — the
                // lag you see as the window smearing while you drag its edge. Drawing
                // synchronously keeps the two in step, and vsync throttles this
                // naturally, so a burst of resize events cannot outrun the display.
                self.redraw();
            }

            WindowEvent::ScaleFactorChanged { .. } => {
                // A DPI change means new pixel metrics for the same point size.
                self.rebuild_fonts();
                self.relayout();
                self.request_redraw();
            }

            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),

            WindowEvent::KeyboardInput { event, .. } => {
                self.on_key(&event);
                if self.exit_requested {
                    event_loop.exit();
                }
            }

            WindowEvent::CursorMoved { position, .. } => self.on_mouse_move(position.x, position.y),
            WindowEvent::MouseInput { button, state, .. } => {
                self.on_mouse_button(button, state);
                if self.exit_requested {
                    event_loop.exit();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => self.on_scroll(delta),

            WindowEvent::Focused(focused) => {
                if focused {
                    self.last_input = Instant::now();
                }
                self.request_redraw();
            }

            WindowEvent::RedrawRequested => self.redraw(),

            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Events can arrive without a proxy wakeup if several land at once.
        self.drain_pty_events();

        if self.exit_requested || self.layout.is_empty() {
            event_loop.exit();
            return;
        }

        // Whichever wants a frame sooner decides when to wake.
        let blink = self.update_blink();
        let toast = self.update_toasts();
        match [blink, toast].into_iter().flatten().min() {
            Some(next) => event_loop.set_control_flow(ControlFlow::WaitUntil(next)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Reap every shell so no child is left orphaned holding a PTY open.
        let panes: Vec<PaneId> = self.sessions.keys().copied().collect();
        for pane in panes {
            self.drop_session(pane);
        }
        let _ = &self.proxy;
    }
}

/// Build the keymap from config plus whatever plugins registered.
///
/// Plugin bindings are applied first so a user's config can override them; the
/// user's own file should always win over a plugin's suggestion.
fn build_keymap(settings: &ConfigManager, plugins: &PluginHost) -> Keymap {
    let mut keys = std::collections::BTreeMap::new();
    for (chord, command) in plugins.keybinds() {
        keys.insert(chord.clone(), command.clone());
    }
    keys.extend(settings.config().effective_keys());

    let plugin_actions: std::collections::HashSet<String> =
        plugins.command_names().into_iter().collect();

    let built = Keymap::from_config(
        keys.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        &plugin_actions,
    );
    for err in &built.errors {
        log::warn!("keybinding: {err}");
    }
    log::debug!("{} keybindings active", built.keymap.len());
    built.keymap
}
