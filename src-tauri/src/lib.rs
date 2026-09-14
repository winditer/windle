pub mod commands;
pub mod scanner;
pub mod utils;

use tauri::{
    Manager,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    menu::{Menu, MenuItem},
    WindowEvent,
};

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
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(commands::clean::CleanState::default())
        .manage(commands::monitor::MonitorState::default())
        .setup(|app| {
            // Build the right-click context menu.
            let open_item = MenuItem::with_id(app, "open", "打开 Windle", true, None::<&str>)?;
            let quit_tray_item =
                MenuItem::with_id(app, "quit_tray", "退出状态栏", true, None::<&str>)?;
            let quit_app_item =
                MenuItem::with_id(app, "quit_app", "退出 Windle", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_item, &quit_tray_item, &quit_app_item])?;

            // Build the tray icon using a template image (monochrome black on
            // transparent) so macOS automatically adapts to light/dark mode.
            let tray_icon_bytes = include_bytes!("../icons/tray-icon.png");
            let tray_icon = tauri::image::Image::from_bytes(tray_icon_bytes)?;
            let _tray = TrayIconBuilder::with_id("windle-tray")
                .icon(tray_icon)
                .icon_as_template(true)
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
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        "quit_tray" => {
                            if let Some(tray) = app.tray_by_id("windle-tray") {
                                let _ = tray.set_visible(false);
                            }
                            if let Some(window) = app.get_webview_window("menubar") {
                                let _ = window.hide();
                            }
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
                            if is_visible {
                                let _ = window.hide();
                            } else {
                                // Position the popup below the tray icon.
                                // rect.position is in physical coordinates.
                                // On macOS, menu bar is at the top, so popup goes below it.
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
                                let x = pos_x + size_w / 2.0 - popup_width / 2.0;
                                let y = pos_y + size_h + 4.0;
                                // Clamp x to keep popup on screen.
                                let x = if x < 0.0 { 0.0 } else { x };
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

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Dashboard / shared
            commands::get_dashboard_summary,
            commands::check_permissions,
            commands::open_full_disk_access_settings,
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
                    }
                }
                WindowEvent::Focused(false) => {
                    let label = window.label();
                    // Auto-hide the popup when it loses focus (macOS standard).
                    if label == "menubar" {
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
        if let tauri::RunEvent::Reopen { .. } = event {
            // macOS delivers `Reopen` instead of starting a second process
            // when the app is launched again while already running. Restore
            // the Dock icon and bring the main window back up.
            #[cfg(target_os = "macos")]
            {
                let _ =
                    app_handle.set_activation_policy(tauri::ActivationPolicy::Regular);
            }
            if let Some(window) = app_handle.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
    });
}
