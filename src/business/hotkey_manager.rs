//! Hotkey Manager
//!
//! Manages global hotkeys for triggering voice input.
//! Supports combo keys (Ctrl+Shift+V) and double-tap of modifier keys (Ctrl).
//! For the modifier-key hook path, holding the trigger key and pressing a
//! second "pause combo" key (e.g. RAlt + Space) is reported separately from
//! a normal tap, so callers can wire it to a different action (pause/resume
//! the whole service instead of toggling recording).

use anyhow::{anyhow, Result};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::data::HotkeyConfig;

/// Hotkey mode
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HotkeyMode {
    /// Combination key mode (e.g., Ctrl+Shift+V)
    Combo,
    /// Double-tap mode (e.g., double-tap Ctrl)
    DoubleTap,
    /// Single-tap mode (e.g., single press of Right Alt)
    SingleTap,
}

/// Whether a key name refers to a bare modifier key that needs the low-level
/// keyboard hook (RegisterHotKey cannot register a modifier alone).
fn is_modifier_key(key_lower: &str) -> bool {
    matches!(
        key_lower,
        "ctrl" | "lctrl" | "rctrl" | "shift" | "lshift" | "rshift" | "alt" | "lalt" | "ralt"
    )
}

/// Parse a key name into its raw Win32 virtual-key code, for the
/// "pause combo" second key (e.g. Space in "hold RAlt + tap Space").
/// Deliberately independent of the `windows` crate (plain numeric VK codes
/// per the Win32 Virtual-Key Codes table) so it compiles on any target.
fn parse_raw_vk(key: &str) -> Option<u16> {
    let upper = key.to_uppercase();
    let vk = match upper.as_str() {
        "SPACE" => 0x20,
        "ENTER" | "RETURN" => 0x0D,
        "TAB" => 0x09,
        "ESCAPE" | "ESC" => 0x1B,
        "F1" => 0x70,
        "F2" => 0x71,
        "F3" => 0x72,
        "F4" => 0x73,
        "F5" => 0x74,
        "F6" => 0x75,
        "F7" => 0x76,
        "F8" => 0x77,
        "F9" => 0x78,
        "F10" => 0x79,
        "F11" => 0x7A,
        "F12" => 0x7B,
        s if s.len() == 1 && s.chars().next().unwrap().is_ascii_alphanumeric() => {
            // VK codes for '0'-'9' and 'A'-'Z' equal their ASCII values.
            s.chars().next().unwrap() as u16
        }
        _ => return None,
    };
    Some(vk)
}

/// Hotkey manager for global hotkey handling
pub struct HotkeyManager {
    _manager: Option<GlobalHotKeyManager>,
    mode: HotkeyMode,
    double_tap_interval: Duration,
    double_tap_key: String,
    /// Raw Win32 virtual-key code for the pause-combo key (e.g. Space), if
    /// configured and recognized. `None` disables the combo feature.
    pause_combo_vk: Option<u16>,
    is_active: Arc<AtomicBool>,
}

impl HotkeyManager {
    /// Create a new hotkey manager based on configuration
    pub fn new(config: &HotkeyConfig) -> Result<Self> {
        let mode = match config.mode.as_str() {
            "combo" => HotkeyMode::Combo,
            "single_tap" => HotkeyMode::SingleTap,
            _ => HotkeyMode::DoubleTap,
        };

        let manager = GlobalHotKeyManager::new()
            .map_err(|e| anyhow!("Failed to create hotkey manager: {}", e))?;

        // Register hotkey based on mode
        match mode {
            HotkeyMode::Combo => {
                // Parse combo key (default: Ctrl+Shift+V)
                let hotkey = parse_combo_key(&config.combo_key)?;
                manager
                    .register(hotkey)
                    .map_err(|e| anyhow!("Failed to register hotkey: {}", e))?;
                tracing::info!("Registered combo hotkey: {}", config.combo_key);
            }
            HotkeyMode::DoubleTap | HotkeyMode::SingleTap => {
                // For modifier keys like Ctrl/Alt (including left/right specific
                // variants), we use a low-level keyboard hook since RegisterHotKey
                // cannot register a bare modifier. For regular keys, global_hotkey
                // works directly.
                let key_lower = config.double_tap_key.to_lowercase();
                if is_modifier_key(&key_lower) {
                    tracing::info!(
                        "{:?} modifier key: {} (using keyboard hook)",
                        mode,
                        config.double_tap_key
                    );
                } else {
                    // Regular key - can use global_hotkey
                    let hotkey = HotKey::new(None, parse_key_code(&config.double_tap_key)?);
                    manager
                        .register(hotkey)
                        .map_err(|e| anyhow!("Failed to register hotkey: {}", e))?;
                    tracing::info!("Registered {:?} hotkey: {}", mode, config.double_tap_key);
                }
            }
        }

        let pause_combo_vk = if config.pause_combo_key.trim().is_empty() {
            None
        } else {
            match parse_raw_vk(&config.pause_combo_key) {
                Some(vk) => Some(vk),
                None => {
                    tracing::warn!(
                        "Unknown pause_combo_key '{}', pause-by-chord disabled",
                        config.pause_combo_key
                    );
                    None
                }
            }
        };

        Ok(Self {
            _manager: Some(manager),
            mode,
            double_tap_interval: Duration::from_millis(config.double_tap_interval),
            double_tap_key: config.double_tap_key.clone(),
            pause_combo_vk,
            is_active: Arc::new(AtomicBool::new(true)),
        })
    }

