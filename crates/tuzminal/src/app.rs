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
    TabKind,
};
use tuz_plugin::Host as PluginHost;
use tuz_plugin_api::{Command as PluginCommand, Event as PluginEvent, KeyOutcome};
use tuz_render::{build_pane, ColorSpace, Instance, PaneGeometry, Renderer};
use tuz_ui::{UiKey, Widget};

use crate::settings::{PanelOutcome, SettingsPanel};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{CursorIcon, ResizeDirection, Window, WindowId};

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

/// Width of the invisible band along each window edge that resizes instead of
/// selecting. Only exists without decorations, where the compositor draws no frame
/// of its own and a borderless window would otherwise be stuck at one size.
const RESIZE_BORDER: i32 = 6;

/// Wayland app id and X11 `WM_CLASS`.
///
/// Must match the basename of the installed `.desktop` file, or the desktop
/// environment cannot pair the window with its name and icon.
///
/// Gated to the platforms that have such a thing, alongside `crate::desktop`: nothing
/// on macOS or Windows reads it, and an unused constant is an error under `-D warnings`.
#[cfg(all(unix, not(target_os = "macos")))]
pub const APP_ID: &str = "tuzminal";

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
    /// A folder was chosen in the system file dialog, or the dialog was cancelled.
    ///
    /// Arrives as an event rather than a return value because the dialog runs on its
    /// own thread: under Wayland it is a D-Bus round trip to the desktop portal,
    /// which can take seconds, and waiting for it on the event loop would freeze the
    /// window until the user picked something.
    FolderPicked {
        purpose: FolderPurpose,
        path: Option<std::path::PathBuf>,
    },
}

