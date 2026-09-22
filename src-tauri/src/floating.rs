//! The desktop floating widget: a semi-transparent ball whose water level is
//! the machine's memory usage.
//!
//! The ball lives in its own small always-on-top window. Everything that owns
//! state — whether the widget is showing, where the ball sits, whether the
//! hover panel is up — is kept here, so the widget's frontend only renders and
//! asks. Two details are worth knowing before changing anything:
//!
//! * The window is exactly as big as what it shows. A transparent window still
//!   swallows clicks, so an always-panel-sized window would put a dead
//!   rectangle on the desktop. Hovering therefore resizes it.
//! * Growing upward moves the window origin as well as its size, and the
//!   collapse restores the recorded origin, so the ball never drifts no matter
//!   how often the panel opens.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{
    menu::CheckMenuItem, AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State,
    WebviewWindow, Wry,
};

use crate::utils::{platform, WindleError, Result};

/// Window label, and how the frontend recognises which window it is.
pub const WINDOW_LABEL: &str = "floating";

/// Fired whenever the widget is shown or hidden, so the sidebar toggle, the
/// tray item and the widget itself all agree on the state.
pub const VISIBILITY_EVENT: &str = "floating://visibility";

/// Fired with the pointer's arrival over the widget and its departure again.
///
/// The webview's own `pointerenter`/`pointerleave` only ever arrive while this
/// app is the active one — macOS delivers mouse tracking to the active app
/// alone — so the ball would stop answering the pointer as soon as the user
/// turns to another app. The pointer's position, unlike the events, is a global
/// reading, and it is that which is watched here.
pub const HOVER_EVENT: &str = "floating://hover";

/// How often the pointer is looked at while the widget is on screen.
const HOVER_INTERVAL: Duration = Duration::from_millis(120);

/// Side of the ball in logical pixels. The ball fills the collapsed window
/// exactly; the same number has to line up with `BALL_SIZE` in the frontend.
pub const BALL_SIZE: f64 = 33.0;

/// Width of the hover panel in logical pixels, matching the frontend's CSS.
/// Wider than the ball, which is what makes the window grow sideways too.
pub const PANEL_WIDTH: f64 = 132.0;

/// Height of the hover panel in logical pixels, matching the frontend's CSS.
pub const PANEL_HEIGHT: f64 = 132.0;

/// Gap kept from the screen edge the first time the widget appears.
const DEFAULT_MARGIN: f64 = 28.0;

/// Position writes are throttled: a drag emits `Moved` for every frame.
const SAVE_INTERVAL: Duration = Duration::from_secs(1);

/// Which side of the ball the hover panel opens on. The frontend picks it —
/// only it knows how much room the window has around it — and passes it along
/// with the resize, which is what makes the ball hold still.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    Above,
    Below,
}

/// Which edge of the window the ball sits on once the panel is open, and so
/// which way the panel reaches out. Same idea as [`Placement`], for the axis
/// the panel is wider than the ball.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
}

/// The tray item whose checkmark mirrors the widget, kept so the two entry
/// points to the feature cannot fall out of step.
pub struct TrayFloatingItem(pub CheckMenuItem<Wry>);

/// Runtime-only widget state.
#[derive(Default)]
pub struct WidgetState {
    /// Where the collapsed ball sat when the panel opened. Restoring it on
    /// collapse is what keeps the ball from creeping up the screen.
    anchor: Mutex<Option<PhysicalPosition<i32>>>,
    /// Start of the current save-throttle window.
    saved_at: Mutex<Option<Instant>>,
    /// Whether the widget is on screen, for the pointer watcher to skip its
    /// work — and its trips to the main thread — while it is hidden.
    shown: AtomicBool,
    /// Whether the pointer was last seen over the widget, so only the comings
    /// and goings are reported rather than every look.
    hovered: AtomicBool,
    /// Whether the watcher thread has been started.
    watching: AtomicBool,
}

/// What survives a restart: whether the widget was on, and where the ball was.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Prefs {
    visible: bool,
    x: Option<i32>,
    y: Option<i32>,
}