    /// Set callbacks for the hotkey: `on_tap` fires on a normal
    /// single/double-tap (toggles recording), `on_pause_combo` fires when the
    /// pause-combo key (e.g. Space) is pressed while the trigger key is held
    /// down (used to pause/resume the whole service). `on_pause_combo` only
    /// applies to the bare-modifier-key hook path (double_tap/single_tap
    /// modes with a Ctrl/Shift/Alt-family key); it's simply never called for
    /// combo mode, a non-modifier key, or if `pause_combo_key` is unset.
    pub fn on_trigger<F1, F2>(&self, on_tap: F1, on_pause_combo: F2)
    where
        F1: Fn() + Send + Sync + 'static,
        F2: Fn() + Send + Sync + 'static,
    {
        let mode = self.mode.clone();
        let double_tap_interval = self.double_tap_interval;
        let double_tap_key = self.double_tap_key.clone();
        let pause_combo_vk = self.pause_combo_vk;
        let is_active = self.is_active.clone();
        let callback = Arc::new(on_tap);

        // Check if we need to use keyboard hook for modifier keys
        let key_lower = double_tap_key.to_lowercase();
        let use_keyboard_hook =
            matches!(mode, HotkeyMode::DoubleTap | HotkeyMode::SingleTap) && is_modifier_key(&key_lower);

        if use_keyboard_hook {
            // Use Windows keyboard hook for modifier key double-tap / single-tap
            #[cfg(target_os = "windows")]
            {
                let callback_clone: Arc<dyn Fn() + Send + Sync> = callback.clone();
                let pause_combo_callback: Arc<dyn Fn() + Send + Sync> = Arc::new(on_pause_combo);
                let require_double_tap = mode == HotkeyMode::DoubleTap;
                thread::spawn(move || {
                    run_modifier_key_hook(
                        key_lower,
                        double_tap_interval,
                        require_double_tap,
                        pause_combo_vk,
                        is_active,
                        callback_clone,
                        pause_combo_callback,
                    );
                });
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = on_pause_combo;
                tracing::warn!("Modifier key hotkeys are not supported on this platform");
            }
        } else {
            // Use global_hotkey receiver
            thread::spawn(move || {
                let receiver = GlobalHotKeyEvent::receiver();
                let mut last_press_time: Option<Instant> = None;

                loop {
                    if !is_active.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }

                    if let Ok(_event) = receiver.recv() {
                        match mode {
                            HotkeyMode::Combo | HotkeyMode::SingleTap => {
                                callback();
                            }
                            HotkeyMode::DoubleTap => {
                                let now = Instant::now();

                                if let Some(last) = last_press_time {
                                    let elapsed = now.duration_since(last);
                                    if elapsed <= double_tap_interval {
                                        callback();
                                        last_press_time = None;
                                        continue;
                                    }
                                }

                                last_press_time = Some(now);
                            }
                        }
                    }
                }
            });
        }
    }

    /// Stop the hotkey manager
    pub fn stop(&self) {
        self.is_active.store(false, Ordering::SeqCst);
    }

    /// Get a shared handle to the active/inactive flag.
    ///
    /// Lets callers pause/resume hotkey triggering (e.g. from a "pause service"
    /// menu action) without needing `HotkeyManager` itself to be shared across
    /// threads.
    pub fn active_handle(&self) -> Arc<AtomicBool> {
        self.is_active.clone()
    }
}

