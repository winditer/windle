pub mod commands;
pub mod floating;
pub mod scanner;
pub mod utils;

use tauri::{
    Manager,
    menu::{CheckMenuItem, Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    WindowEvent,
};

/// When the tray popup last hid itself because it lost focus. A click on the
/// tray icon deactivates the popup before the click is delivered on Windows,
/// so the toggle below has to tell that auto-hide apart from a user close.
static POPUP_HIDDEN_AT: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);

/// Label of the tray menu's "hide the icon, keep the app" item.
#[cfg(target_os = "macos")]
const TRAY_QUIT_LABEL: &str = "退出状态栏";
#[cfg(not(target_os = "macos"))]
const TRAY_QUIT_LABEL: &str = "退出托盘";

/// The tray menu entry that shows and hides the desktop widget.
const TRAY_FLOATING_LABEL: &str = "桌面悬窗";

/// Menu bar / status bar commands. Kept in a dedicated module so the
/// `#[tauri::command]` helper macros do not clash with `generate_handler!`,
/// which lives in the same crate-root module.
mod menubar {
    use tauri::Manager;

    /// Quit the status bar (tray icon) without quitting the main app.
    /// If the main window is not visible, exit the app entirely.
    #[tauri::command]
    pub fn quit_status_bar(app: tauri::AppHandle) {
        if let Some(tray) = app.tray_by_id("windle-tray") {
            let _ = tray.set_visible(false);
        }
        if let Some(window) = app.get_webview_window("menubar") {
            let _ = window.hide();
        }
        // If main window is not visible, exit the app
        let main_visible = app
            .get_webview_window("main")
            .map(|w| w.is_visible().unwrap_or(false))
            .unwrap_or(false);
        if !main_visible {
            app.exit(0);
        }
    }

    /// Show the main window and hide the popup.
    #[tauri::command]
    pub fn show_main_window(app: tauri::AppHandle) {
        // Restore the app to the Dock (normal behavior) before showing.
        #[cfg(target_os = "macos")]
        {
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
        }
        if let Some(window) = app.get_webview_window("main") {
            // A window hidden while minimized stays minimized through `show`
            // on Windows, so it is restored first.
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
        if let Some(window) = app.get_webview_window("menubar") {
            let _ = window.hide();
        }
    }
}

/// Build and run the Tauri application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();

    // Launching Windle again must not start a second copy. macOS routes that
    // through `RunEvent::Reopen` further down, so the plugin covers the other
    // platforms; it has to be registered before any other plugin to get the
    // chance to stop the second instance early.
    #[cfg(not(target_os = "macos"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }));