fn prefs_path() -> PathBuf {
    platform::app_support_dir().join("floating.json")
}

/// Read the stored preferences; anything unreadable means "not shown yet".
fn load_prefs() -> Prefs {
    load_from(&prefs_path())
}

fn load_from(file: &Path) -> Prefs {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_prefs(prefs: Prefs) {
    save_to(&prefs_path(), prefs);
}

fn save_to(file: &Path, prefs: Prefs) {
    let Some(parent) = file.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }

    if let Ok(json) = serde_json::to_string_pretty(&prefs) {
        let _ = std::fs::write(file, json);
    }
}

/// The live widget window, when this build has one.
fn widget(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(WINDOW_LABEL)
}

/// Whether the widget is currently on screen.
pub fn is_visible(app: &AppHandle) -> bool {
    widget(app)
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
}

/// Tell every other surface — tray checkmark, sidebar toggle, the widget
/// itself — that the widget was shown or hidden.
fn announce(app: &AppHandle, visible: bool) {
    if let Some(item) = app.try_state::<TrayFloatingItem>() {
        let _ = item.0.set_checked(visible);
    }
    let _ = app.emit(VISIBILITY_EVENT, visible);
}

/// Whether a physical cursor position falls inside a window with the given
/// physical origin and size. Kept out of [`hover_tick`] so the arithmetic — the
/// half-open edges and the negative origins of a multi-monitor desktop — can be
/// checked without a window.
fn inside_window(
    origin: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    cursor: PhysicalPosition<f64>,
) -> bool {
    cursor.x >= f64::from(origin.x)
        && cursor.y >= f64::from(origin.y)
        && cursor.x < f64::from(origin.x + size.width as i32)
        && cursor.y < f64::from(origin.y + size.height as i32)
}

/// One look at the pointer: report it if it has come or gone since last time.
fn hover_tick(app: &AppHandle) {
    let Some(window) = widget(app) else {
        return;
    };

    let inside = window.is_visible().unwrap_or(false)
        && match (
            window.outer_position(),
            window.outer_size(),
            app.cursor_position(),
        ) {
            (Ok(origin), Ok(size), Ok(cursor)) => inside_window(origin, size, cursor),
            _ => false,
        };

    let Some(state) = app.try_state::<WidgetState>() else {
        return;
    };
    if state.hovered.swap(inside, Ordering::SeqCst) == inside {
        return;
    }

    let _ = app.emit_to(WINDOW_LABEL, HOVER_EVENT, inside);
}

/// Start watching the pointer, once per run.
///
/// AppKit state is only safe to touch on the main thread, so the loop runs
/// apart from it and hands each look over.
fn watch_hover(app: &AppHandle) {
    let Some(state) = app.try_state::<WidgetState>() else {
        return;
    };
    if state.watching.swap(true, Ordering::SeqCst) {
        return;
    }
    drop(state);

    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(HOVER_INTERVAL);

        let busy = app
            .try_state::<WidgetState>()
            .is_some_and(|state| state.shown.load(Ordering::Relaxed));
        if !busy {
            continue;
        }

        let tick = app.clone();
        let _ = app.run_on_main_thread(move || hover_tick(&tick));
    });
}

/// The ball's pixel size on the monitor the window currently sits on.
fn ball_size(window: &WebviewWindow) -> i32 {
    (BALL_SIZE * window.scale_factor().unwrap_or(1.0)).round() as i32
}

fn panel_width(window: &WebviewWindow) -> i32 {
    (PANEL_WIDTH * window.scale_factor().unwrap_or(1.0)).round() as i32
}

fn panel_height(window: &WebviewWindow) -> i32 {
    (PANEL_HEIGHT * window.scale_factor().unwrap_or(1.0)).round() as i32
}

/// Whether a stored spot still lands on a screen that exists — monitors get
/// unplugged, and a ball parked off-screen would be invisible for good.
///
/// Deliberately not `monitor_from_point`: that takes logical points, while what
/// is stored here is physical, so on a Retina display it would call a perfectly
/// good spot off-screen and send the ball to the corner instead.
fn on_screen(app: &AppHandle, x: i32, y: i32) -> bool {
    let Ok(monitors) = app.available_monitors() else {
        return false;
    };

    monitors.iter().any(|monitor| {
        let origin = monitor.position();
        let size = monitor.size();
        x >= origin.x
            && x < origin.x + size.width as i32
            && y >= origin.y
            && y < origin.y + size.height as i32
    })
}