/// Which field a picked folder belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderPurpose {
    ImportPlugin,
    ExportPlugins,
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

    /// The newest size the compositor has asked for, not yet acted on.
    ///
    /// Dragging a window edge produces a stream of `Resized` events — far more of them
    /// per second than the display can show frames. Only the last one in a batch
    /// describes the size the window actually has, so the rest are recorded here and
    /// thrown away, and the work they would each have cost happens once per frame
    /// instead. See the `Resized` arm.
    pending_size: Option<PhysicalSize<u32>>,

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
    /// The toolbar button currently held down, for the pressed state.
    pressed_button: Option<ChromeButton>,
    /// The status-bar editor button under the pointer, and the one held down.
    hovered_ide: Option<usize>,
    pressed_ide: Option<usize>,
    /// When and where the title bar was last pressed, for double-click detection.
    last_title_click: Option<(Instant, i32, i32)>,
    /// Resize cursor currently set, so it is only pushed to the compositor on change.
    resize_cursor: Option<CursorIcon>,
    /// The focused pane's working directory, for the status bar.
    cwd: crate::status::CwdCache,
    /// Clickable status segments and where they were drawn last frame.
    ///
    /// Recorded from the draw rather than recomputed, so what is clickable is exactly
    /// what is on screen. The `String` is the qualified `plugin.id`.
    ide_hits: Vec<(String, Rect)>,
    /// Shell for the next session, when one was chosen from the menu.
    ///
    /// A one-shot rather than a field on the pane: it is only ever read by the
    /// `ensure_session` that immediately follows setting it, and storing it per pane
    /// would imply a pane could be respawned with it, which nothing does.
    pending_shell: Option<String>,
    /// Directory the next spawned pane should start in, for one spawn only.
    ///
    /// Captured before the new pane takes focus, which is the whole reason it exists:
    /// by the time `ensure_session` runs, the active pane *is* the new one and it has no
    /// shell to ask. Same shape as `pending_shell` above — an exception to the config
    /// for one spawn, rather than a second parameter on `Session::spawn`.
    pending_cwd: Option<std::path::PathBuf>,
    /// The open dropdown, if any.
    menu: Option<crate::menu::Menu>,
    /// Where it was drawn last frame, for hit-testing what is on screen.
    menu_rect: Option<Rect>,
    /// The shortcut reference, when its tab is open.
    help: Option<crate::help::HelpPage>,
    /// The plugins page, when its tab is open.
    plugins_page: Option<crate::plugins::PluginsPage>,
    /// Its scrollable body from the last frame, for the wheel and `scroll_to_focus`.
    plugins_body: Option<Rect>,
    /// The file explorer, when open. `None` means closed.
    sidebar: Option<crate::explorer::Explorer>,
    /// Whether the sidebar has the keyboard.
    ///
    /// Separate from being open, and that separation is the whole design: unlike the
    /// settings page — which is a whole tab, with no terminal on screen to type into —
    /// the sidebar sits beside a live shell, and the common case is looking at it
    /// while typing. Being open must not take the keyboard.
    sidebar_focused: bool,
    /// Dragging the sidebar's right edge. Distinct from `dragging`, which is the
    /// split-divider drag between panes.
    /// Offset from the pointer to the sidebar's right edge while dragging it.
    ///
    /// Stored so the edge does not jump to the cursor on the first motion event.
    dragging_sidebar: Option<i32>,
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
            pressed_button: None,
            hovered_ide: None,
            pressed_ide: None,
            last_title_click: None,
            resize_cursor: None,
            cwd: crate::status::CwdCache::default(),
            pending_shell: None,
            pending_cwd: None,
            pending_size: None,
            menu: None,
            menu_rect: None,
            help: None,
            plugins_page: None,
            plugins_body: None,
            ide_hits: Vec::new(),
            sidebar: None,
            sidebar_focused: false,
            dragging_sidebar: None,
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
        // The settings page has no terminal, so every built-in segment — cursor
        // position, grid size, the shell's directory — would be reporting on a pane
        // that is not on screen. An empty strip taking a row is worse than no strip.
        if matches!(
            self.layout.active_kind(),
            TabKind::Settings | TabKind::Help | TabKind::Plugins
        ) {
            return 0;
        }
        // Config decides, with one exception: a plugin contributing segments still
        // forces the strip to appear, so turning the built-in content off does not
        // silently swallow a plugin the user installed.
        if !self.settings.config().status_bar.enabled && self.plugins.status_segments().is_empty() {
            return 0;
        }
        self.cell_size().height + CHROME_PADDING
    }

    /// Width the sidebar occupies, in pixels. Zero when it is closed.
    ///
    /// Config stores cells rather than pixels so the sidebar scales with the font
    /// instead of becoming a sliver at 20pt.
    fn sidebar_width(&self) -> u32 {
        if self.sidebar.is_none() {
            return 0;
        }
        // A file browser beside the settings or shortcuts page browses nothing you
        // could act on: its actions all reach into a shell, and there is no shell on
        // those tabs. It stays open and comes back when a terminal tab does.
        if self.layout.active_kind() != TabKind::Terminal {
            return 0;
        }
        let cfg = self.settings.config();
        let cells = cfg.explorer.width.clamp(
            tuz_config::EXPLORER_MIN_WIDTH,
            tuz_config::EXPLORER_MAX_WIDTH,
        );
        cells as u32 * self.cell_size().width
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
            sidebar_width: self.sidebar_width(),
            tab_width: TAB_WIDTH,
            min_tab_width: MIN_TAB_WIDTH,
            buttons: self.chrome_buttons(),
            cell: self.cell_size(),
        }
    }

    /// Buttons for the tab strip, in right-to-left order.
    ///
    /// Which window edge or corner the pointer is over, if any.
    ///
    /// Returns `None` with decorations on (the compositor's own frame handles it) and
    /// while maximized (there is nothing to drag a maximized window's edge to).
    fn resize_edge(&self, x: i32, y: i32) -> Option<ResizeDirection> {
        if self.settings.config().window.decorations {
            return None;
        }
        let Some(w) = &self.window else {
            return None;
        };
        if w.is_maximized() {
            return None;
        }
        let size = w.inner_size();
        resize_edge_at(x, y, size.width as i32, size.height as i32)
    }

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

    /// Width of the window outline, or zero where one should not be drawn.
    ///
    /// Suppressed in three cases. With decorations the compositor draws the frame and
    /// a second border inside it reads as a mistake. Maximized, the window edge is the
    /// screen edge, so the outline would only eat a row of pixels at each side. And it
    /// is clamped to a quarter of the smaller dimension, because a border wide enough
    /// to meet itself in the middle would paint the whole window in the outline color
    /// — which is what a negative inset would produce.
    fn border_width(cfg: &tuz_config::Config, maximized: bool, (width, height): (u32, u32)) -> f32 {
        if cfg.window.decorations || maximized {
            return 0.0;
        }
        let limit = width.min(height) as f32 / 4.0;
        cfg.window.border_width.clamp(0.0, limit)
    }

    /// Toolbar buttons whose panel is on screen right now.
    ///
    /// A toggle that looks identical whether its panel is open or shut leaves the
    /// only way to find out being to press it and see.
    fn active_buttons(&self) -> Vec<ChromeButton> {
        let mut out = Vec::new();
        if self.sidebar.is_some() {
            out.push(ChromeButton::Explorer);
        }
        match self.layout.active_kind() {
            // All three live behind one button now, so it lights for any of them.
            TabKind::Settings | TabKind::Help | TabKind::Plugins => out.push(ChromeButton::AppMenu),
            TabKind::Terminal => {}
        }
        out
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
        // Settings, shortcuts and plugins share one button. Three separate icons for
        // three pages you open occasionally crowded out the ones you press often.
        buttons.push(ChromeButton::AppMenu);
        buttons.push(ChromeButton::Explorer);
        buttons.push(ChromeButton::SplitDown);
        buttons.push(ChromeButton::SplitRight);
        buttons.push(ChromeButton::NewTab);
        buttons.push(ChromeButton::NewTabMenu);
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
        // A settings tab has no process to take a name from, and numbering it would
        // say nothing about what it holds.
        match tab.kind() {
            TabKind::Settings => return "Settings".to_owned(),
            TabKind::Help => return "Shortcuts".to_owned(),
            TabKind::Plugins => return "Plugins".to_owned(),
            TabKind::Terminal => {}
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

        // A menu choice overrides the configured program for this one spawn. Cloning
        // the config is a spawn-time cost, and it keeps `Session::spawn` taking one
        // config rather than a config plus an exception to it.
        let mut config = self.settings.config().clone();
        if let Some(program) = &self.pending_shell {
            config.shell.program = Some(program.clone());
        }
        // `inherit_pane` can only be resolved here: `tuz-core` has no idea which pane
        // has focus, and by now the answer has already been captured into `pending_cwd`
        // by whoever created the pane. Left unresolved it falls back to the home
        // directory, which is what the very first pane gets.
        if config.shell.working_directory.as_deref() == Some("inherit_pane") {
            if let Some(cwd) = self.pending_cwd.as_ref() {
                config.shell.working_directory = Some(cwd.display().to_string());
            }
        }

        match Session::spawn(
            pane,
            &config,
            size,
            self.events_tx.clone(),
            self.waker.clone(),
        ) {
            Ok(session) => {
                self.sessions.insert(pane, session);
                self.notify_plugins(PluginEvent::PaneOpened {
                    pane: tuz_plugin_api::PaneId(pane.0),
                });
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

    /// Act on the most recent size the compositor asked for, if it has moved.
    ///
    /// The expensive half of handling a resize, deferred out of the event handler so a
    /// burst of resize events costs one of these rather than one each. Called from
    /// `redraw` rather than only from the `RedrawRequested` arm, because several paths
    /// paint without going through that arm and none of them should paint at a size the
    /// window has stopped being.
    fn apply_pending_resize(&mut self) {
        let Some(size) = self.pending_size else {
            return;
        };
        // Held rather than dropped when there is no swapchain to configure yet: a
        // resize can land between the window being created and the gpu being ready,
        // and the compositor will not repeat itself.
        if self.gpu.is_none() {
            return;
        }
        self.pending_size = None;

        let gpu = self.gpu.as_mut().expect("checked above");
        if gpu.size() == (size.width, size.height) {
            return;
        }
        gpu.resize(size.width, size.height);
        self.relayout();
    }

    fn redraw(&mut self) {
        self.apply_pending_resize();

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
        // How far a panel's title is indented. It has to be `Metrics::padding` and not
        // the cell height: the two were the same number until the panel metrics were
        // snapped to a grid, and a title indented differently from the rows it heads
        // reads as a misalignment. `draw_panel_title` says as much in its doc comment,
        // and a test in `tuz-render` is what caught this when the snap made them differ.
        let panel_inset = tuz_ui::Metrics::from_cell(cell.width, cell.height).padding as f32;
        let active = self.layout.active_pane();
        let blink_on = self.blink_on;
        let active_tab = self.layout.active_index();

        // The built-in status segments, gathered here for the same reason as the tab
        // titles: `self.cwd.get` needs `&mut self`, which the field destructure below
        // rules out.
        let status_left: Vec<tuz_plugin_api::StatusSegment> = {
            let cfg = self.settings.config();
            if cfg.status_bar.enabled {
                let session = self.sessions.get(&active);
                let pane_status = session.map(|s| s.status());
                let pid = session.and_then(|s| s.child_pid());
                let home = crate::proc::home();
                let title = self.titles.get(&active).cloned();
                let panes = self.layout.active_tab().pane_count();
                let tabs = self.layout.tab_count();
                let theme_name = self.settings.theme().name.clone();
                let font_size = cfg.font.size;
                let show = cfg.status_bar.clone();
                let width = self.gpu.as_ref().map_or(0, |g| g.size().0) as f32;
                let directory = self
                    .cwd
                    .get(active, pid, Instant::now())
                    .map(std::path::Path::to_owned);

                crate::status::build(&crate::status::StatusInput {
                    directory: directory.as_deref(),
                    home: home.as_deref(),
                    title: title.as_deref(),
                    cursor: pane_status.map(|s| (s.column as u16, s.line.max(0) as u16)),
                    grid: pane_status.map_or((0, 0), |s| (s.columns, s.rows)),
                    display_offset: pane_status.map_or(0, |s| s.display_offset),
                    panes,
                    tabs,
                    theme: &theme_name,
                    font_size,
                    cell_width: cell.width as f32,
                    width,
                    show: &show,
                })
            } else {
                Vec::new()
            }
        };

        // Chrome text is gathered here, before the `&mut` field borrows below, since
        // building a label needs `&self`.
        let tab_titles: Vec<String> = (0..self.layout.tab_count())
            .map(|i| self.tab_title(i))
            .collect();

        // A maximized window is flush with the screen edges, and rounding there just
        // punches holes showing the desktop through the corners.
        let maximized = self
            .window
            .as_ref()
            .map(|w| w.is_maximized())
            .unwrap_or(false);
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
        // Resolved before the field destructure below, which takes `self` apart and
        // leaves the keymap out of reach.
        let shortcut = hovered_button.and_then(|b| self.button_shortcut(b));
        let hovered_tab = self.hovered_tab;
        let hovered_close = self.hovered_close;
        // Widgets are built here, before the `&mut` field borrows, because building a
        // row needs to read the config.
        let panel_widgets: Option<(Vec<Widget>, Vec<Widget>)> = self.panel.as_ref().map(|panel| {
            (
                panel.widgets(self.settings.config()),
                panel.footer_widgets(),
            )
        });

        let pressed_button = self.pressed_button;
        let hovered_ide = self.hovered_ide;
        let pressed_ide = self.pressed_ide;
        // Segment plus the qualified id of the plugin that owns it, when it is one
        // that can be pressed.
        let status_owned = self.plugins.status_segments_with_owner();
        let active_buttons = self.active_buttons();

        // Same reason as the settings widgets: building rows needs `&self`, and the
        // field destructure below rules that out.
        let explorer_view: Option<(Vec<Widget>, Vec<Widget>, String, bool)> =
            self.sidebar.as_ref().map(|e| {
                (
                    e.widgets(),
                    e.footer_widgets(),
                    crate::proc::display_path(e.dir(), crate::proc::home().as_deref()),
                    self.sidebar_focused,
                )
            });

        let plugins_widgets: Option<(Vec<Widget>, Vec<Widget>)> = self
            .plugins_page
            .as_ref()
            .map(|page| (page.widgets(self.settings.config()), page.footer_widgets()));
        let plugins_page_rect: Option<Rect> = (self.layout.active_kind() == TabKind::Plugins)
            .then(|| frame.panes.first().map(|p| p.rect))
            .flatten();

        let help_widgets: Option<Vec<Widget>> = self
            .help
            .as_ref()
            .map(|page| page.widgets(self.settings.config()));
        let help_page: Option<Rect> = (self.layout.active_kind() == TabKind::Help)
            .then(|| frame.panes.first().map(|p| p.rect))
            .flatten();

        // The settings page fills its tab's pane. Resolved here, before the field
        // borrows below, and `None` whenever another tab is showing — which is what
        // keeps the page from drawing over a terminal.
        let settings_page: Option<Rect> = (self.layout.active_kind() == TabKind::Settings)
            .then(|| frame.panes.first().map(|p| p.rect))
            .flatten();
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
        let radius = if maximized {
            0.0
        } else {
            Self::corner_radius(cfg)
        };
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

            tuz_render::chrome::draw_chrome_buttons(
                instances,
                frame.tab_bar,
                &frame.actions,
                hovered_button,
                pressed_button,
                &active_buttons,
                theme,
                colors,
                radius,
            );

            // After the strip, so it overlaps the tabs below rather than being
            // painted over by them.
            if let Some(button) = hovered_button {
                if let Some((_, anchor)) = frame.actions.iter().find(|(b, _)| *b == button) {
                    let window = Rect::from_size(gpu.size().0, gpu.size().1);
                    tuz_render::draw_tooltip(
                        instances,
                        fonts,
                        button,
                        shortcut.as_deref(),
                        *anchor,
                        window,
                        theme,
                        colors,
                    );
                }
            }
        }

        let mut ide_hits: Vec<(String, Rect)> = Vec::new();
        if frame.status_bar.height > 0 {
            // Clickable segments are drawn like buttons; the rest are text. The
            // built-in editor buttons used to live here; they ship as a plugin now,
            // which is why this no longer special-cases anything of its own.
            let clickable: Vec<bool> = status_owned.iter().map(|(_, o)| o.is_some()).collect();
            let seg_colors: Vec<(Option<String>, Option<String>)> = clickable
                .iter()
                .enumerate()
                .map(|(i, is_button)| {
                    if !is_button {
                        return (None, None);
                    }
                    if pressed_ide == Some(i) {
                        (
                            Some(theme.background.to_hex()),
                            Some(theme.cursor().to_hex()),
                        )
                    } else if hovered_ide == Some(i) {
                        (
                            Some(theme.foreground.to_hex()),
                            Some(theme.background_focused().to_hex()),
                        )
                    } else {
                        (None, None)
                    }
                })
                .collect();

            let right: Vec<tuz_render::StatusItem<'_>> = status_owned
                .iter()
                .zip(&seg_colors)
                .map(|((segment, _), (fg, bg))| tuz_render::StatusItem {
                    text: &segment.text,
                    foreground: fg.as_deref().or(segment.foreground.as_deref()),
                    background: bg.as_deref().or(segment.background.as_deref()),
                })
                .collect();

            let left: Vec<tuz_render::StatusItem<'_>> = status_left
                .iter()
                .map(|segment| tuz_render::StatusItem {
                    text: &segment.text,
                    foreground: None,
                    background: None,
                })
                .collect();
            let rects = tuz_render::draw_status_bar(
                instances,
                fonts,
                frame.status_bar,
                &left,
                &right,
                theme,
                colors,
                radius,
            );
            // Every clickable segment, paired with where it was drawn. Segments with
            // no id are skipped: a clock should not swallow a press.
            ide_hits = status_owned
                .iter()
                .zip(rects)
                .filter_map(|((_, owner), rect)| owner.clone().map(|id| (id, rect)))
                .collect();
        }
        // The sidebar sits in its own column, carved out of the pane body, so it
        // overlaps nothing and needs no scrim.

        if let (Some((rows, footer, title, focused)), Some(explorer)) =
            (explorer_view, self.sidebar.as_mut())
        {
            let rect = frame.sidebar;
            if rect.width > 0 {
                tuz_render::draw_page_frame(instances, rect, theme, colors, 0.0);
                let body = tuz_render::draw_panel_title(
                    instances,
                    fonts,
                    rect,
                    &title,
                    theme,
                    colors,
                    panel_inset,
                );
                explorer.ui.layout_split_with(
                    &rows,
                    &footer,
                    body,
                    tuz_ui::Metrics::from_cell(cell.width, cell.height),
                );

                // The prompt owns the keyboard while it is open, and the caret is
                // only drawn for the focused widget.
                explorer.focus_prompt();

                let footer_area = explorer.ui.footer_area();
                let scroll_area = Rect::new(
                    body.x,
                    body.y,
                    body.width,
                    body.height.saturating_sub(footer_area.height),
                );

                if footer_area.height > 0 {
                    tuz_render::draw_footer_divider(instances, footer_area, theme, colors, 0.0);
                }

                tuz_render::draw_widgets_in(
                    instances,
                    fonts,
                    explorer.ui.body(),
                    &explorer.ui,
                    theme,
                    colors,
                );

                tuz_render::draw_widgets_in(
                    instances,
                    fonts,
                    &explorer.ui.placed()[explorer.ui.body().len()..],
                    &explorer.ui,
                    theme,
                    colors,
                );
                tuz_render::draw_scrollbar(instances, &explorer.ui, scroll_area, theme, colors);

                // A line down the right edge, brighter while focused. Without it the
                // sidebar having the keyboard is guesswork.
                instances.push(tuz_render::Instance::solid(
                    (rect.right() - 1) as f32,
                    rect.y as f32,
                    1.0,
                    rect.height as f32,
                    colors.convert_opaque(if focused {
                        theme.cursor()
                    } else {
                        theme.split_divider()
                    }),
                ));
            }
        }

        // The plugins page: the same shape as settings — scrolling rows, a pinned
        // footer — so it reuses that path rather than growing a third one.
        let mut plugins_body: Option<Rect> = None;
        let mut plugins_start = 0u32;
        let mut plugins_end = 0u32;
        if let (Some((widgets, footer)), Some(rect), Some(page)) = (
            plugins_widgets,
            plugins_page_rect,
            self.plugins_page.as_mut(),
        ) {
            tuz_render::draw_page_frame(instances, rect, theme, colors, radius);
            let body = tuz_render::draw_panel_title(
                instances,
                fonts,
                rect,
                "Plugins",
                theme,
                colors,
                panel_inset,
            );
            page.ui.layout_split_with(
                &widgets,
                &footer,
                body,
                tuz_ui::Metrics::from_cell(cell.width, cell.height),
            );

            let footer_area = page.ui.footer_area();
            let scroll_area = Rect::new(
                body.x,
                body.y,
                body.width,
                body.height.saturating_sub(footer_area.height),
            );
            plugins_body = Some(scroll_area);
            tuz_render::draw_footer_divider(instances, footer_area, theme, colors, radius);

            plugins_start = instances.len() as u32;
            tuz_render::draw_widgets_in(instances, fonts, page.ui.body(), &page.ui, theme, colors);
            plugins_end = instances.len() as u32;

            tuz_render::draw_widgets_in(
                instances,
                fonts,
                &page.ui.placed()[page.ui.body().len()..],
                &page.ui,
                theme,
                colors,
            );
            tuz_render::draw_scrollbar(instances, &page.ui, scroll_area, theme, colors);
        }

        // The reference page: rows and a title, no footer and nothing to edit.
        let mut help_body: Option<Rect> = None;
        let mut help_start = 0u32;
        let mut help_end = 0u32;
        if let (Some(widgets), Some(rect), Some(page)) =
            (help_widgets, help_page, self.help.as_mut())
        {
            tuz_render::draw_page_frame(instances, rect, theme, colors, radius);
            let body = tuz_render::draw_panel_title(
                instances,
                fonts,
                rect,
                "Shortcuts",
                theme,
                colors,
                panel_inset,
            );
            page.ui.layout_split_with(
                &widgets,
                &[],
                body,
                tuz_ui::Metrics::from_cell(cell.width, cell.height),
            );
            help_body = Some(body);

            help_start = instances.len() as u32;
            tuz_render::draw_widgets_in(instances, fonts, page.ui.body(), &page.ui, theme, colors);
            help_end = instances.len() as u32;

            tuz_render::draw_scrollbar(instances, &page.ui, body, theme, colors);
        }

        // The panel goes last so it sits over terminal content and chrome alike.
        let mut panel_body: Option<Rect> = None;
        let mut widget_start = 0u32;
        let mut widget_end = 0u32;
        if let (Some((widgets, footer)), Some(rect), Some(panel)) =
            (panel_widgets, settings_page, self.panel.as_mut())
        {
            // The page only owns the window's bottom corners when nothing is drawn
            // below it; a status bar takes them over instead.
            let page_radius = if frame.status_bar.height > 0 {
                0.0
            } else {
                radius
            };
            tuz_render::draw_page_frame(instances, rect, theme, colors, page_radius);
            let body = tuz_render::draw_panel_title(
                instances,
                fonts,
                rect,
                "Tuzminal Settings",
                theme,
                colors,
                panel_inset,
            );
            panel.ui.layout_split_with(
                &widgets,
                &footer,
                body,
                tuz_ui::Metrics::from_cell(cell.width, cell.height),
            );

            // The scrolling region is the body minus the pinned footer, and that is
            // what has to be clipped and scrolled against — not the whole page.
            let footer_area = panel.ui.footer_area();
            let scroll_area = Rect::new(
                body.x,
                body.y,
                body.width,
                body.height.saturating_sub(footer_area.height),
            );
            panel_body = Some(scroll_area);

            tuz_render::draw_footer_divider(instances, footer_area, theme, colors, page_radius);

            // Rows are clipped to the scrolling region, so a scrolled list cannot draw
            // over the title bar or down into the footer.
            widget_start = instances.len() as u32;
            tuz_render::draw_widgets_in(
                instances,
                fonts,
                panel.ui.body(),
                &panel.ui,
                theme,
                colors,
            );
            widget_end = instances.len() as u32;

            // Footer buttons are drawn after the clipped range so they are never cut
            // off by it, and the scrollbar tracks the body alone.
            tuz_render::draw_widgets_in(
                instances,
                fonts,
                &panel.ui.placed()[panel.ui.body().len()..],
                &panel.ui,
                theme,
                colors,
            );
            tuz_render::draw_scrollbar(instances, &panel.ui, scroll_area, theme, colors);
        }
        // Above every panel: a dropdown is the thing being interacted with, and one
        // drawn under the page it hangs over would be unusable.
        let mut menu_rect: Option<Rect> = None;
        if let Some(menu) = self.menu.as_ref() {
            let window = Rect::from_size(gpu.size().0, gpu.size().1);
            let rect = menu.rect(window, (cell.width, cell.height));
            let rows: Vec<(Rect, &str)> = menu
                .items
                .iter()
                .enumerate()
                .map(|(i, item)| (menu.row_rect(rect, i, cell.height), item.label.as_str()))
                .collect();
            tuz_render::draw_menu(instances, fonts, rect, &rows, menu.selected, theme, colors);
            menu_rect = Some(rect);
        }

        if !toasts.is_empty() {
            let window = Rect::from_size(gpu.size().0, gpu.size().1);
            tuz_render::draw_toasts(instances, fonts, &toasts, window, theme, colors);
        }
        // Last of all, over every pane and every piece of chrome. The tab strip and the
        // status bar both run the full width of the window, so a border drawn with the
        // background would be buried under them along the top and bottom edges and
        // survive only down the sides.
        let border = Self::border_width(cfg, maximized, gpu.size());
        if border > 0.0 {
            let (w, h) = gpu.size();
            instances.push(Instance::ring(
                0.0,
                0.0,
                w as f32,
                h as f32,
                colors.convert(theme.window_border()),
                radius,
                tuz_render::instance::FLAG_ROUND_TOP | tuz_render::instance::FLAG_ROUND_BOTTOM,
                border,
            ));
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
                if plugins_end > plugins_start {
                    if let Some(body) = plugins_body {
                        renderer.draw(pass, divider_start..plugins_start);
                        pass.set_scissor_rect(
                            body.x.max(0) as u32,
                            body.y.max(0) as u32,
                            body.width.min(width),
                            body.height.min(height),
                        );
                        renderer.draw(pass, plugins_start..plugins_end);
                        pass.set_scissor_rect(0, 0, width, height);
                        renderer.draw(pass, plugins_end..chrome_end);
                        return;
                    }
                }
                if help_end > help_start {
                    if let Some(body) = help_body {
                        renderer.draw(pass, divider_start..help_start);
                        pass.set_scissor_rect(
                            body.x.max(0) as u32,
                            body.y.max(0) as u32,
                            body.width.min(width),
                            body.height.min(height),
                        );
                        renderer.draw(pass, help_start..help_end);
                        pass.set_scissor_rect(0, 0, width, height);
                        renderer.draw(pass, help_end..chrome_end);
                        return;
                    }
                }
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
        self.ide_hits = ide_hits;
        self.menu_rect = menu_rect;
        self.plugins_body = plugins_body;

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

                // Notified here rather than at the keybinding, which was the only
                // path that told plugins. Editing `config.toml` in an editor is how
                // most reloads happen, and `on_config_reload` never ran for any of
                // them.
                self.notify_plugins(PluginEvent::ConfigReload);

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
                if self.settings_active() {
                    self.close_settings();
                } else {
                    self.open_settings();
                }
            }
            OpenExplorer => self.toggle_explorer(),
            OpenHelp => self.toggle_help(),
            OpenPlugins => self.toggle_plugins(),

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
                self.on_tab_activated();
                self.relayout();
                self.request_redraw();
            }
            PrevTab => {
                self.layout.prev_tab();
                self.on_tab_activated();
                self.relayout();
                self.request_redraw();
            }
            SelectTab(n) => {
                if self.layout.select_tab((n as usize).saturating_sub(1)) {
                    self.on_tab_activated();
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
    /// Everything that must happen when a different tab becomes the visible one.
    ///
    /// One function rather than a call to each half at seven separate sites: the
    /// plugin notification was missing entirely, and adding it beside every existing
    /// `clear_activity` call would have left the settings, help and explorer paths
    /// out, since those switch tabs without clearing activity.
    fn on_tab_activated(&mut self) {
        for pane in self.layout.visible_panes() {
            self.activity.remove(&pane);
        }
        let index = self.layout.active_index() as u32;
        self.notify_plugins(PluginEvent::TabSwitch { index });
    }

    /// Set the sidebar width from a dragged right edge, in pixels.
    ///
    /// Stored in cells rather than pixels, so the sidebar keeps its proportions when
    /// the font size changes instead of becoming a sliver or swallowing the window.
    fn drag_sidebar(&mut self, edge_x: i32) {
        let Some(frame) = self.frame.as_ref() else {
            return;
        };
        let cell = self.cell_size().width.max(1) as i32;
        let cells = ((edge_x - frame.sidebar.x) / cell).clamp(
            tuz_config::EXPLORER_MIN_WIDTH as i32,
            tuz_config::EXPLORER_MAX_WIDTH as i32,
        ) as u16;

        if cells == self.settings.config().explorer.width {
            return;
        }
        // Through `modify` for the session only, not `save`: writing config.toml on
        // every drag frame would rewrite the file dozens of times a second and trip
        // its own watcher. The settings page is where it becomes permanent.
        let actions = self.settings.modify(|c| c.explorer.width = cells);
        self.apply_reload_actions(&actions);
        self.request_redraw();
    }

    /// A row was clicked: select it, and activate it if it was already selected.
    ///
    /// One click selects, a second activates. A single click that descended would make
    /// the list impossible to browse with the mouse — every click would move you.
    fn explorer_click(&mut self, id: tuz_ui::WidgetId) {
        let Some(explorer) = self.sidebar.as_mut() else {
            return;
        };
        let outcome = explorer.click_row(id);
        self.handle_explorer_outcome(outcome);
    }

    /// Route one keystroke to the focused sidebar.
    fn explorer_key(&mut self, chord: &tuz_input::KeyChord, event: &winit::event::KeyEvent) {
        use tuz_input::{Key, NamedKey as N};

        let plain = !self.modifiers.control_key()
            && !self.modifiers.alt_key()
            && !self.modifiers.super_key();

        // A prompt is modal within the sidebar: while one is open every key belongs to
        // it, or typing a name would also be navigating the list underneath.
        let prompting = self
            .sidebar
            .as_ref()
            .map(|e| e.prompt().is_some())
            .unwrap_or(false);

        if prompting {
            self.explorer_prompt_key(chord, event, plain);
            return;
        }

        let Some(explorer) = self.sidebar.as_mut() else {
            return;
        };

        let outcome = match chord.key {
            Key::Named(N::Escape) => crate::explorer::ExplorerOutcome::Unfocus,
            Key::Named(N::Up) => step(explorer.move_selection(-1)),
            Key::Named(N::Down) => step(explorer.move_selection(1)),
            Key::Named(N::PageUp) => step(explorer.move_selection(-10)),
            Key::Named(N::PageDown) => step(explorer.move_selection(10)),
            Key::Named(N::Home) => step(explorer.move_selection(i32::MIN / 2)),
            Key::Named(N::End) => step(explorer.move_selection(i32::MAX / 2)),
            Key::Named(N::Enter) => explorer.activate(),
            Key::Named(N::Backspace) => step(explorer.go_up()),
            Key::Char('r') if plain => step(explorer.begin_rename()),
            Key::Char('n') if plain => {
                explorer.begin_new_folder();
                crate::explorer::ExplorerOutcome::Redraw
            }
            Key::Char('d') if plain => step(explorer.begin_delete()),
            Key::Char('h') if plain => {
                let show = !self.settings.config().explorer.show_hidden;
                let actions = self.settings.modify(|c| c.explorer.show_hidden = show);
                if let Some(e) = self.sidebar.as_mut() {
                    e.set_show_hidden(show);
                }
                self.apply_reload_actions(&actions);
                crate::explorer::ExplorerOutcome::Redraw
            }
            // The three actions that reach into the shell.
            Key::Char('c') if plain => match explorer.selected() {
                Some(entry) if entry.kind != tuz_ui::EntryKind::File => {
                    crate::explorer::ExplorerOutcome::RunCd(entry.path.clone())
                }
                _ => crate::explorer::ExplorerOutcome::Continue,
            },
            Key::Char('e') if plain => match explorer.selected() {
                Some(entry) => crate::explorer::ExplorerOutcome::OpenEditor(entry.path.clone()),
                None => crate::explorer::ExplorerOutcome::Continue,
            },
            Key::Char('p') if plain => match explorer.selected() {
                Some(entry) => crate::explorer::ExplorerOutcome::InsertPath(entry.path.clone()),
                None => crate::explorer::ExplorerOutcome::Continue,
            },
            _ => crate::explorer::ExplorerOutcome::Continue,
        };
        self.handle_explorer_outcome(outcome);
    }

    /// Keys while a rename / new-folder / delete prompt is open.
    fn explorer_prompt_key(
        &mut self,
        chord: &tuz_input::KeyChord,
        event: &winit::event::KeyEvent,
        plain: bool,
    ) {
        use tuz_input::{Key, NamedKey as N};

        let confirming_delete = matches!(
            self.sidebar.as_ref().and_then(|e| e.prompt()),
            Some(crate::explorer::Prompt::Delete { .. })
        );

        let Some(explorer) = self.sidebar.as_mut() else {
            return;
        };

        match chord.key {
            Key::Named(N::Escape) => {
                explorer.cancel_prompt();
                self.request_redraw();
            }
            // A delete asks y/n rather than taking Enter, so the key that confirms a
            // rename cannot also confirm a deletion by muscle memory.
            Key::Char('y') if confirming_delete && plain => self.commit_explorer_prompt(),
            Key::Char('n') if confirming_delete && plain => {
                explorer.cancel_prompt();
                self.request_redraw();
            }
            _ if confirming_delete => {}

            Key::Named(N::Enter) => self.commit_explorer_prompt(),
            Key::Named(N::Backspace) => {
                explorer.prompt_backspace();
                self.request_redraw();
            }
            _ => {
                if plain {
                    if let Some(text) = event.text.as_deref() {
                        if explorer.prompt_input(text) {
                            self.request_redraw();
                        }
                    }
                }
            }
        }
    }

    fn commit_explorer_prompt(&mut self) {
        let Some(explorer) = self.sidebar.as_mut() else {
            return;
        };
        match explorer.commit_prompt() {
            Ok(_) => {}
            // Never silent: a failed rename that just closed the prompt would look
            // like it worked.
            Err(message) => self.notify(message, tuz_plugin_api::NotifyLevel::Error),
        }
        self.request_redraw();
    }

    /// Open or close the new-tab dropdown.
    ///
    /// A separate chevron rather than a menu on the `+` itself: the common case is
    /// wanting another tab of the shell you already use, and making that a two-step
    /// gesture to serve the rare case would be the wrong trade.
    fn toggle_new_tab_menu(&mut self) {
        if self.menu.is_some() {
            self.close_menu();
            return;
        }
        let Some(anchor) = self.frame.as_ref().and_then(|f| {
            f.actions
                .iter()
                .find(|(b, _)| *b == ChromeButton::NewTabMenu)
                .map(|(_, rect)| *rect)
        }) else {
            return;
        };

        let items: Vec<crate::menu::MenuItem> = crate::shells::available()
            .into_iter()
            .map(|shell| crate::menu::MenuItem {
                label: shell.name.clone(),
                value: shell.path.display().to_string(),
            })
            .collect();

        if items.is_empty() {
            // Nothing to choose between, so a menu would be an empty box. Fall back to
            // what the button beside it does rather than showing nothing at all.
            self.new_tab_with(None);
            return;
        }

        self.menu = Some(crate::menu::Menu::new(
            crate::menu::MenuKind::NewTabShell,
            anchor,
            items,
        ));
        self.request_redraw();
    }

    /// Move the menu's selection to the row under the pointer.
    ///
    /// Hover and selection are the same thing here rather than two states: in a menu
    /// you expect Enter to take whatever you are pointing at, and a separate hover
    /// highlight would leave two rows looking chosen at once.
    ///
    /// A pointer outside the rows leaves the selection where it was, so drifting off
    /// the edge on the way to a row does not lose your place.
    fn update_menu_hover(&mut self, x: i32, y: i32) {
        let cell_height = self.cell_size().height;
        let Some(rect) = self.menu_rect else {
            return;
        };
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        let Some(index) = menu.row_at(rect, cell_height, x, y) else {
            return;
        };
        if menu.selected != index {
            menu.selected = index;
            self.request_redraw();
        }
    }

    fn move_menu(&mut self, delta: i32) {
        if let Some(menu) = self.menu.as_mut() {
            menu.move_selection(delta);
            self.request_redraw();
        }
    }

    /// Act on the highlighted row and close.
    fn pick_menu_item(&mut self) {
        let choice = self
            .menu
            .as_ref()
            .and_then(|m| m.selected().map(|item| (m.kind, item.value.clone())));
        self.close_menu();

        let Some((kind, value)) = choice else {
            return;
        };
        match kind {
            crate::menu::MenuKind::NewTabShell => self.new_tab_with(Some(value)),
            crate::menu::MenuKind::AppMenu => match value.as_str() {
                "settings" => self.open_settings(),
                "help" => self.toggle_help(),
                "plugins" => self.toggle_plugins(),
                other => log::debug!("unknown menu entry `{other}`"),
            },
        }
    }

    /// Open the menu that groups the pages you reach occasionally.
    fn toggle_app_menu(&mut self) {
        if self.menu.is_some() {
            self.close_menu();
            return;
        }
        let Some(anchor) = self.frame.as_ref().and_then(|f| {
            f.actions
                .iter()
                .find(|(b, _)| *b == ChromeButton::AppMenu)
                .map(|(_, rect)| *rect)
        }) else {
            return;
        };

        let items = [
            ("Settings", "settings"),
            ("Shortcuts", "help"),
            ("Plugins", "plugins"),
        ]
        .into_iter()
        .map(|(label, value)| crate::menu::MenuItem {
            label: label.to_owned(),
            value: value.to_owned(),
        })
        .collect();

        self.menu = Some(crate::menu::Menu::new(
            crate::menu::MenuKind::AppMenu,
            anchor,
            items,
        ));
        self.request_redraw();
    }

    fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            self.menu_rect = None;
            self.request_redraw();
        }
    }

    /// Open a tab running `shell`, or the configured default when `None`.
    fn new_tab_with(&mut self, shell: Option<String>) {
        // Before `new_tab`, which moves focus to the tab being created.
        let inherited = self.focused_directory();
        let pane = self.layout.new_tab();
        self.pending_shell = shell;
        self.pending_cwd = inherited;
        self.ensure_session(pane);
        self.pending_shell = None;
        self.pending_cwd = None;
        self.relayout();
        self.request_redraw();
    }

    /// The working directory of the focused pane's shell, if it can be read.
    ///
    /// `None` covers a pane whose shell has exited, a platform without `/proc`, and the
    /// case of there being no pane at all — the first launch. Each of those ends with
    /// the new shell starting in the home directory instead.
    fn focused_directory(&self) -> Option<std::path::PathBuf> {
        let pid = self.focused_session()?.child_pid()?;
        crate::proc::working_directory(pid)
    }

    /// Everything on disk, whether or not it loaded.
    ///
    /// Built from `discover` rather than from the running host: a disabled plugin is
    /// never loaded, so a list of loaded plugins could show you how to turn things
    /// off and never how to turn them back on.
    fn installed_plugins(&self) -> Vec<crate::plugins::Installed> {
        let dirs = self.settings.paths().plugin_dirs().to_vec();
        let running: Vec<&str> = self
            .plugins
            .plugins()
            .iter()
            .map(|p| p.manifest.name.as_str())
            .collect();

        tuz_plugin::discover(&dirs)
            .into_iter()
            .filter_map(|found| found.ok())
            .map(|(directory, manifest)| {
                let problem = if running.contains(&manifest.name.as_str()) {
                    None
                } else if !self.settings.config().plugins.enabled {
                    Some("plugins are off".to_owned())
                } else {
                    Some("not loaded".to_owned())
                };
                crate::plugins::Installed {
                    manifest,
                    directory,
                    problem,
                }
            })
            .collect()
    }

    fn toggle_plugins(&mut self) {
        if let Some(index) = self.layout.tab_of_kind(TabKind::Plugins) {
            if self.layout.active_kind() == TabKind::Plugins {
                self.close_plugins();
                return;
            }
            if self.layout.select_tab(index) {
                self.relayout();
                self.on_tab_activated();
            }
            self.request_redraw();
            return;
        }
        let found = self.installed_plugins();
        let install_dir = self
            .settings
            .paths()
            .plugin_dirs()
            .first()
            .cloned()
            .unwrap_or_default();
        self.plugins_page = Some(crate::plugins::PluginsPage::open(found, install_dir));
        self.layout.new_tab_of(TabKind::Plugins);
        self.relayout();
        self.request_redraw();
    }

    fn close_plugins(&mut self) {
        self.plugins_page = None;
        if let Some(index) = self.layout.tab_of_kind(TabKind::Plugins) {
            if let Some(panes) = self.layout.close_tab(index) {
                for pane in panes {
                    self.drop_session(pane);
                }
            }
            if self.layout.is_empty() {
                self.exit_requested = true;
                return;
            }
            self.relayout();
        }
        self.request_redraw();
    }

    fn plugins_active(&self) -> bool {
        self.plugins_page.is_some() && self.layout.active_kind() == TabKind::Plugins
    }

    /// Load plugins again from disk and rebuild the keymap.
    ///
    /// Registered keybinds and commands are cleared by the reload, so the keymap has
    /// to be rebuilt from what comes back or a toggled-off plugin keeps its bindings.
    fn reload_plugin_host(&mut self) {
        let dirs = self.settings.paths().plugin_dirs().to_vec();
        let cfg = self.settings.config().plugins.clone();
        for error in self.plugins.reload(&dirs, &cfg) {
            log::warn!("plugin reload: {error}");
        }
        self.keymap = build_keymap(&self.settings, &self.plugins);

        // The page lists what is on disk, which an import just changed.
        let found = self.installed_plugins();
        if let Some(page) = self.plugins_page.as_mut() {
            page.refresh(found);
        }
    }

    /// Open the system folder chooser, off the event loop.
    ///
    /// The dialog is modal to the desktop, not to us: the terminal keeps drawing and
    /// keeps running shells while it is up. The result comes back through the same
    /// proxy the PTY threads use.
    fn pick_folder(&self, purpose: FolderPurpose) {
        let proxy = self.proxy.clone();
        let title = match purpose {
            FolderPurpose::ImportPlugin => "Choose a plugin folder",
            FolderPurpose::ExportPlugins => "Choose where to export plugins",
        };

        std::thread::spawn(move || {
            let path = rfd::FileDialog::new().set_title(title).pick_folder();
            // A closed event loop means the terminal is shutting down; dropping the
            // answer is correct.
            let _ = proxy.send_event(UserEvent::FolderPicked { purpose, path });
        });
    }

    /// Apply an action from the plugins page.
    fn handle_plugins_action(&mut self, action: tuz_ui::UiAction) {
        let Some(mut page) = self.plugins_page.take() else {
            return;
        };
        let mut next = self.settings.config().clone();
        let outcome = page.apply(action, &mut next);

        match outcome {
            crate::plugins::PluginsOutcome::Continue => {}
            crate::plugins::PluginsOutcome::ChooseFolder(purpose) => self.pick_folder(purpose),
            crate::plugins::PluginsOutcome::Toggled => {
                let actions = self.settings.modify(|c| *c = next);
                self.apply_reload_actions(&actions);
                self.reload_plugin_host();
            }
            crate::plugins::PluginsOutcome::Close => {
                self.close_plugins();
                return;
            }
        }

        self.plugins_page = Some(page);
        self.request_redraw();
    }

    /// Open the shortcut reference, or return to it if it is already open.
    ///
    /// A tab rather than an overlay for the same reason settings is one: you want to
    /// read it *while* trying the keys, which means switching away and back without
    /// losing your place.
    fn toggle_help(&mut self) {
        if let Some(index) = self.layout.tab_of_kind(TabKind::Help) {
            if self.layout.active_kind() == TabKind::Help {
                self.close_help();
                return;
            }
            if self.layout.select_tab(index) {
                self.relayout();
                self.on_tab_activated();
            }
            self.request_redraw();
            return;
        }
        self.help = Some(crate::help::HelpPage::open());
        self.layout.new_tab_of(TabKind::Help);
        self.relayout();
        self.request_redraw();
    }

    fn close_help(&mut self) {
        self.help = None;
        if let Some(index) = self.layout.tab_of_kind(TabKind::Help) {
            if let Some(panes) = self.layout.close_tab(index) {
                for pane in panes {
                    self.drop_session(pane);
                }
            }
            if self.layout.is_empty() {
                self.exit_requested = true;
                return;
            }
            self.relayout();
        }
        self.request_redraw();
    }

    /// True when the reference is the tab on screen.
    fn help_active(&self) -> bool {
        self.help.is_some() && self.layout.active_kind() == TabKind::Help
    }

    /// Toggle the explorer sidebar.
    ///
    /// Opening also focuses it: you pressed a key to use it, not to look at it. Every
    /// pane regrids, because the sidebar takes its width out of the pane body.
    fn toggle_explorer(&mut self) {
        if self.sidebar.is_some() {
            self.sidebar = None;
            self.sidebar_focused = false;
        } else {
            let dir = self.explorer_start_dir();
            let show_hidden = self.settings.config().explorer.show_hidden;
            self.sidebar = Some(crate::explorer::Explorer::open(dir, show_hidden));
            self.sidebar_focused = true;
        }
        self.relayout();
        self.request_redraw();
    }

    /// Where the explorer opens: the focused shell's directory, or `$HOME`.
    ///
    /// Read directly rather than through `self.cwd`, which is only populated when the
    /// status bar is enabled and so cannot be relied on to be warm.
    fn explorer_start_dir(&self) -> std::path::PathBuf {
        self.focused_session()
            .and_then(|s| s.child_pid())
            .and_then(crate::proc::working_directory)
            .or_else(crate::proc::home)
            .unwrap_or_else(|| std::path::PathBuf::from("/"))
    }

    /// Give the keyboard back to the terminal without closing the sidebar.
    fn unfocus_explorer(&mut self) {
        if self.sidebar_focused {
            self.sidebar_focused = false;
            self.request_redraw();
        }
    }

    /// Act on what the explorer asked for.
    fn handle_explorer_outcome(&mut self, outcome: crate::explorer::ExplorerOutcome) {
        use crate::explorer::ExplorerOutcome as O;
        match outcome {
            O::Continue => {}
            O::Redraw => self.request_redraw(),
            O::Unfocus => self.unfocus_explorer(),

            // A paste, not typing: bracketed paste tells the shell this is inserted
            // text, and strips the terminator so the path cannot inject keystrokes.
            O::InsertPath(path) => {
                let text = crate::explorer::shell_quote(&path.to_string_lossy());
                let mode = self.focused_mode();
                if let Some(session) = self.focused_session() {
                    session.write(tuz_core::encode_paste(&text, mode));
                }
                self.request_redraw();
            }

            // These two are meant to run, so they must NOT be bracketed — that is
            // precisely the marker that tells a shell not to execute.
            O::RunCd(dir) => {
                let bytes = crate::explorer::cd_command(&dir);
                if let Some(session) = self.focused_session() {
                    session.write(bytes);
                }
                self.request_redraw();
            }
            O::OpenEditor(path) => {
                let bytes = crate::explorer::editor_command(&path);
                if let Some(session) = self.focused_session() {
                    session.write(bytes);
                }
                // Hand the keyboard back: an editor you cannot type into is not open,
                // and pressing Escape first to reach it would be a step nobody guesses.
                self.unfocus_explorer();
                self.request_redraw();
            }
        }
    }

    /// Open the settings page, gathering the option lists once.
    ///
    /// Settings lives in a tab rather than an overlay, so opening it twice returns to
    /// the tab already open — with its scroll position and unsaved edits intact —
    /// rather than stacking a second copy or silently doing nothing.
    fn open_settings(&mut self) {
        if let Some(index) = self.layout.tab_of_kind(TabKind::Settings) {
            if self.layout.select_tab(index) {
                self.relayout();
                self.on_tab_activated();
            }
            self.request_redraw();
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
        // The tab gets a pane like any other so the layout code needs no special
        // case, but no session is ever started for it, so nothing runs behind it.
        self.layout.new_tab_of(TabKind::Settings);
        self.relayout();
        self.request_redraw();
    }

    fn close_settings(&mut self) {
        // Closing keeps any unsaved changes for the session, matching how the
        // font-size keybindings already behave.
        self.panel = None;
        if let Some(index) = self.layout.tab_of_kind(TabKind::Settings) {
            if let Some(panes) = self.layout.close_tab(index) {
                for pane in panes {
                    self.drop_session(pane);
                }
            }
            if self.layout.is_empty() {
                self.exit_requested = true;
                return;
            }
            self.relayout();
        }
        self.request_redraw();
    }

    /// True when the settings page is the tab currently shown.
    ///
    /// The page owns the keyboard and the pointer only while it is the visible tab:
    /// switching to a terminal must hand both straight back, or the settings tab
    /// would keep swallowing input from behind another tab.
    fn settings_active(&self) -> bool {
        self.panel.is_some() && self.layout.active_kind() == TabKind::Settings
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
                // Closing the page means closing the tab that holds it. Dropping the
                // panel alone left the tab in place, showing an empty page that no
                // longer had a panel behind it to draw or click.
                self.close_settings();
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
        if actions.reload_theme {
            // Everything reads the palette through `settings.theme()`, so without
            // this the name changes and not one color does — which is exactly how
            // picking a theme in the panel appeared to do nothing.
            if let Err(e) = self.settings.reload_theme() {
                log::error!("could not load theme: {e}");
                self.notify(
                    format!("theme failed to load: {e}"),
                    tuz_plugin_api::NotifyLevel::Error,
                );
            }
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
    /// Point the cursor at whichever edge would be dragged from here.
    ///
    /// Without this the resize band is invisible and undiscoverable: the window is
    /// resizable but nothing says so. Only changes on transitions, since setting the
    /// cursor on every motion event is a round trip to the compositor per pixel.
    fn update_resize_cursor(&mut self, x: i32, y: i32) {
        // The sidebar's grip, when the window's own resize band does not claim it.
        let on_grip = self
            .frame
            .as_ref()
            .map(|f| f.sidebar.width > 0 && (x - f.sidebar.right()).abs() <= DIVIDER_GRAB as i32)
            .unwrap_or(false);

        let icon = self.resize_edge(x, y).map(|d| match d {
            ResizeDirection::North => CursorIcon::NResize,
            ResizeDirection::South => CursorIcon::SResize,
            ResizeDirection::East => CursorIcon::EResize,
            ResizeDirection::West => CursorIcon::WResize,
            ResizeDirection::NorthEast => CursorIcon::NeResize,
            ResizeDirection::NorthWest => CursorIcon::NwResize,
            ResizeDirection::SouthEast => CursorIcon::SeResize,
            ResizeDirection::SouthWest => CursorIcon::SwResize,
        });
        let icon = icon.or(if on_grip {
            Some(CursorIcon::ColResize)
        } else {
            None
        });
        if icon == self.resize_cursor {
            return;
        }
        self.resize_cursor = icon;
        if let Some(w) = &self.window {
            w.set_cursor(icon.unwrap_or(CursorIcon::Default));
        }
    }

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

    /// The action a toolbar button performs, where it is one that can be bound.
    ///
    /// Kept immediately beside [`Self::press_chrome_button`], which is the other half:
    /// this exists so a tooltip can name the chord that does the same thing, and the
    /// pair drifting would have a button advertise a key that does something else. The
    /// test below walks every button and checks the two agree.
    ///
    /// `None` for the four window controls and the two dropdowns. A dropdown is opened
    /// by the button and has no chord of its own, and minimize/maximize/close belong to
    /// the compositor's own bindings rather than ours.
    fn button_action(button: ChromeButton) -> Option<Action> {
        Some(match button {
            ChromeButton::NewTab => Action::NewTab,
            ChromeButton::Settings => Action::OpenSettings,
            ChromeButton::Explorer => Action::OpenExplorer,
            ChromeButton::Help => Action::OpenHelp,
            ChromeButton::Plugins => Action::OpenPlugins,
            ChromeButton::SplitRight => Action::SplitRight,
            ChromeButton::SplitDown => Action::SplitDown,
            ChromeButton::NewTabMenu
            | ChromeButton::AppMenu
            | ChromeButton::Minimize
            | ChromeButton::Maximize
            | ChromeButton::Close => return None,
        })
    }

    /// The chord to show in a button's tooltip, as the user currently has it bound.
    ///
    /// Read from the live keymap rather than from `DEFAULT_KEYS`, so someone who
    /// rebinds `open_settings` sees their own chord. Unbinding it — `"none"` in config —
    /// leaves the tooltip with just its label, which is correct: there is no key to
    /// advertise.
    ///
    /// The lowest chord when several are bound, matching the sort `chords_for` applies,
    /// so the tooltip does not flip between equivalents from frame to frame.
    fn button_shortcut(&self, button: ChromeButton) -> Option<String> {
        let action = Self::button_action(button)?;
        self.keymap
            .chords_for(&action)
            .first()
            .map(ToString::to_string)
    }

    fn press_chrome_button(&mut self, button: ChromeButton) {
        match button {
            ChromeButton::NewTabMenu => self.toggle_new_tab_menu(),
            ChromeButton::NewTab => {
                self.dispatch(Action::NewTab);
            }
            ChromeButton::Settings => self.open_settings(),
            ChromeButton::Explorer => self.toggle_explorer(),
            ChromeButton::Help => self.toggle_help(),
            ChromeButton::Plugins => self.toggle_plugins(),
            ChromeButton::AppMenu => self.toggle_app_menu(),
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
        // Before `split`, which gives focus to the new half.
        let inherited = self.focused_directory();
        if let Some(pane) = self.layout.split(dir) {
            // Layout first so the new session is spawned with the right grid, then
            // again so the resized sibling's PTY learns its new size.
            self.relayout();
            self.pending_cwd = inherited;
            self.ensure_session(pane);
            self.pending_cwd = None;
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
            self.notify_plugins(PluginEvent::PaneClosed {
                pane: tuz_plugin_api::PaneId(pane.0),
            });
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

        // An open dropdown is modal: it is a small, deliberate choice, and letting
        // keys through to whatever is behind it would make Escape the only safe key.
        if self.menu.is_some() {
            use tuz_input::{Key, NamedKey as N};
            match chord.key {
                Key::Named(N::Escape) => self.close_menu(),
                Key::Named(N::Up) => self.move_menu(-1),
                Key::Named(N::Down) => self.move_menu(1),
                Key::Named(N::Home) => self.move_menu(i32::MIN / 2),
                Key::Named(N::End) => self.move_menu(i32::MAX / 2),
                Key::Named(N::Enter) | Key::Named(N::Space) => self.pick_menu_item(),
                _ => self.close_menu(),
            }
            return;
        }

        // The plugins page has editable fields and pressable rows, so it routes
        // exactly like settings rather than like the read-only reference page.
        if self.plugins_active() {
            if self.keymap.lookup(&chord) == Some(&Action::OpenPlugins) {
                self.close_plugins();
                return;
            }
            if !self.modifiers.control_key()
                && !self.modifiers.alt_key()
                && !self.modifiers.super_key()
            {
                if let Some(text) = event.text.as_deref() {
                    let mut edited = None;
                    if let Some(page) = self.plugins_page.as_mut() {
                        for c in text.chars() {
                            if let Some(action) = page.ui.type_char(c) {
                                edited = Some(action);
                            }
                        }
                    }
                    if let Some(action) = edited {
                        self.handle_plugins_action(action);
                        return;
                    }
                }
            }
            if let Some(key) = panel_key(&chord) {
                let response = match self.plugins_page.as_mut() {
                    Some(page) => page.ui.key(key),
                    None => return,
                };
                if let (Some(page), Some(body)) = (self.plugins_page.as_mut(), self.plugins_body) {
                    page.ui.scroll_to_focus(body);
                }
                match response {
                    tuz_ui::KeyResponse::Close => self.close_plugins(),
                    tuz_ui::KeyResponse::Action(action) => self.handle_plugins_action(action),
                    tuz_ui::KeyResponse::Consumed => self.request_redraw(),
                }
            }
            return;
        }

        // The reference page has nothing to edit or press, so it wants only two
        // things: a way to scroll and a way to leave. Everything else is swallowed
        // rather than reaching a shell that is not on screen.
        if self.help_active() {
            use tuz_input::{Key, NamedKey as N};
            let scroll = match chord.key {
                Key::Named(N::Escape) => {
                    self.close_help();
                    return;
                }
                Key::Named(N::Up) => -(self.cell_size().height as i32),
                Key::Named(N::Down) => self.cell_size().height as i32,
                Key::Named(N::PageUp) => -(self.cell_size().height as i32 * 10),
                Key::Named(N::PageDown) => self.cell_size().height as i32 * 10,
                Key::Named(N::Home) => i32::MIN / 2,
                Key::Named(N::End) => i32::MAX / 2,
                _ => {
                    // The binding that opened it closes it, matching settings.
                    if self.keymap.lookup(&chord) == Some(&Action::OpenHelp) {
                        self.close_help();
                    }
                    return;
                }
            };
            if let Some(page) = self.help.as_mut() {
                if page.ui.scroll_by(scroll) {
                    self.request_redraw();
                }
            }
            return;
        }

        // The sidebar takes the keyboard only while focused — being open must not,
        // because the whole point of a sidebar is looking at it while typing into the
        // shell beside it. Checked before the keymap so its own keys win, but after
        // nothing: the toggle binding is handled inside `explorer_key` so the chord
        // that opened it still closes it.
        if self.sidebar_focused && self.sidebar.is_some() {
            if self.keymap.lookup(&chord) == Some(&Action::OpenExplorer) {
                self.toggle_explorer();
                return;
            }
            self.explorer_key(&chord, event);
            // Anything else is swallowed rather than reaching the shell, so typing at
            // the sidebar cannot run commands in the terminal behind it.
            return;
        }

        // While the settings tab is the one on screen it owns the keyboard. The
        // binding that opened it still works, so the same chord toggles it shut.
        if self.settings_active() {
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
            self.dragging_sidebar = None;
            let mut cleared = self.pressed_button.take().is_some();
            cleared |= self.pressed_ide.take().is_some();
            // Widget buttons on the tabbed pages, which hold their pressed look only
            // between press and release.
            for ui in [
                self.panel.as_mut().map(|p| &mut p.ui),
                self.plugins_page.as_mut().map(|p| &mut p.ui),
                self.help.as_mut().map(|p| &mut p.ui),
            ]
            .into_iter()
            .flatten()
            {
                cleared |= ui.set_pressed(None);
            }
            if cleared {
                self.request_redraw();
            }
            if let Some(drag) = self.dragging_tab.take() {
                if drag.active && drag.current != drag.origin {
                    log::debug!("moved tab {} to {}", drag.origin, drag.current);
                    self.request_redraw();
                }
            }
        }

        // The resize band comes before everything, including the settings page. It
        // runs along all four window edges, so whatever is drawn there — a tab, a
        // pane, the settings footer — overlaps it, and anything that claims a click
        // first takes the edge with it. The settings page did exactly that, and the
        // bottom edge stopped resizing whenever settings was the open tab.
        if pressed && button == MouseButton::Left {
            if let Some(direction) = self.resize_edge(x, y) {
                if let Some(w) = &self.window {
                    if let Err(e) = w.drag_resize_window(direction) {
                        log::debug!("compositor declined a resize drag: {e}");
                    }
                }
                return;
            }
        }

        // An open dropdown takes the next click wherever it lands: inside, it picks;
        // outside, it dismisses without the click doing anything else. Dismissing
        // *and* acting would mean a click meant to close the menu also split a pane.
        if pressed && self.menu.is_some() {
            let hit = self.menu_rect.and_then(|rect| {
                self.menu
                    .as_ref()
                    .and_then(|m| m.row_at(rect, self.cell_size().height, x, y))
            });
            match hit {
                Some(index) => {
                    if let Some(menu) = self.menu.as_mut() {
                        menu.selected = index;
                    }
                    self.pick_menu_item();
                }
                None => self.close_menu(),
            }
            return;
        }

        // Ahead of the `is_chrome` branch below, which would otherwise read a press
        // on the status bar as grabbing the title bar and start moving the window.
        if pressed && button == MouseButton::Left {
            if let Some(index) = self
                .ide_hits
                .iter()
                .position(|(_, rect)| rect.contains(x, y))
            {
                let id = self.ide_hits[index].0.clone();
                self.pressed_ide = Some(index);
                let commands = self.plugins.click_status_segment(&id);
                self.apply_plugin_commands(commands);
                self.request_redraw();
                return;
            }
        }

        // The grip is checked before the sidebar body, or the rightmost few pixels of
        // the file list would never be draggable.
        if pressed && button == MouseButton::Left && frame.sidebar.width > 0 {
            let edge = frame.sidebar.right();
            if (x - edge).abs() <= DIVIDER_GRAB as i32 && frame.sidebar.contains(x.min(edge - 1), y)
            {
                self.dragging_sidebar = Some(x - edge);
                return;
            }
        }

        // The sidebar claims its own column. This must come before the `is_chrome`
        // branch below, which treats anything it matches as the draggable title bar.
        if pressed && frame.sidebar.width > 0 && frame.sidebar.contains(x, y) {
            self.sidebar_focused = true;
            if let Some(tuz_ui::UiAction::Pressed(id)) =
                self.sidebar.as_mut().and_then(|e| e.ui.click(x, y))
            {
                self.explorer_click(id);
            }
            self.request_redraw();
            return;
        }

        if self.plugins_active() {
            let page = frame.panes.first().map(|p| p.rect);
            if page.is_some_and(|r| r.contains(x, y)) {
                if !pressed {
                    return;
                }
                if let Some(page) = self.plugins_page.as_mut() {
                    let hit = page.ui.hit(x, y);
                    page.ui.set_pressed(hit);
                }
                if let Some(action) = self.plugins_page.as_mut().and_then(|p| p.ui.click(x, y)) {
                    self.handle_plugins_action(action);
                } else {
                    self.request_redraw();
                }
                return;
            }
        }

        // The settings page takes clicks that land on it, and only those. It fills a
        // tab rather than floating over the window, so the strip above it must stay
        // live — swallowing everything here would leave no way to click back to a
        // terminal.
        if self.settings_active() {
            let page = frame.panes.first().map(|p| p.rect);
            if page.is_some_and(|r| r.contains(x, y)) {
                if !pressed {
                    return;
                }
                if let Some(panel) = self.panel.as_mut() {
                    let hit = panel.ui.hit(x, y);
                    panel.ui.set_pressed(hit);
                }
                if let Some(action) = self.panel.as_mut().and_then(|p| p.ui.click(x, y)) {
                    self.handle_panel_action(action);
                } else {
                    self.request_redraw();
                }
                return;
            }
        }

        if pressed {
            // Buttons come before tabs: a close button sits inside its tab, so
            // checking the tab first would swallow the click.
            if let Some(button) = frame.action_at(x, y) {
                log::debug!("chrome button: {}", button.describe());
                self.pressed_button = Some(button);
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
                    self.on_tab_activated();
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
                // Clicking into a terminal is the clearest possible statement that
                // typing should go there.
                self.unfocus_explorer();
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

        // An open dropdown is modal, so nothing behind it should react to the pointer
        // — including the window's resize cursor, which would otherwise change shape
        // over a menu that has already claimed the next click.
        if self.menu.is_some() {
            self.update_menu_hover(x as i32, y as i32);
            return;
        }

        self.update_resize_cursor(x as i32, y as i32);

        if let Some(offset) = self.dragging_sidebar {
            self.drag_sidebar(x as i32 - offset);
            return;
        }

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
        // A tabbed page occupies its tab, not the whole window, so chrome hover stays
        // live above it. Copied out so the frame borrow ends before the pages are
        // borrowed mutably.
        let pane_rect: Option<Rect> = self
            .frame
            .as_ref()
            .and_then(|f| f.panes.first().map(|p| p.rect));
        let kind = self.layout.active_kind();
        let page_of = |want: TabKind| if kind == want { pane_rect } else { None };

        // Outside its own page the pointer belongs to the chrome, and each page is
        // told so explicitly — otherwise a row stays highlighted after the pointer
        // has left the page entirely.
        let at = |page: Option<Rect>| match page {
            Some(r) if r.contains(x, y) => (x, y),
            _ => (i32::MIN, i32::MIN),
        };

        let mut changed = false;
        // Every page with rows, not just settings. The plugins page had no hover at
        // all because it was never given the pointer.
        if let Some(panel) = self.panel.as_mut() {
            let (px, py) = at(page_of(TabKind::Settings));
            changed |= panel.ui.set_pointer(px, py);
        }
        if let Some(page) = self.plugins_page.as_mut() {
            let (px, py) = at(page_of(TabKind::Plugins));
            changed |= page.ui.set_pointer(px, py);
        }
        if let Some(help) = self.help.as_mut() {
            let (px, py) = at(page_of(TabKind::Help));
            changed |= help.ui.set_pointer(px, py);
        }

        let Some(frame) = self.frame.as_ref() else {
            return changed;
        };

        let ide = self
            .ide_hits
            .iter()
            .position(|(_, rect)| rect.contains(x, y));
        let ide_changed = ide != self.hovered_ide;
        self.hovered_ide = ide;

        let button = frame.action_at(x, y);
        let tab = frame.tab_at(x, y);
        let close = frame.tab_close_at(x, y).is_some();

        let changed = changed
            || ide_changed
            || button != self.hovered_button
            || tab != self.hovered_tab
            || close != self.hovered_close;

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
        // The sidebar scrolls when the pointer is over it, whatever has focus —
        // scrolling is a pointer gesture, not a keyboard one.
        if self.plugins_active() {
            let cell_height = self.cell_size().height as f32;
            let pixels = match delta {
                MouseScrollDelta::LineDelta(_, y) => -(y * 3.0 * cell_height),
                MouseScrollDelta::PixelDelta(p) => -(p.y as f32),
            };
            if let Some(page) = self.plugins_page.as_mut() {
                if page.ui.scroll_by(pixels as i32) {
                    self.request_redraw();
                }
            }
            return;
        }

        if self.help_active() {
            let cell_height = self.cell_size().height as f32;
            let pixels = match delta {
                MouseScrollDelta::LineDelta(_, y) => -(y * 3.0 * cell_height),
                MouseScrollDelta::PixelDelta(p) => -(p.y as f32),
            };
            if let Some(page) = self.help.as_mut() {
                if page.ui.scroll_by(pixels as i32) {
                    self.request_redraw();
                }
            }
            return;
        }

        let over_sidebar = self
            .frame
            .as_ref()
            .map(|f| {
                f.sidebar.width > 0 && f.sidebar.contains(self.mouse.0 as i32, self.mouse.1 as i32)
            })
            .unwrap_or(false);
        if over_sidebar {
            let cell_height = self.cell_size().height as f32;
            let pixels = match delta {
                MouseScrollDelta::LineDelta(_, y) => -(y * 3.0 * cell_height),
                MouseScrollDelta::PixelDelta(p) => -(p.y as f32),
            };
            if let Some(explorer) = self.sidebar.as_mut() {
                if explorer.ui.scroll_by(pixels as i32) {
                    self.request_redraw();
                }
            }
            return;
        }

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

        // Wayland takes the icon from the `.desktop` file rather than the window, so
        // this is what X11 and the app switcher use; `assets/tuzminal.desktop` covers
        // the Wayland case.
        let icon = {
            let (pixels, size) = crate::appicon::rgba();
            winit::window::Icon::from_rgba(pixels, size, size)
                .map_err(|e| log::debug!("could not build the window icon: {e}"))
                .ok()
        };

        let attrs = Window::default_attributes()
            .with_title(&cfg.window.title)
            .with_window_icon(icon)
            .with_decorations(cfg.window.decorations)
            // Rounded corners need an alpha channel just as much as opacity does:
            // the corner pixels are transparent, and on an opaque surface they would
            // come out black instead.
            .with_transparent(cfg.window.opacity < 1.0 || cfg.window.corner_radius > 0.0)
            .with_inner_size(winit::dpi::LogicalSize::new(width, height));

        // The application id ties the window to `tuzminal.desktop`. Without it the
        // compositor has no name or icon for us and calls the window "Unknown" — which
        // is what a GNOME "not responding" dialog was reporting.
        #[cfg(all(unix, not(target_os = "macos")))]
        let attrs = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            use winit::platform::x11::WindowAttributesExtX11;
            WindowAttributesExtX11::with_name(
                WindowAttributesExtWayland::with_name(attrs, APP_ID, APP_ID),
                APP_ID,
                APP_ID,
            )
        };

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

        // Honour the config key, but do not take the keyboard: a sidebar that is open
        // because it was configured that way is not one you just asked for, and the
        // first thing anyone does in a terminal is type.
        if self.settings.config().explorer.enabled {
            let dir = self.explorer_start_dir();
            let show_hidden = self.settings.config().explorer.show_hidden;
            self.sidebar = Some(crate::explorer::Explorer::open(dir, show_hidden));
        }

        self.relayout();
        self.request_redraw();
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::ConfigChanged => self.reload_config(),
            UserEvent::Wakeup => self.drain_pty_events(),
            UserEvent::FolderPicked { purpose, path } => {
                // `None` is a cancelled dialog, which is not an error and should
                // leave whatever was already typed alone.
                // `None` is a cancelled dialog, which is not an error.
                let Some(path) = path else { return };
                let Some(mut page) = self.plugins_page.take() else {
                    return;
                };
                let changed = page.folder_chosen(purpose, path);
                self.plugins_page = Some(page);

                if changed {
                    self.reload_plugin_host();
                }
                self.request_redraw();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                // Recorded, not acted on. This used to reconfigure the swapchain,
                // relayout every pane and paint a frame right here, on the reasoning
                // that a queued redraw would leave the content a frame behind the
                // border. That reasoning had the cost backwards.
                //
                // All three of the things it did are expensive, and one of them is much
                // worse than it looks. A relayout resizes every pane's terminal, and
                // `Term::resize` rewraps the entire scrollback whenever the column
                // count changes. Measured on this machine, one column step costs:
                //
                //     empty history         7µs
                //      1,000 lines       1.16ms
                //     10,000 lines       6.68ms
                //
                // A height-only change is free by comparison (under a microsecond),
                // because nothing needs rewrapping.
                //
                // So a horizontal drag with a full scrollback cost ~6.7ms of reflow per
                // resize event, and a drag delivers those far faster than the refresh
                // rate. Add a swapchain reconfiguration and a paint that blocks in
                // `get_current_texture`, and events arrived faster than they could be
                // retired; the queue grew and the window fell further behind the pointer
                // the longer the drag went on.
                //
                // Worth knowing if this looks slow again: the first time it was measured
                // it came out at 0.17ms, on a terminal with no output in it. An empty
                // history has nothing to rewrap, so that number said nothing at all
                // about the case that hurts.
                //
                // Only the last size in a batch is real; the rest describe sizes the
                // window has already stopped being. So keep the newest and let
                // `RedrawRequested` do the work once, which is also the point at which
                // the compositor is ready for another frame.
                self.pending_size = Some(size);
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
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

/// Which window edge or corner a point falls in, for a window of `width` x `height`.
///
/// Split out from `App` so the geometry can be tested without a window: it decides
/// whether a borderless window can be resized at all, and the corners in particular
/// are easy to get subtly wrong.
fn resize_edge_at(x: i32, y: i32, width: i32, height: i32) -> Option<ResizeDirection> {
    let left = x <= RESIZE_BORDER;
    let right = x >= width - RESIZE_BORDER;
    let top = y <= RESIZE_BORDER;
    let bottom = y >= height - RESIZE_BORDER;

    Some(match (top, bottom, left, right) {
        (true, _, true, _) => ResizeDirection::NorthWest,
        (true, _, _, true) => ResizeDirection::NorthEast,
        (_, true, true, _) => ResizeDirection::SouthWest,
        (_, true, _, true) => ResizeDirection::SouthEast,
        (true, ..) => ResizeDirection::North,
        (_, true, ..) => ResizeDirection::South,
        (_, _, true, _) => ResizeDirection::West,
        (_, _, _, true) => ResizeDirection::East,
        _ => return None,
    })
}

/// A selection move that did nothing is not worth a frame.
fn step(moved: bool) -> crate::explorer::ExplorerOutcome {
    if moved {
        crate::explorer::ExplorerOutcome::Redraw
    } else {
        crate::explorer::ExplorerOutcome::Continue
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

#[cfg(test)]
mod resize_tests {
    use super::*;

    const W: i32 = 800;
    const H: i32 = 600;

    #[test]
    fn each_edge_and_corner_reports_its_own_direction() {
        use ResizeDirection::*;
        let cases = [
            ((0, 0), Some(NorthWest)),
            ((W - 1, 0), Some(NorthEast)),
            ((0, H - 1), Some(SouthWest)),
            ((W - 1, H - 1), Some(SouthEast)),
            ((W / 2, 0), Some(North)),
            ((W / 2, H - 1), Some(South)),
            ((0, H / 2), Some(West)),
            ((W - 1, H / 2), Some(East)),
            ((W / 2, H / 2), None),
        ];
        for ((x, y), want) in cases {
            assert_eq!(
                resize_edge_at(x, y, W, H),
                want,
                "({x}, {y}) should resolve to {want:?}"
            );
        }
    }

    #[test]
    fn the_bottom_band_is_live_and_just_inside_it_is_not() {
        // The band along the bottom is what the settings page used to swallow, which
        // left the window unresizable from that edge whenever settings was open.
        assert_eq!(
            resize_edge_at(W / 2, H - RESIZE_BORDER, W, H),
            Some(ResizeDirection::South)
        );
        assert_eq!(resize_edge_at(W / 2, H - RESIZE_BORDER - 1, W, H), None);
    }
}

#[cfg(test)]
mod border_tests {
    use super::*;

    fn cfg() -> tuz_config::Config {
        tuz_config::Config::default()
    }

    const SIZE: (u32, u32) = (800, 600);

    #[test]
    fn a_borderless_window_gets_the_configured_width() {
        let mut c = cfg();
        c.window.decorations = false;
        c.window.border_width = 2.0;
        assert_eq!(App::border_width(&c, false, SIZE), 2.0);
    }

    #[test]
    fn decorations_and_maximizing_each_suppress_it() {
        // Decorated, the compositor draws the frame and this would sit inside it.
        // Maximized, the window edge is the screen edge and there is nothing to
        // separate the window from.
        let mut c = cfg();
        c.window.border_width = 2.0;

        c.window.decorations = true;
        assert_eq!(App::border_width(&c, false, SIZE), 0.0);

        c.window.decorations = false;
        assert_eq!(App::border_width(&c, true, SIZE), 0.0);
    }

    #[test]
    fn an_absurd_width_is_clamped_instead_of_inverting_the_background() {
        // The background is drawn inset by this much on each side. Left unclamped, a
        // width past half the smaller dimension makes that inset negative, and the
        // window fills with the outline color.
        let mut c = cfg();
        c.window.border_width = 10_000.0;
        let border = App::border_width(&c, false, SIZE);
        assert!(border > 0.0);
        assert!(
            600.0 - border * 2.0 > 0.0,
            "{border} leaves no room for the background"
        );
    }

    #[test]
    fn a_negative_width_is_treated_as_none() {
        let mut c = cfg();
        c.window.border_width = -4.0;
        assert_eq!(App::border_width(&c, false, SIZE), 0.0);
    }

    #[test]
    fn the_default_config_draws_a_border() {
        // The whole point of the feature: it is on without being asked for. A default
        // of zero would make this dead code for everyone who never opens config.toml.
        assert!(App::border_width(&cfg(), false, SIZE) > 0.0);
    }
}

#[cfg(test)]
mod tooltip_shortcut_tests {
    use super::*;

    /// Every button that runs an action must name it, and no other button may.
    ///
    /// The two halves are separate matches — `press_chrome_button` does the thing,
    /// `button_action` names it for the tooltip — so nothing but this stops a new button
    /// getting a handler and no chord, or worse, being credited with the wrong one.
    #[test]
    fn the_buttons_with_actions_are_exactly_the_ones_with_shortcuts() {
        // The window controls and the two dropdowns. A dropdown is opened by its button
        // and has no chord of its own; minimize, maximize and close are the
        // compositor's bindings rather than ours.
        let expected_none = [
            ChromeButton::NewTabMenu,
            ChromeButton::AppMenu,
            ChromeButton::Minimize,
            ChromeButton::Maximize,
            ChromeButton::Close,
        ];

        for button in ChromeButton::ALL {
            let action = App::button_action(button);
            let should_have = !expected_none.contains(&button);
            assert_eq!(
                action.is_some(),
                should_have,
                "{button:?}: describe() is {:?}, action is {action:?}",
                button.describe()
            );
        }
    }

    #[test]
    fn every_named_action_is_bound_by_default_so_the_tooltip_says_something() {
        // A button that names an action nothing is bound to shows a bare label. That is
        // correct behaviour for an unbound chord, but out of the box each of these
        // should carry a key — otherwise the feature is invisible on a fresh install.
        let keymap = Keymap::from_config(
            tuz_config::DEFAULT_KEYS.iter().copied(),
            &std::collections::HashSet::new(),
        )
        .keymap;

        for button in ChromeButton::ALL {
            let Some(action) = App::button_action(button) else {
                continue;
            };
            assert!(
                !keymap.chords_for(&action).is_empty(),
                "{button:?} points at {action:?}, which no default chord binds"
            );
        }
    }

    #[test]
    fn the_chord_shown_is_the_lowest_of_several() {
        // Four arrow/letter pairs are bound to the focus actions, and the tooltip must
        // pick deterministically or it flickers between them as the map is rebuilt.
        let keymap = Keymap::from_config(
            tuz_config::DEFAULT_KEYS.iter().copied(),
            &std::collections::HashSet::new(),
        )
        .keymap;
        let chords = keymap.chords_for(&Action::NewTab);
        assert!(!chords.is_empty());
        let mut sorted = chords.clone();
        sorted.sort();
        assert_eq!(chords, sorted, "chords_for must return a stable order");
    }
}