/// Windows keyboard hook for modifier key double-tap / single-tap /
/// pause-combo (modifier held + second key) detection
#[cfg(target_os = "windows")]
fn run_modifier_key_hook(
    key: String,
    interval: Duration,
    require_double_tap: bool,
    pause_combo_vk: Option<u16>,
    is_active: Arc<AtomicBool>,
    callback: Arc<dyn Fn() + Send + Sync>,
    pause_combo_callback: Arc<dyn Fn() + Send + Sync>,
) {
    use std::cell::RefCell;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        VK_CONTROL, VK_LCONTROL, VK_RCONTROL, VK_LSHIFT, VK_RSHIFT, VK_LMENU, VK_RMENU,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
        HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYUP,
        WM_SYSKEYUP,
    };

    // Determine which virtual keys to watch (left/right-specific variants only
    // watch that side; the bare name watches both).
    let target_vks: Vec<u16> = match key.as_str() {
        "ctrl" => vec![VK_CONTROL.0, VK_LCONTROL.0, VK_RCONTROL.0],
        "lctrl" => vec![VK_LCONTROL.0],
        "rctrl" => vec![VK_RCONTROL.0],
        "shift" => vec![VK_LSHIFT.0, VK_RSHIFT.0],
        "lshift" => vec![VK_LSHIFT.0],
        "rshift" => vec![VK_RSHIFT.0],
        "alt" => vec![VK_LMENU.0, VK_RMENU.0],
        "lalt" => vec![VK_LMENU.0],
        "ralt" => vec![VK_RMENU.0],
        _ => vec![],
    };

    if target_vks.is_empty() {
        tracing::error!("Unknown modifier key: {}", key);
        return;
    }

    tracing::info!(
        "Starting keyboard hook for {} {} detection{}",
        key,
        if require_double_tap { "double-tap" } else { "single-tap" },
        match pause_combo_vk {
            Some(vk) => format!(" (+ pause-combo vk=0x{:02X})", vk),
            None => String::new(),
        }
    );

    // Thread-local state for hook callback
    thread_local! {
        static HOOK_STATE: RefCell<Option<HookState>> = RefCell::new(None);
    }

    struct HookState {
        target_vks: Vec<u16>,
        interval: Duration,
        require_double_tap: bool,
        last_release: Option<Instant>,
        /// Whether the trigger key is currently held down.
        modifier_down: bool,
        /// Raw vk code of the pause-combo key, if configured.
        pause_combo_vk: Option<u16>,
        /// Whether the pause-combo already fired during the current hold of
        /// the trigger key, so the eventual release isn't also treated as a
        /// plain tap, and repeated keydowns from held-key auto-repeat don't
        /// re-fire it.
        pause_combo_fired: bool,
        callback: Arc<dyn Fn() + Send + Sync>,
        pause_combo_callback: Arc<dyn Fn() + Send + Sync>,
        is_active: Arc<AtomicBool>,
    }

    // Initialize thread-local state
    HOOK_STATE.with(|state| {
        *state.borrow_mut() = Some(HookState {
            target_vks,
            interval,
            require_double_tap,
            last_release: None,
            modifier_down: false,
            pause_combo_vk,
            pause_combo_fired: false,
            callback,
            pause_combo_callback,
            is_active,
        });
    });

    // Low-level keyboard hook procedure
    unsafe extern "system" fn keyboard_hook_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code >= 0 {
            let kb_struct = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            let vk_code = kb_struct.vkCode as u16;
            let is_key_up = wparam.0 as u32 == WM_KEYUP || wparam.0 as u32 == WM_SYSKEYUP;

            let mut swallow = false;

            HOOK_STATE.with(|state| {
                if let Some(ref mut hook_state) = *state.borrow_mut() {
                    let is_modifier = hook_state.target_vks.contains(&vk_code);
                    let is_combo_key = hook_state.pause_combo_vk == Some(vk_code);

                    if is_modifier {
                        // Eat both the down and up of the trigger key so it
                        // never reaches the foreground app. Alt (and to a
                        // lesser extent Ctrl/Shift) pressed alone is handled
                        // specially by Windows/apps as a "activate the menu /
                        // shift focus" gesture; left unswallowed, using it as
                        // a hotkey steals focus from whatever the user was
                        // typing into and needs an extra click to recover.
                        // This is unconditional (not gated on `is_active`):
                        // the key must keep working even while the service is
                        // paused, since the pause-combo on it is how you resume.
                        swallow = true;

                        if is_key_up {
                            hook_state.modifier_down = false;

                            // A pause-combo already fired during this hold ->
                            // the release itself is not also a tap.
                            let combo_fired = hook_state.pause_combo_fired;
                            hook_state.pause_combo_fired = false;

                            if combo_fired {
                                // Already handled at combo-keydown time.
                            } else if !hook_state.is_active.load(Ordering::SeqCst) {
                                // Paused: swallow the tap but don't toggle
                                // recording.
                            } else if hook_state.require_double_tap {
                                let now = Instant::now();
                                if let Some(last) = hook_state.last_release {
                                    let elapsed = now.duration_since(last);
                                    if elapsed <= hook_state.interval {
                                        // Double-tap detected!
                                        tracing::info!("Double-tap detected!");
                                        (hook_state.callback)();
                                        hook_state.last_release = None;
                                    } else {
                                        hook_state.last_release = Some(now);
                                    }
                                } else {
                                    hook_state.last_release = Some(now);
                                }
                            } else {
                                // Single-tap mode: fire on every release of the key.
                                tracing::info!("Single-tap detected!");
                                (hook_state.callback)();
                            }
                        } else {
                            hook_state.modifier_down = true;
                        }
                    } else if is_combo_key && hook_state.modifier_down {
                        // Only intercept the combo key while the trigger key
                        // is actually held; otherwise it behaves as a normal
                        // key (e.g. Space types a space).
                        swallow = true;

                        if !is_key_up && !hook_state.pause_combo_fired {
                            hook_state.pause_combo_fired = true;
                            // Always fires, paused or not: it's the
                            // pause/resume toggle itself, so gating it on
                            // `is_active` would make resuming impossible.
                            tracing::info!("Pause-combo detected!");
                            (hook_state.pause_combo_callback)();
                        }
                        // Ignore OS auto-repeat keydowns and the eventual
                        // keyup of the combo key; both are just swallowed.
                    }
                }
            });

            if swallow {
                // Per the WH_KEYBOARD_LL docs: returning a nonzero value here
                // (instead of calling CallNextHookEx) blocks the keystroke
                // from propagating further.
                return LRESULT(1);
            }
        }

        CallNextHookEx(HHOOK::default(), code, wparam, lparam)
    }

    // Install the hook
    let hook = unsafe {
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), None, 0)
    };

    match hook {
        Ok(h) => {
            tracing::info!("Keyboard hook installed successfully");

            // Message loop to keep hook alive
            let mut msg = MSG::default();
            unsafe {
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    DispatchMessageW(&msg);
                }
            }

            // Cleanup
            let _ = unsafe { UnhookWindowsHookEx(h) };
            tracing::info!("Keyboard hook uninstalled");
        }
        Err(e) => {
            tracing::error!("Failed to install keyboard hook: {:?}", e);
        }
    }
}