/// The bottom-right corner of the window's monitor, used the first time.
fn corner_position(window: &WebviewWindow) -> PhysicalPosition<i32> {
    let ball = ball_size(window);
    let margin = (DEFAULT_MARGIN * window.scale_factor().unwrap_or(1.0)).round() as i32;

    window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
        .map(|monitor| {
            let origin = monitor.position();
            PhysicalPosition::new(
                origin.x + monitor.size().width as i32 - ball - margin,
                origin.y + monitor.size().height as i32 - ball - margin,
            )
        })
        .unwrap_or(PhysicalPosition::new(margin, margin))
}

/// Put the window back into its collapsed shape and park it at `position`.
///
/// Shared by the show and collapse paths, so there is only one place that knows
/// what "collapsed" means.
fn collapse_to(window: &WebviewWindow, position: PhysicalPosition<i32>) {
    let ball = ball_size(window);
    let _ = window.set_position(position);
    let _ = window.set_size(PhysicalSize::new(ball, ball));
}

/// Forget the panel: it is folded away whenever the widget goes into hiding,
/// and the ball's own corner is the position worth storing.
fn take_anchor(app: &AppHandle) -> Option<PhysicalPosition<i32>> {
    app.try_state::<WidgetState>()
        .and_then(|state| state.anchor.lock().ok().and_then(|mut anchor| anchor.take()))
}

/// Remember where the ball was left, for the next launch.
fn remember(position: PhysicalPosition<i32>) {
    let mut prefs = load_prefs();
    prefs.x = Some(position.x);
    prefs.y = Some(position.y);
    save_prefs(prefs);
}

/// Show the widget, at its stored spot when that still exists.
pub fn show(app: &AppHandle) -> Result<()> {
    let Some(window) = widget(app) else {
        return Err(WindleError::NotFound(WINDOW_LABEL.into()));
    };

    let mut prefs = load_prefs();
    let position = match (prefs.x, prefs.y) {
        (Some(x), Some(y)) if on_screen(app, x, y) => PhysicalPosition::new(x, y),
        _ => corner_position(&window),
    };

    collapse_to(&window, position);
    let _ = window.show();

    // The pointer may already be sitting on the spot: starting from "not over
    // it" is what makes the first look report its arrival.
    if let Some(state) = app.try_state::<WidgetState>() {
        state.hovered.store(false, Ordering::SeqCst);
        state.shown.store(true, Ordering::SeqCst);
    }
    watch_hover(app);

    prefs.visible = true;
    prefs.x = Some(position.x);
    prefs.y = Some(position.y);
    save_prefs(prefs);

    announce(app, true);
    Ok(())
}

/// Hide the widget, remembering the ball's spot for the next launch.
pub fn hide(app: &AppHandle) -> Result<()> {
    let Some(window) = widget(app) else {
        return Err(WindleError::NotFound(WINDOW_LABEL.into()));
    };

    // Hiding while the panel is open has to fold it back first: the next show
    // starts from the collapsed shape, and a frontend left in panel layout
    // would be clipped.
    let position = take_anchor(app)
        .or_else(|| window.outer_position().ok())
        .unwrap_or_default();
    collapse_to(&window, position);
    let _ = window.hide();

    if let Some(state) = app.try_state::<WidgetState>() {
        state.shown.store(false, Ordering::SeqCst);
        state.hovered.store(false, Ordering::SeqCst);
    }

    let mut prefs = load_prefs();
    prefs.visible = false;
    prefs.x = Some(position.x);
    prefs.y = Some(position.y);
    save_prefs(prefs);

    announce(app, false);
    Ok(())
}