    let app = builder
        .plugin(tauri_plugin_shell::init())
        .manage(commands::clean::CleanState::default())
        .manage(commands::monitor::MonitorState::default())
        .manage(floating::WidgetState::default())
        .setup(|app| {
            // Build the right-click context menu. Its labels are the one piece
            // of UI the frontend cannot localize — it is built here, once — so
            // they follow the platform's own wording instead: the icon sits in
            // the menu bar on macOS and in the notification area on Windows.
            let open_item = MenuItem::with_id(app, "open", "打开 Windle", true, None::<&str>)?;
            let floating_item = CheckMenuItem::with_id(
                app,
                "floating",
                TRAY_FLOATING_LABEL,
                true,
                false,
                None::<&str>,
            )?;
            let quit_tray_item =
                MenuItem::with_id(app, "quit_tray", TRAY_QUIT_LABEL, true, None::<&str>)?;
            let quit_app_item =
                MenuItem::with_id(app, "quit_app", "退出 Windle", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[&open_item, &floating_item, &quit_tray_item, &quit_app_item],
            )?;

            // The checkmark has to follow the widget wherever it is toggled
            // from, so the item handle is kept in managed state.
            app.manage(floating::TrayFloatingItem(floating_item));

            // macOS takes a template image (monochrome black on transparent)
            // and adapts it to light and dark menu bars automatically. Windows
            // has no such notion — it would simply draw the black shape — so
            // the coloured application icon is used there instead.
            #[cfg(target_os = "macos")]
            let tray_icon_bytes = include_bytes!("../icons/tray-icon.png");
            #[cfg(not(target_os = "macos"))]
            let tray_icon_bytes = include_bytes!("../icons/32x32.png");

            let tray_icon = tauri::image::Image::from_bytes(tray_icon_bytes)?;
            let _tray = TrayIconBuilder::with_id("windle-tray")
                .icon(tray_icon)
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip("Windle")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| {
                    match event.id().as_ref() {
                        "open" => {
                            #[cfg(target_os = "macos")]
                            {
                                let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
                            }
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.unminimize();
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        "floating" => {
                            let _ = floating::toggle_floating_window(app.clone());
                        }
                        "quit_tray" => {
                            if let Some(tray) = app.tray_by_id("windle-tray") {
                                let _ = tray.set_visible(false);
                            }
                            if let Some(window) = app.get_webview_window("menubar") {
                                let _ = window.hide();
                            }
                            // The tray icon was the last thing holding the app
                            // up: with the main window closed too, quit.
                            let main_visible = app
                                .get_webview_window("main")
                                .map(|w| w.is_visible().unwrap_or(false))
                                .unwrap_or(false);
                            if !main_visible {
                                app.exit(0);
                            }
                        }
                        "quit_app" => {
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        rect,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("menubar") {
                            let is_visible = window.is_visible().unwrap_or(false);
                            // A recent auto-hide means the click that hid the
                            // popup was this very click on the tray icon, so
                            // it asked to close rather than reopen.
                            let just_hid = POPUP_HIDDEN_AT
                                .lock()
                                .ok()
                                .and_then(|at| *at)
                                .is_some_and(|at| {
                                    at.elapsed() < std::time::Duration::from_millis(250)
                                });
                            if is_visible || just_hid {
                                let _ = window.hide();
                            } else {
                                // Position the popup next to the tray icon.
                                // rect.position is in physical coordinates.
                                let (pos_x, pos_y, size_w, size_h) = match (&rect.position, &rect.size) {
                                    (tauri::Position::Physical(pos), tauri::Size::Physical(sz)) => {
                                        (pos.x as f64, pos.y as f64, sz.width as f64, sz.height as f64)
                                    }
                                    (tauri::Position::Logical(pos), tauri::Size::Logical(sz)) => {
                                        (pos.x, pos.y, sz.width, sz.height)
                                    }
                                    _ => (0.0, 0.0, 0.0, 0.0),
                                };
                                let popup_width = 340.0;
                                let popup_height = window
                                    .outer_size()
                                    .map(|size| size.height as f64)
                                    .unwrap_or(480.0);

                                // Which side of the icon has room depends on
                                // where the bar sits: macOS keeps its menu bar
                                // along the top, Windows usually has the
                                // taskbar at the bottom, and either can be
                                // moved. Comparing the icon with the middle of
                                // the monitor it sits on picks the direction;
                                // the icon's own point is the only reliable
                                // hint, since another monitor may be a
                                // different height.
                                let (center_x, center_y) =
                                    (pos_x + size_w / 2.0, pos_y + size_h / 2.0);
                                let opens_below = app
                                    .monitor_from_point(center_x, center_y)
                                    .ok()
                                    .flatten()
                                    .map(|monitor| {
                                        let middle = monitor.position().y as f64
                                            + monitor.size().height as f64 / 2.0;
                                        center_y < middle
                                    })
                                    .unwrap_or(true);

                                let x = (center_x - popup_width / 2.0).max(0.0);
                                let y = if opens_below {
                                    pos_y + size_h + 4.0
                                } else {
                                    (pos_y - popup_height - 4.0).max(0.0)
                                };

                                use tauri::PhysicalPosition;
                                let _ = window
                                    .set_position(PhysicalPosition::new(x as i32, y as i32));
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            floating::restore(app.handle());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Dashboard / shared
            commands::get_dashboard_summary,
            commands::check_permissions,
            commands::open_full_disk_access_settings,
            commands::platform_info,
            commands::reveal_in_finder,
            // Deep Clean
            commands::clean::scan_junk,
            commands::clean::clean_paths,
            commands::clean::empty_trash,
            commands::clean::cancel_clean_scan,
            // Docker Cleanup
            commands::docker::docker_status,
            commands::docker::docker_start_desktop,
            commands::docker::docker_scan,
            commands::docker::docker_clean,
            // Smart Uninstall
            commands::uninstall::list_apps,
            commands::uninstall::build_uninstall_plan,
            commands::uninstall::uninstall_app,
            commands::uninstall::find_orphaned_leftovers,
            // Disk Analyzer
            commands::analyze::list_volumes,
            commands::analyze::analyze_path,
            commands::analyze::expand_node,
            commands::analyze::find_largest_files,
            // System Optimize
            commands::optimize::list_optimize_tasks,
            commands::optimize::run_optimize_task,
            commands::optimize::run_optimize_tasks,
            commands::optimize::list_login_items,
            commands::optimize::set_login_item_enabled,
            commands::optimize::clear_optimize_auth,
            // Live Monitor
            commands::monitor::get_snapshot,
            commands::monitor::get_widget_snapshot,
            commands::monitor::start_monitor,
            commands::monitor::stop_monitor,
            commands::monitor::list_processes,
            commands::monitor::list_top_memory_processes,
            commands::monitor::kill_process,
            // Project Purge
            commands::purge::scan_projects,
            commands::purge::purge_artifacts,
            // Installer Cleanup
            commands::installer::scan_installers,
            commands::installer::remove_installers,
            commands::installer::detach_mounted_images,
            // AI Agent Cleanup
            commands::agent::scan_ai_agents,
            commands::agent::remove_ai_data,
            // Menu bar / status bar
            menubar::quit_status_bar,
            menubar::show_main_window,
            // Desktop floating widget
            floating::show_floating_window,
            floating::hide_floating_window,
            floating::toggle_floating_window,
            floating::floating_window_visible,
            floating::remember_floating_position,
            floating::set_floating_expanded,
        ])
        .on_window_event(|window, event| {
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    let label = window.label();
                    if label == "main" {
                        // Hide instead of close — keeps the app alive with the tray.
                        let _ = window.hide();
                        api.prevent_close();
                        // Hide from Dock when the main window is closed.
                        #[cfg(target_os = "macos")]
                        {
                            let _ = window.app_handle().set_activation_policy(
                                tauri::ActivationPolicy::Accessory,
                            );
                        }
                    } else if label == "menubar" {
                        let _ = window.hide();
                        api.prevent_close();
                    } else if label == floating::WINDOW_LABEL {
                        // Closing is hiding: the tray item and the sidebar
                        // toggle are the way back.
                        let _ = floating::hide(window.app_handle());
                        api.prevent_close();
                    }
                }
                WindowEvent::Moved(position) => {
                    if window.label() == floating::WINDOW_LABEL {
                        floating::remember_position(window.app_handle(), *position);
                    }
                }
                WindowEvent::Focused(false) => {
                    let label = window.label();
                    // Auto-hide the popup when it loses focus (macOS standard).
                    if label == "menubar" {
                        if let Ok(mut at) = POPUP_HIDDEN_AT.lock() {
                            *at = Some(std::time::Instant::now());
                        }
                        let _ = window.hide();
                    } else if label == "main"
                        && window.is_minimized().unwrap_or(false)
                    {
                        // User minimized the main window — hide it and hide from Dock.
                        let _ = window.hide();
                        #[cfg(target_os = "macos")]
                        {
                            let _ = window.app_handle().set_activation_policy(
                                tauri::ActivationPolicy::Accessory,
                            );
                        }
                    }
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building the Windle application");

    // Lifecycle events need the callback form of `run` — the plain
    // `.run(context)` variant ignores them (which is why re-launching the
    // app from Finder / Launchpad / Applications did nothing while the
    // app was dock-less in the status bar).
    app.run(|app_handle, event| {
        // macOS delivers `Reopen` instead of starting a second process when the
        // app is launched again while already running: restore the Dock icon
        // and bring the main window back up. Elsewhere the single-instance
        // plugin registered above already handles that.
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            let _ = app_handle.set_activation_policy(tauri::ActivationPolicy::Regular);
            if let Some(window) = app_handle.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }

        #[cfg(not(target_os = "macos"))]
        let _ = (app_handle, event);
    });
}