/// Parse a combo key string like "Ctrl+Shift+V"
fn parse_combo_key(key_str: &str) -> Result<HotKey> {
    let parts: Vec<&str> = key_str.split('+').map(|s| s.trim()).collect();

    let mut modifiers = Modifiers::empty();
    let mut key_code: Option<Code> = None;

    for part in parts {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "shift" => modifiers |= Modifiers::SHIFT,
            "alt" => modifiers |= Modifiers::ALT,
            "super" | "win" | "meta" => modifiers |= Modifiers::SUPER,
            _ => {
                key_code = Some(parse_key_code(part)?);
            }
        }
    }

    let code = key_code.ok_or_else(|| anyhow!("No key specified in combo: {}", key_str))?;

    Ok(HotKey::new(Some(modifiers), code))
}

/// Parse a key code from string
fn parse_key_code(key: &str) -> Result<Code> {
    let code = match key.to_uppercase().as_str() {
        "A" => Code::KeyA,
        "B" => Code::KeyB,
        "C" => Code::KeyC,
        "D" => Code::KeyD,
        "E" => Code::KeyE,
        "F" => Code::KeyF,
        "G" => Code::KeyG,
        "H" => Code::KeyH,
        "I" => Code::KeyI,
        "J" => Code::KeyJ,
        "K" => Code::KeyK,
        "L" => Code::KeyL,
        "M" => Code::KeyM,
        "N" => Code::KeyN,
        "O" => Code::KeyO,
        "P" => Code::KeyP,
        "Q" => Code::KeyQ,
        "R" => Code::KeyR,
        "S" => Code::KeyS,
        "T" => Code::KeyT,
        "U" => Code::KeyU,
        "V" => Code::KeyV,
        "W" => Code::KeyW,
        "X" => Code::KeyX,
        "Y" => Code::KeyY,
        "Z" => Code::KeyZ,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "SPACE" => Code::Space,
        "ENTER" | "RETURN" => Code::Enter,
        "ESCAPE" | "ESC" => Code::Escape,
        "F1" => Code::F1,
        "F2" => Code::F2,
        "F3" => Code::F3,
        "F4" => Code::F4,
        "F5" => Code::F5,
        "F6" => Code::F6,
        "F7" => Code::F7,
        "F8" => Code::F8,
        "F9" => Code::F9,
        "F10" => Code::F10,
        "F11" => Code::F11,
        "F12" => Code::F12,
        _ => return Err(anyhow!("Unknown key: {}", key)),
    };

    Ok(code)
}
