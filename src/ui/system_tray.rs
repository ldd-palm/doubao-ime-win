//! System Tray
//!
//! Implements the system tray icon and menu with proper Windows message loop.

use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
};

use crate::business::{HotkeyManager, VoiceController};
use crate::data::AppConfig;
use crate::ui::{
    ButtonState, FloatingButton, FloatingButtonConfig, FloatingButtonEvent,
    FloatingButtonStateSetter,
};

/// Run the application with system tray and floating button
pub async fn run_app(
    config: AppConfig,
    voice_controller: Arc<Mutex<VoiceController>>,
    hotkey_manager: HotkeyManager,
) -> Result<()> {
    // Create floating button
    let mut floating_button = FloatingButton::new();
    let button_state_setter = floating_button.state_setter();
    let floating_rx = floating_button.take_event_receiver();

    // If a recording session dies on its own (dropped connection, reconnect
    // attempts exhausted) rather than via an explicit stop, make sure the
    // floating button doesn't keep showing "recording" for a session that's
    // actually dead.
    {
        let setter = button_state_setter.clone();
        let mut controller = voice_controller.lock().await;
        controller.set_stopped_unexpectedly_callback(move || {
            tracing::warn!("Recording stopped unexpectedly; syncing UI back to idle");
            setter.set_state(ButtonState::Idle);
        });
    }

    // Configure floating button position from config
    let fb_config = FloatingButtonConfig {
        initial_x: config.floating_button.position_x,
        initial_y: config.floating_button.position_y,
        size: 56,
    };

    // Spawn floating button thread if enabled
    if config.floating_button.enabled {
        std::thread::spawn(move || {
            floating_button.run(fb_config);
        });
    }

    // Create tray icon on main thread
    let icon = load_icon(false)?;
    let menu = Menu::new();

    let start_item = MenuItem::new("Start Voice Input", true, None);
    let stop_item = MenuItem::new("Stop Voice Input", true, None);
    let separator1 = PredefinedMenuItem::separator();
    let service_item = MenuItem::new("Service: Pause", true, None);
    let separator2 = PredefinedMenuItem::separator();
    let settings_item = MenuItem::new("Settings...", true, None);
    let separator3 = PredefinedMenuItem::separator();
    let quit_item = MenuItem::new("Exit", true, None);

    let start_id = start_item.id().clone();
    let stop_id = stop_item.id().clone();
    let service_id = service_item.id().clone();
    let settings_id = settings_item.id().clone();
    let quit_id = quit_item.id().clone();

    menu.append(&start_item)?;
    menu.append(&stop_item)?;
    menu.append(&separator1)?;
    menu.append(&service_item)?;
    menu.append(&separator2)?;
    menu.append(&settings_item)?;
    menu.append(&separator3)?;
    menu.append(&quit_item)?;

    let tray_icon_handle = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Doubao Voice Input")
        .with_icon(icon)
        .build()?;

    tracing::info!("System tray initialized");

    // Running flag
    let running = Arc::new(AtomicBool::new(true));

    // Whether the whole service (hotkey + floating button) is paused.
    let service_paused = Arc::new(AtomicBool::new(false));
    // Set by the event thread when the tray icon/menu text needs a refresh;
    // consumed on the main thread, which is the only thread allowed to touch
    // `tray_icon_handle` / `service_item` (both are !Send GUI handles).
    let tray_dirty = Arc::new(AtomicBool::new(false));
    // Shared flag that gates hotkey triggering; owned by HotkeyManager but
    // exposed so pausing the service can disable it without sharing
    // HotkeyManager itself across threads.
    let hotkey_active = hotkey_manager.active_handle();

    // Get menu and floating button receivers
    let menu_rx = MenuEvent::receiver();
    let tray_rx = TrayIconEvent::receiver();

    // Get tokio runtime handle for async operations
    let runtime_handle = tokio::runtime::Handle::current();

    // Set up hotkey callbacks with state sync: a normal tap toggles
    // recording, holding the trigger key and tapping the pause-combo key
    // (e.g. RAlt + Space) pauses/resumes the whole service.
    let vc_for_hotkey = voice_controller.clone();
    let state_for_hotkey = button_state_setter.clone();
    let handle_for_hotkey = runtime_handle.clone();
    let vc_for_pause_combo = voice_controller.clone();
    let state_for_pause_combo = button_state_setter.clone();
    let handle_for_pause_combo = runtime_handle.clone();
    let hotkey_active_for_pause_combo = hotkey_active.clone();
    let service_paused_for_pause_combo = service_paused.clone();
    let tray_dirty_for_pause_combo = tray_dirty.clone();
    hotkey_manager.on_trigger(
        move || {
            let vc = vc_for_hotkey.clone();
            let setter = state_for_hotkey.clone();
            let handle = handle_for_hotkey.clone();
            handle.spawn(async move {
                let mut controller = vc.lock().await;
                if controller.is_recording() {
                    tracing::info!("Hotkey: stopping voice input");
                    setter.set_state(ButtonState::Processing);
                    if let Err(e) = controller.stop().await {
                        tracing::error!("Failed to stop voice input: {}", e);
                    }
                    setter.set_state(ButtonState::Idle);
                } else {
                    tracing::info!("Hotkey: starting voice input");
                    // Immediate feedback: the ASR handshake below is a real
                    // network round trip (can take ~1-3s), so show "processing"
                    // right away instead of leaving the button looking unresponsive.
                    setter.set_state(ButtonState::Processing);
                    if let Err(e) = controller.start().await {
                        tracing::error!("Failed to start voice input: {}", e);
                        setter.set_state(ButtonState::Idle);
                    } else {
                        setter.set_state(ButtonState::Recording);
                    }
                }
            });
        },
        move || {
            tracing::info!("Hotkey: pause-combo detected, toggling service pause");
            handle_for_pause_combo.spawn(toggle_service_pause(
                vc_for_pause_combo.clone(),
                state_for_pause_combo.clone(),
                hotkey_active_for_pause_combo.clone(),
                service_paused_for_pause_combo.clone(),
                tray_dirty_for_pause_combo.clone(),
            ));
        },
    );
    // Keep the manager (and its internal hook thread) alive for the app's lifetime.
    let _hotkey_manager = hotkey_manager;

    // Spawn event handler thread for menu, tray icon, and floating button events
    let running_clone = running.clone();
    let vc_clone = voice_controller.clone();
    let state_setter_clone = button_state_setter.clone();
    let service_paused_clone = service_paused.clone();
    let tray_dirty_clone = tray_dirty.clone();
    let hotkey_active_clone = hotkey_active.clone();

    std::thread::spawn(move || {
        // Toggle the paused/resumed state of the whole service: stops any
        // active recording, disables the hotkey, and hides the floating
        // button. Reusable from the tray icon's left-click, the
        // "Service: Pause/Resume" menu item, and (separately, see the
        // `hotkey_manager.on_trigger` call above) the hotkey's pause-combo.
        let request_pause_toggle = {
            let vc = vc_clone.clone();
            let setter = state_setter_clone.clone();
            let service_paused = service_paused_clone.clone();
            let tray_dirty = tray_dirty_clone.clone();
            let hotkey_active = hotkey_active_clone.clone();
            let runtime_handle = runtime_handle.clone();
            move || {
                runtime_handle.spawn(toggle_service_pause(
                    vc.clone(),
                    setter.clone(),
                    hotkey_active.clone(),
                    service_paused.clone(),
                    tray_dirty.clone(),
                ));
            }
        };

        while running_clone.load(Ordering::SeqCst) {
            // Check menu events
            if let Ok(event) = menu_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                if event.id == start_id {
                    if service_paused_clone.load(Ordering::SeqCst) {
                        tracing::info!("Ignoring start request: service is paused");
                    } else {
                        let vc = vc_clone.clone();
                        let setter = state_setter_clone.clone();
                        runtime_handle.spawn(async move {
                            let mut controller = vc.lock().await;
                            if !controller.is_recording() {
                                tracing::info!("Starting from menu");
                                setter.set_state(ButtonState::Processing);
                                if let Err(e) = controller.start().await {
                                    tracing::error!("Failed to start: {}", e);
                                    setter.set_state(ButtonState::Idle);
                                } else {
                                    setter.set_state(ButtonState::Recording);
                                }
                            }
                        });
                    }
                } else if event.id == stop_id {
                    let vc = vc_clone.clone();
                    let setter = state_setter_clone.clone();
                    runtime_handle.spawn(async move {
                        let mut controller = vc.lock().await;
                        if controller.is_recording() {
                            tracing::info!("Stopping from menu");
                            setter.set_state(ButtonState::Processing);
                            if let Err(e) = controller.stop().await {
                                tracing::error!("Failed to stop: {}", e);
                            }
                            setter.set_state(ButtonState::Idle);
                        }
                    });
                } else if event.id == service_id {
                    request_pause_toggle();
                } else if event.id == settings_id {
                    tracing::info!("Settings from menu");
                    #[cfg(target_os = "windows")]
                    {
                        use windows::core::w;
                        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_OK, MB_ICONINFORMATION};
                        unsafe {
                            MessageBoxW(
                                None,
                                w!("Doubao Voice Input Settings\n\nHotkey: see config.toml [hotkey]\nFloating button: click to toggle recording\nTray icon: left-click to pause/resume the service\n\nConfig file: config.toml"),
                                w!("Settings"),
                                MB_OK | MB_ICONINFORMATION,
                            );
                        }
                    }
                } else if event.id == quit_id {
                    tracing::info!("Quit from menu");
                    running_clone.store(false, Ordering::SeqCst);
                    #[cfg(target_os = "windows")]
                    unsafe {
                        windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
                    }
                }
            }

            // Check tray icon events (left-click pauses/resumes the service)
            if let Ok(event) = tray_rx.try_recv() {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    request_pause_toggle();
                }
            }

            // Check floating button events
            if let Some(ref rx) = floating_rx {
                if let Ok(event) = rx.try_recv() {
                    match event {
                        FloatingButtonEvent::ToggleRecording => {
                            let vc = vc_clone.clone();
                            let setter = state_setter_clone.clone();
                            runtime_handle.spawn(async move {
                                let mut controller = vc.lock().await;
                                if controller.is_recording() {
                                    tracing::info!("Toggle: stopping");
                                    setter.set_state(ButtonState::Processing);
                                    if let Err(e) = controller.stop().await {
                                        tracing::error!("Failed to stop: {}", e);
                                    }
                                    setter.set_state(ButtonState::Idle);
                                } else {
                                    tracing::info!("Toggle: starting");
                                    setter.set_state(ButtonState::Processing);
                                    if let Err(e) = controller.start().await {
                                        tracing::error!("Failed to start: {}", e);
                                        setter.set_state(ButtonState::Idle);
                                    } else {
                                        setter.set_state(ButtonState::Recording);
                                    }
                                }
                            });
                        }
                        FloatingButtonEvent::Exit => {
                            tracing::info!("Exit from floating button");
                            running_clone.store(false, Ordering::SeqCst);
                            #[cfg(target_os = "windows")]
                            unsafe {
                                windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
                            }
                        }
                    }
                }
            }
        }
    });

    // Run Win32 message loop on main thread (REQUIRED for tray icon to work,
    // and for touching `tray_icon_handle` / `service_item`, which are !Send).
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, GetMessageW, KillTimer, SetTimer, TranslateMessage, MSG,
        };

        tracing::info!("Running Win32 message loop on main thread");

        // Thread-bound timer (no window) so the loop wakes up periodically
        // even when no real input message arrives, to pick up pause/resume
        // requests from the event thread promptly.
        let refresh_timer_id = unsafe { SetTimer(None, 0, 150, None) };

        let mut msg = MSG::default();
        unsafe {
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);

                if tray_dirty.swap(false, Ordering::SeqCst) {
                    let paused = service_paused.load(Ordering::SeqCst);
                    if let Ok(new_icon) = load_icon(paused) {
                        let _ = tray_icon_handle.set_icon(Some(new_icon));
                    }
                    let _ = tray_icon_handle.set_tooltip(Some(if paused {
                        "Doubao Voice Input - Paused"
                    } else {
                        "Doubao Voice Input"
                    }));
                    service_item.set_text(if paused {
                        "Service: Resume"
                    } else {
                        "Service: Pause"
                    });
                }

                if !running.load(Ordering::SeqCst) {
                    break;
                }
            }
        }

        let _ = unsafe { KillTimer(None, refresh_timer_id) };
    }

    #[cfg(not(target_os = "windows"))]
    {
        while running.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    tracing::info!("Application exiting");
    Ok(())
}