/// Bring the widget back when the previous session left it on.
///
/// This has to happen here rather than from the widget's own frontend: a window
/// that is created hidden never loads its webview, so nothing on the other side
/// would ever ask for it.
pub fn restore(app: &AppHandle) {
    if load_prefs().visible {
        let _ = show(app);
    }
}

#[tauri::command]
pub fn show_floating_window(app: AppHandle) -> Result<()> {
    show(&app)
}

#[tauri::command]
pub fn hide_floating_window(app: AppHandle) -> Result<()> {
    hide(&app)
}

#[tauri::command]
pub fn toggle_floating_window(app: AppHandle) -> Result<bool> {
    if is_visible(&app) {
        hide(&app)?;
    } else {
        show(&app)?;
    }

    Ok(is_visible(&app))
}

#[tauri::command]
pub fn floating_window_visible(app: AppHandle) -> bool {
    is_visible(&app)
}

/// Save where the ball was left, once a drag has ended.
///
/// The `Moved` stream writes at most once a second, so the tail of a quick drag
/// — everything after that one write — is dropped, and the next launch would
/// park the ball back where the drag started. The frontend knows when the drag
/// is over, which makes this the one moment the resting place is certain.
#[tauri::command]
pub fn remember_floating_position(app: AppHandle) -> Result<()> {
    let Some(window) = widget(&app) else {
        return Err(WindleError::NotFound(WINDOW_LABEL.into()));
    };

    if !panel_open(&app) {
        if let Ok(position) = window.outer_position() {
            remember(position);
        }
    }

    Ok(())
}

/// Grow the window for the hover panel, or fold it back onto the ball.
///
/// `placement` decides which way the window grows vertically and `side` which
/// edge the ball keeps; between them the origin moves so the ball's own pixels
/// stay put. The frontend has already applied the matching layout by the time
/// this returns, which is why nothing flickers.
#[tauri::command]
pub fn set_floating_expanded(
    window: WebviewWindow,
    state: State<'_, WidgetState>,
    expanded: bool,
    placement: Placement,
    side: Side,
) -> Result<()> {
    let Ok(mut anchor) = state.anchor.lock() else {
        return Ok(());
    };

    if expanded {
        if anchor.is_none() {
            let Ok(from) = window.outer_position() else {
                return Ok(());
            };
            *anchor = Some(from);

            // The panel is wider than the ball, so growing the window can push
            // the ball sideways unless the origin follows.
            let sideways = match side {
                Side::Left => 0,
                Side::Right => panel_width(&window) - ball_size(&window),
            };
            let upward = match placement {
                Placement::Above => panel_height(&window),
                Placement::Below => 0,
            };
            let _ = window.set_position(PhysicalPosition::new(
                from.x - sideways,
                from.y - upward,
            ));
        }
        let ball = ball_size(&window);
        let _ = window.set_size(PhysicalSize::new(
            panel_width(&window),
            ball + panel_height(&window),
        ));
    } else {
        let from = anchor.take().or_else(|| window.outer_position().ok());
        collapse_to(&window, from.unwrap_or_default());
    }

    Ok(())
}

/// Whether the window is currently grown for the hover panel. Its origin is
/// then the panel's corner, not the ball's, so it is not a place to park the
/// ball at.
fn panel_open(app: &AppHandle) -> bool {
    app.try_state::<WidgetState>()
        .and_then(|state| state.anchor.lock().ok().map(|anchor| anchor.is_some()))
        .unwrap_or(true)
}

