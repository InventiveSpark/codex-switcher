use tauri::{
    image::Image,
    tray::{TrayIcon, TrayIconBuilder, TrayIconEvent},
    webview::WebviewWindowBuilder,
    AppHandle, Manager,
};

/// Initialize system tray
pub fn init(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    // Load and scale icon
    let icon_bytes = include_bytes!("../icons/app-icon-squircle.png");
    let base_img =
        image::load_from_memory(icon_bytes).map_err(|e| format!("Failed to load icon: {}", e))?;

    let target_size = 128;
    let content_size = 105;
    let padding = (target_size - content_size) / 2;

    let scaled_content = base_img.resize(
        content_size,
        content_size,
        image::imageops::FilterType::Lanczos3,
    );
    let mut final_img = image::RgbaImage::new(target_size, target_size);

    image::imageops::overlay(
        &mut final_img,
        &scaled_content,
        padding as i64,
        padding as i64,
    );

    let (width, height) = final_img.dimensions();
    let icon = Image::new_owned(final_img.into_raw(), width, height);

    let _tray = TrayIconBuilder::with_id("main")
        .icon(icon)
        .icon_as_template(false)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray: &TrayIcon, event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button_state: tauri::tray::MouseButtonState::Up,
                position,
                ..
            } = event
            {
                // Any click → show popup
                toggle_popup(tray.app_handle(), position);
            }
        })
        .build(app)?;

    println!("[Tray] System tray started");
    Ok(())
}

/// Show/hide tray popup window
fn toggle_popup(app: &AppHandle, position: tauri::PhysicalPosition<f64>) {
    let label = "tray-popup";

    // If exists, toggle show/hide
    if let Some(win) = app.get_webview_window(label) {
        if win.is_visible().unwrap_or(false) {
            let _ = win.hide();
            return;
        }
        // Reposition and show
        let _ = position_popup(&win, position);
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }

    // First time creation
    let popup_width = 380.0;
    let popup_height = 410.0;

    let url = tauri::WebviewUrl::App("index.html".into());

    match WebviewWindowBuilder::new(app, label, url)
        .title("Codex Switcher")
        .inner_size(popup_width, popup_height)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .build()
    {
        Ok(win) => {
            // Listen for focus loss → auto hide
            let win_clone = win.clone();
            win.on_window_event(move |event| {
                if let tauri::WindowEvent::Focused(false) = event {
                    let _ = win_clone.hide();
                }
            });

            let _ = position_popup(&win, position);
            let _ = win.show();
            let _ = win.set_focus();
        }
        Err(e) => eprintln!("[Tray] Failed to create popup window: {}", e),
    }
}

/// Position popup window near tray icon (below macOS top menu bar)
fn position_popup(
    win: &tauri::WebviewWindow,
    tray_pos: tauri::PhysicalPosition<f64>,
) -> Result<(), String> {
    let popup_width = 380.0;

    let scale = win.scale_factor().unwrap_or(1.0);

    let x = (tray_pos.x - popup_width * scale / 2.0).max(0.0) as i32;
    let y = (tray_pos.y + 4.0) as i32; // Leave some spacing for menu bar

    let _ = win.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(
        x, y,
    )));
    Ok(())
}

pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        #[cfg(target_os = "macos")]
        app.set_activation_policy(tauri::ActivationPolicy::Regular)
            .unwrap_or(());
    }
}

/// Entry point for Tauri command invocation
pub fn show_main_window_from_cmd(app: &AppHandle) {
    show_main_window(app);
    // Also hide popup
    if let Some(popup) = app.get_webview_window("tray-popup") {
        let _ = popup.hide();
    }
}

/// Update tray tooltip (no longer needs full menu)
pub fn update_tray_menu(app: &AppHandle) {
    let state = app.state::<crate::AppState>();
    let store = match state.store.lock() {
        Ok(s) => s,
        Err(_) => return,
    };

    let tooltip = if let Some(current_id) = &store.current {
        if let Some(acc) = store.accounts.get(current_id) {
            let quota = acc
                .cached_quota
                .as_ref()
                .map(|q| format!(" | 5H: {:.0}%  Wk: {:.0}%", q.five_hour_left, q.weekly_left))
                .unwrap_or_default();
            format!("Codex Switcher - {}{}", acc.name, quota)
        } else {
            "Codex Switcher".to_string()
        }
    } else {
        "Codex Switcher - Not signed in".to_string()
    };

    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(&tooltip));
    }
}