/// Pause/resume the whole service: stops any active recording, disables the
/// hotkey's tap action (the pause-combo keeps working so the user can
/// resume), and shows/hides the floating button as the visual indicator.
/// Shared by the tray icon's left-click, the "Service: Pause/Resume" menu
/// item, and the hotkey's pause-combo (e.g. RAlt + Space).
async fn toggle_service_pause(
    voice_controller: Arc<Mutex<VoiceController>>,
    button_state_setter: FloatingButtonStateSetter,
    hotkey_active: Arc<AtomicBool>,
    service_paused: Arc<AtomicBool>,
    tray_dirty: Arc<AtomicBool>,
) {
    let pausing = !service_paused.load(Ordering::SeqCst);
    if pausing {
        let mut controller = voice_controller.lock().await;
        if controller.is_recording() {
            tracing::info!("Pausing service: stopping active recording");
            if let Err(e) = controller.stop().await {
                tracing::error!("Failed to stop voice input while pausing: {}", e);
            }
        }
        drop(controller);
        hotkey_active.store(false, Ordering::SeqCst);
        button_state_setter.set_state(ButtonState::Idle);
        button_state_setter.set_visible(false);
        tracing::info!("Service paused");
    } else {
        hotkey_active.store(true, Ordering::SeqCst);
        button_state_setter.set_visible(true);
        tracing::info!("Service resumed");
    }
    service_paused.store(pausing, Ordering::SeqCst);
    tray_dirty.store(true, Ordering::SeqCst);
}

