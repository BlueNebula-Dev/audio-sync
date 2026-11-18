pub mod audio_engine;
pub mod network;
pub mod protocol;
pub mod usb_tunnel;

#[cfg(not(test))]
mod app_runtime {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use global_hotkey::hotkey::{Code, HotKey, Modifiers};
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    use parking_lot::Mutex;
    use serde::{Deserialize, Serialize};
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    use tauri::{Emitter, Manager, State, WindowEvent};
    use tokio::sync::mpsc;

    use crate::audio_engine::{list_output_devices, AudioConfig, AudioEngine, AudioLevels};
    use crate::network::{
        NetworkEngine, NetworkStatus, SharedNetworkState, TransportMode, DEFAULT_UDP_PORT,
    };
    use crate::protocol::ControlMessage;

    /// Shared application state managed by Tauri.
    pub struct AppState {
        pub audio_config: Arc<Mutex<AudioConfig>>,
        pub audio_engine: Arc<AudioEngine>,
        pub network_engine: Arc<NetworkEngine>,
        pub control_tx: mpsc::Sender<ControlMessage>,
        pub ui_config_tx: mpsc::UnboundedSender<AudioConfig>,
    }

    /// Complete telemetry & configuration snapshot returned to the UI.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct AppSnapshot {
        pub config: AudioConfig,
        pub levels: AudioLevels,
        pub network: NetworkStatus,
        pub vbcable_device_name: String,
        pub vbcable_detected: bool,
    }

    #[tauri::command]
    fn get_app_snapshot(state: State<'_, AppState>) -> AppSnapshot {
        let config = state.audio_config.lock().clone();
        let levels = state.audio_engine.telemetry.snapshot();
        let network = state.network_engine.state.snapshot();
        let (vbcable_device_name, vbcable_detected) = state.audio_engine.vbcable_status();

        AppSnapshot {
            config,
            levels,
            network,
            vbcable_device_name,
            vbcable_detected,
        }
    }

    fn sync_config_to_peer_and_ui(
        cfg: &AudioConfig,
        net_state: &SharedNetworkState,
        control_tx: &mpsc::Sender<ControlMessage>,
        ui_config_tx: &mpsc::UnboundedSender<AudioConfig>,
    ) {
        let rev = net_state.next_revision();
        let msg = net_state.build_sync_message(cfg, rev);
        let _ = control_tx.try_send(msg);
        let _ = ui_config_tx.send(cfg.clone());
    }

    #[tauri::command]
    fn update_audio_config(state: State<'_, AppState>, new_config: AudioConfig) -> AudioConfig {
        {
            let mut cfg = state.audio_config.lock();
            *cfg = new_config.clone();
        }
        sync_config_to_peer_and_ui(
            &new_config,
            &state.network_engine.state,
            &state.control_tx,
            &state.ui_config_tx,
        );
        new_config
    }

    #[tauri::command]
    fn toggle_mic_mute(state: State<'_, AppState>) -> bool {
        let snapshot = {
            let mut cfg = state.audio_config.lock();
            cfg.mic_muted = !cfg.mic_muted;
            cfg.clone()
        };
        sync_config_to_peer_and_ui(
            &snapshot,
            &state.network_engine.state,
            &state.control_tx,
            &state.ui_config_tx,
        );
        snapshot.mic_muted
    }

    #[tauri::command]
    fn toggle_pc_mute(state: State<'_, AppState>) -> bool {
        let snapshot = {
            let mut cfg = state.audio_config.lock();
            cfg.pc_muted = !cfg.pc_muted;
            cfg.clone()
        };
        sync_config_to_peer_and_ui(
            &snapshot,
            &state.network_engine.state,
            &state.control_tx,
            &state.ui_config_tx,
        );
        snapshot.pc_muted
    }

    #[tauri::command]
    fn set_push_to_talk_active(state: State<'_, AppState>, active: bool) -> bool {
        let snapshot = {
            let mut cfg = state.audio_config.lock();
            cfg.push_to_talk_active = active;
            cfg.clone()
        };
        sync_config_to_peer_and_ui(
            &snapshot,
            &state.network_engine.state,
            &state.control_tx,
            &state.ui_config_tx,
        );
        snapshot.push_to_talk_active
    }

    #[tauri::command]
    fn set_manual_peer(state: State<'_, AppState>, ip_and_port: String) -> Result<String, String> {
        let addr: SocketAddr = ip_and_port
            .parse()
            .map_err(|e| format!("Invalid IP:Port address '{ip_and_port}': {e}"))?;
        state.network_engine.state.set_manual_peer(addr);
        Ok(addr.to_string())
    }

    #[tauri::command]
    fn get_output_audio_devices() -> Vec<String> {
        list_output_devices()
    }

    #[tauri::command]
    fn set_transport_mode(state: State<'_, AppState>, mode: TransportMode) -> TransportMode {
        state.network_engine.state.set_transport_mode(mode);
        mode
    }

    /// Initializes global Windows hotkeys:
    /// - `Ctrl+Shift+M`: Toggle Microphone Mute
    /// - `Ctrl+Shift+Space`: Hold for Push-to-Talk
    fn spawn_global_hotkey_listener(
        audio_config: Arc<Mutex<AudioConfig>>,
        net_state: Arc<SharedNetworkState>,
        control_tx: mpsc::Sender<ControlMessage>,
        ui_config_tx: mpsc::UnboundedSender<AudioConfig>,
    ) {
        std::thread::Builder::new()
            .name("global-hotkey-listener".to_string())
            .spawn(move || {
                let Ok(manager) = GlobalHotKeyManager::new() else {
                    log::warn!("Could not initialize GlobalHotKeyManager on this thread");
                    return;
                };

                let mute_hotkey =
                    HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyM);
                let ptt_hotkey =
                    HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space);

                let _ = manager.register(mute_hotkey);
                let _ = manager.register(ptt_hotkey);

                let receiver = GlobalHotKeyEvent::receiver();
                while let Ok(event) = receiver.recv() {
                    if event.id == mute_hotkey.id() && event.state == HotKeyState::Pressed {
                        let snapshot = {
                            let mut cfg = audio_config.lock();
                            cfg.mic_muted = !cfg.mic_muted;
                            cfg.clone()
                        };
                        sync_config_to_peer_and_ui(
                            &snapshot,
                            &net_state,
                            &control_tx,
                            &ui_config_tx,
                        );
                    } else if event.id == ptt_hotkey.id() {
                        let snapshot = {
                            let mut cfg = audio_config.lock();
                            cfg.push_to_talk_active = event.state == HotKeyState::Pressed;
                            cfg.clone()
                        };
                        sync_config_to_peer_and_ui(
                            &snapshot,
                            &net_state,
                            &control_tx,
                            &ui_config_tx,
                        );
                    }
                }
            })
            .ok();
    }

    #[cfg_attr(mobile, tauri::mobile_entry_point)]
    pub fn run() {
        let audio_config = Arc::new(Mutex::new(AudioConfig::default()));
        let audio_engine = Arc::new(AudioEngine::new(Arc::clone(&audio_config)));
        let network_engine = Arc::new(NetworkEngine::new(Arc::clone(&audio_config)));
        let (control_tx, control_rx) = mpsc::channel::<ControlMessage>(64);
        let (ui_config_tx, mut ui_config_rx) = mpsc::unbounded_channel::<AudioConfig>();

        let app_state = AppState {
            audio_config: Arc::clone(&audio_config),
            audio_engine: Arc::clone(&audio_engine),
            network_engine: Arc::clone(&network_engine),
            control_tx: control_tx.clone(),
            ui_config_tx: ui_config_tx.clone(),
        };

        let remote_config_tx = ui_config_tx.clone();
        let hotkey_ui_config_tx = ui_config_tx.clone();
        let tray_ui_config_tx = ui_config_tx;

        tauri::Builder::default()
            .manage(app_state)
            .on_window_event(|window, event| match event {
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                WindowEvent::Resized(_) => {
                    if window.is_minimized().unwrap_or(false) {
                        let _ = window.hide();
                    }
                }
                _ => {}
            })
            .setup(move |app| {
                // System tray icon with Show / Toggle Mic Mute / Quit controls
                let show_item =
                    MenuItem::with_id(app, "show", "Show AudioSync Pro", true, None::<&str>)?;
                let mute_item = MenuItem::with_id(
                    app,
                    "toggle_mic_mute",
                    "Toggle Mic Mute",
                    true,
                    None::<&str>,
                )?;
                let quit_item =
                    MenuItem::with_id(app, "quit", "Quit AudioSync Pro", true, None::<&str>)?;
                let tray_menu = Menu::with_items(app, &[&show_item, &mute_item, &quit_item])?;

                let tray_cfg_ref = Arc::clone(&audio_config);
                let tray_net_ref = Arc::clone(&network_engine.state);
                let tray_ctrl_tx = control_tx.clone();
                let tray_ui_tx = tray_ui_config_tx;

                let mut tray_builder = TrayIconBuilder::with_id("main-tray")
                    .menu(&tray_menu)
                    .tooltip("AudioSync Pro")
                    .show_menu_on_left_click(false)
                    .on_menu_event(move |app, event| match event.id.as_ref() {
                        "show" => {
                            if let Some(win) = app.get_webview_window("main") {
                                let _ = win.unminimize();
                                let _ = win.show();
                                let _ = win.set_focus();
                            }
                        }
                        "toggle_mic_mute" => {
                            let snapshot = {
                                let mut cfg = tray_cfg_ref.lock();
                                cfg.mic_muted = !cfg.mic_muted;
                                cfg.clone()
                            };
                            sync_config_to_peer_and_ui(
                                &snapshot,
                                &tray_net_ref,
                                &tray_ctrl_tx,
                                &tray_ui_tx,
                            );
                        }
                        "quit" => {
                            app.exit(0);
                        }
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            let app = tray.app_handle();
                            if let Some(win) = app.get_webview_window("main") {
                                let _ = win.unminimize();
                                let _ = win.show();
                                let _ = win.set_focus();
                            }
                        }
                    });

                if let Some(icon) = app.default_window_icon().cloned() {
                    tray_builder = tray_builder.icon(icon);
                }
                let _tray = tray_builder.build(app)?;

                let (outbound_opus_tx, outbound_opus_rx) = mpsc::channel(256);
                let (inbound_mic_tx, inbound_mic_rx) = mpsc::channel(256);

                if let Err(err) = audio_engine.start(outbound_opus_tx, inbound_mic_rx) {
                    log::error!("Failed to start AudioEngine: {err}");
                }

                let net_engine_for_spawn = Arc::clone(&network_engine);
                tauri::async_runtime::spawn(async move {
                    if let Err(err) = net_engine_for_spawn
                        .start(
                            DEFAULT_UDP_PORT,
                            outbound_opus_rx,
                            inbound_mic_tx,
                            control_rx,
                            remote_config_tx,
                        )
                        .await
                    {
                        log::error!("Failed to start NetworkEngine: {err}");
                    }
                });

                let app_handle_remote = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    while let Some(updated_cfg) = ui_config_rx.recv().await {
                        let _ = app_handle_remote.emit("config-updated", updated_cfg);
                    }
                });

                spawn_global_hotkey_listener(
                    Arc::clone(&audio_config),
                    Arc::clone(&network_engine.state),
                    control_tx.clone(),
                    hotkey_ui_config_tx,
                );

                // Split telemetry emitter:
                // - `audio-levels` (20-byte float struct) at 20 Hz only while window is visible & changing
                // - `app-state` (full snapshot) only on config/network state change or 1 Hz heartbeat
                // - Completely paused when minimized or hidden to tray (0% WebView2 GPU/IPC churn)
                let app_handle = app.handle().clone();
                let audio_eng = Arc::clone(&audio_engine);
                let net_eng = Arc::clone(&network_engine);
                let aud_cfg = Arc::clone(&audio_config);

                tauri::async_runtime::spawn(async move {
                    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(50));
                    let mut last_levels: Option<AudioLevels> = None;
                    let mut last_config: Option<AudioConfig> = None;
                    let mut last_network: Option<NetworkStatus> = None;
                    let mut last_vbcable: Option<(String, bool)> = None;
                    let mut ticks_since_state_emit: u32 = 20;
                    let mut was_hidden = false;

                    loop {
                        ticker.tick().await;

                        let hidden = app_handle
                            .get_webview_window("main")
                            .map(|win| {
                                win.is_minimized().unwrap_or(false)
                                    || !win.is_visible().unwrap_or(true)
                            })
                            .unwrap_or(false);

                        if hidden {
                            was_hidden = true;
                            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                            continue;
                        }

                        if was_hidden {
                            was_hidden = false;
                            last_levels = None;
                            ticks_since_state_emit = 20;
                        }

                        let levels = audio_eng.telemetry.snapshot();
                        let levels_changed = last_levels.as_ref() != Some(&levels);
                        let non_zero_levels = levels != AudioLevels::default();
                        if levels_changed && (non_zero_levels || last_levels.is_some()) {
                            let _ = app_handle.emit("audio-levels", levels);
                            last_levels = Some(levels);
                        }

                        ticks_since_state_emit += 1;
                        let config = aud_cfg.lock().clone();
                        let network = net_eng.state.snapshot();
                        let vbcable = audio_eng.vbcable_status();

                        let state_changed = last_config.as_ref() != Some(&config)
                            || last_network.as_ref() != Some(&network)
                            || last_vbcable.as_ref() != Some(&vbcable);

                        if state_changed || ticks_since_state_emit >= 20 {
                            ticks_since_state_emit = 0;
                            last_config = Some(config.clone());
                            last_network = Some(network.clone());
                            last_vbcable = Some(vbcable.clone());

                            let snapshot = AppSnapshot {
                                config,
                                levels,
                                network,
                                vbcable_device_name: vbcable.0,
                                vbcable_detected: vbcable.1,
                            };
                            let _ = app_handle.emit("app-state", snapshot);
                        }
                    }
                });

                Ok(())
            })
            .invoke_handler(tauri::generate_handler![
                get_app_snapshot,
                update_audio_config,
                toggle_mic_mute,
                toggle_pc_mute,
                set_push_to_talk_active,
                set_manual_peer,
                get_output_audio_devices,
                set_transport_mode,
            ])
            .run(tauri::generate_context!())
            .expect("error while running AudioSync Pro Tauri application");
    }
}

#[cfg(not(test))]
pub use app_runtime::{run, AppSnapshot, AppState};

#[cfg(test)]
pub fn run() {}