/// Remember a new resting place for the ball.
///
/// Called from the `Moved` window event, which fires for every frame of a drag
/// and also for the moves the widget makes on its own while the panel is open
/// — those are not the ball's position, so they are skipped. Writes are
/// throttled, which is why the end of a drag is saved on its own account by
/// `remember_floating_position`.
pub fn remember_position(app: &AppHandle, position: PhysicalPosition<i32>) {
    let Some(state) = app.try_state::<WidgetState>() else {
        return;
    };

    if panel_open(app) {
        return;
    }

    {
        let Ok(mut saved_at) = state.saved_at.lock() else {
            return;
        };
        if saved_at.is_some_and(|at| at.elapsed() < SAVE_INTERVAL) {
            return;
        }
        *saved_at = Some(Instant::now());
    }

    remember(position);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pointer_counts_as_inside_up_to_the_far_edge() {
        let origin = PhysicalPosition::new(2758, 1678);
        let size = PhysicalSize::new(66, 66);

        assert!(inside_window(origin, size, PhysicalPosition::new(2758.0, 1678.0)));
        assert!(inside_window(origin, size, PhysicalPosition::new(2823.5, 1743.5)));

        // The far edges are outside: the widget must not act as if it were a
        // pixel wider than it is.
        assert!(!inside_window(origin, size, PhysicalPosition::new(2824.0, 1700.0)));
        assert!(!inside_window(origin, size, PhysicalPosition::new(2800.0, 1744.0)));
        assert!(!inside_window(origin, size, PhysicalPosition::new(2757.0, 1700.0)));
        assert!(!inside_window(origin, size, PhysicalPosition::new(2800.0, 1677.0)));
    }

    #[test]
    fn a_monitor_left_of_the_main_one_reports_negative_origins() {
        let origin = PhysicalPosition::new(-1920, -300);
        let size = PhysicalSize::new(66, 66);

        assert!(inside_window(origin, size, PhysicalPosition::new(-1900.0, -280.0)));
        assert!(!inside_window(origin, size, PhysicalPosition::new(-2000.0, -280.0)));
        assert!(!inside_window(origin, size, PhysicalPosition::new(-1900.0, -400.0)));
    }

    /// A private preferences file per test, so parallel runs cannot collide and
    /// the user's real widget position is never touched.
    fn scratch(name: &str) -> PathBuf {
        let file = crate::utils::test_support::scratch_base().join(format!(
            "windle-floating-{name}-{}.json",
            std::process::id()
        ));
        std::fs::remove_file(&file).ok();
        file
    }

    #[test]
    fn preferences_round_trip_through_the_file() {
        let file = scratch("round-trip");

        save_to(
            &file,
            Prefs {
                visible: true,
                x: Some(-1200),
                y: Some(24),
            },
        );
        let prefs = load_from(&file);

        assert!(prefs.visible);
        assert_eq!(prefs.x, Some(-1200));
        assert_eq!(prefs.y, Some(24));

        // A later write replaces the file rather than appending to it.
        save_to(
            &file,
            Prefs {
                visible: false,
                x: Some(10),
                y: Some(10),
            },
        );
        assert!(!load_from(&file).visible);

        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn a_missing_file_reads_as_hidden() {
        let prefs = load_from(&scratch("missing"));

        assert!(!prefs.visible);
        assert_eq!((prefs.x, prefs.y), (None, None));
    }

    #[test]
    fn a_corrupt_file_reads_as_hidden() {
        let file = scratch("corrupt");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{ not json").unwrap();

        assert!(!load_from(&file).visible);

        // The next write repairs it rather than failing.
        save_to(&file, Prefs {
            visible: true,
            x: None,
            y: None,
        });
        assert!(load_from(&file).visible);

        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn a_file_from_an_older_version_still_loads() {
        // Only `visible` was stored back when the ball always opened in the
        // corner; unknown fields must be ignored too.
        let prefs: Prefs = serde_json::from_str(r#"{"visible":true,"extra":1}"#).unwrap();

        assert!(prefs.visible);
        assert_eq!((prefs.x, prefs.y), (None, None));
    }

    #[test]
    fn the_stored_shape_is_camel_case() {
        let json = serde_json::to_string(&Prefs {
            visible: true,
            x: Some(1),
            y: Some(2),
        })
        .unwrap();

        assert_eq!(json, r#"{"visible":true,"x":1,"y":2}"#);
    }

    #[test]
    fn placement_is_lower_case_on_the_wire() {
        assert_eq!(serde_json::to_string(&Placement::Above).unwrap(), r#""above""#);
        assert_eq!(serde_json::to_string(&Placement::Below).unwrap(), r#""below""#);
        assert_eq!(
            serde_json::from_str::<Placement>(r#""above""#).unwrap(),
            Placement::Above
        );
    }
}