/// Load the tray icon with modern appearance.
///
/// `paused` swaps the purple/blue gradient for a muted gray so the tray icon
/// itself shows at a glance that the service is paused.
fn load_icon(paused: bool) -> Result<tray_icon::Icon> {
    let width = 32u32;
    let height = 32u32;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);

    let center_x = width as f32 / 2.0;
    let center_y = height as f32 / 2.0;
    let radius = (width.min(height) as f32 / 2.0) - 1.0;

    // Modern gradient colors (purple to blue), muted to gray when paused.
    let (color_start, color_end) = if paused {
        ((120u8, 120u8, 120u8), (90u8, 90u8, 90u8))
    } else {
        ((139u8, 92u8, 246u8), (59u8, 130u8, 246u8))
    };

    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 - center_x;
            let dy = y as f32 - center_y;
            let dist = (dx * dx + dy * dy).sqrt();

            if dist <= radius {
                // Gradient based on position (top-left to bottom-right)
                let gradient_t = ((x as f32 / width as f32) + (y as f32 / height as f32)) / 2.0;
                let r = (color_start.0 as f32 * (1.0 - gradient_t) + color_end.0 as f32 * gradient_t) as u8;
                let g = (color_start.1 as f32 * (1.0 - gradient_t) + color_end.1 as f32 * gradient_t) as u8;
                let b = (color_start.2 as f32 * (1.0 - gradient_t) + color_end.2 as f32 * gradient_t) as u8;

                // Soft edge anti-aliasing
                let alpha = if dist > radius - 1.5 {
                    ((radius - dist + 1.5) / 1.5 * 255.0) as u8
                } else {
                    255
                };

                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                rgba.push(alpha);
            } else {
                rgba.push(0);
                rgba.push(0);
                rgba.push(0);
                rgba.push(0);
            }
        }
    }

    // Draw modern microphone icon (white, clean design)
    let mic_color = (255u8, 255u8, 255u8, 255u8);
    let cx = center_x as i32;
    let cy = center_y as i32;

    // Mic head (rounded rectangle)
    for dy in -5..=3 {
        for dx in -3..=3 {
            let in_corner = (dy == -5 || dy == 3) && (dx == -3 || dx == 3);
            if !in_corner {
                let idx = ((cy + dy) as u32 * width + (cx + dx) as u32) as usize * 4;
                if idx + 3 < rgba.len() {
                    rgba[idx] = mic_color.0;
                    rgba[idx + 1] = mic_color.1;
                    rgba[idx + 2] = mic_color.2;
                    rgba[idx + 3] = mic_color.3;
                }
            }
        }
    }

    // Mic holder arc (U shape)
    for dx in -5..=5 {
        let idx = ((cy + 6) as u32 * width + (cx + dx) as u32) as usize * 4;
        if idx + 3 < rgba.len() {
            rgba[idx] = mic_color.0;
            rgba[idx + 1] = mic_color.1;
            rgba[idx + 2] = mic_color.2;
            rgba[idx + 3] = mic_color.3;
        }
    }
    for dy in 3..=6 {
        for dx in [-5, 5] {
            let idx = ((cy + dy) as u32 * width + (cx + dx) as u32) as usize * 4;
            if idx + 3 < rgba.len() {
                rgba[idx] = mic_color.0;
                rgba[idx + 1] = mic_color.1;
                rgba[idx + 2] = mic_color.2;
                rgba[idx + 3] = mic_color.3;
            }
        }
    }

    // Mic stand
    for dy in 7..=10 {
        let idx = ((cy + dy) as u32 * width + cx as u32) as usize * 4;
        if idx + 3 < rgba.len() {
            rgba[idx] = mic_color.0;
            rgba[idx + 1] = mic_color.1;
            rgba[idx + 2] = mic_color.2;
            rgba[idx + 3] = mic_color.3;
        }
    }

    // Mic base
    for dx in -3..=3 {
        let idx = ((cy + 10) as u32 * width + (cx + dx) as u32) as usize * 4;
        if idx + 3 < rgba.len() {
            rgba[idx] = mic_color.0;
            rgba[idx + 1] = mic_color.1;
            rgba[idx + 2] = mic_color.2;
            rgba[idx + 3] = mic_color.3;
        }
    }

    let icon = tray_icon::Icon::from_rgba(rgba, width, height)?;
    Ok(icon)
}
