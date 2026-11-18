import React, { useEffect, useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { ConnectionCard } from './components/ConnectionCard';
import { PcAudioCard } from './components/PcAudioCard';
import { MicAudioCard } from './components/MicAudioCard';
import { AppSnapshot, AudioConfig, AudioLevels, TransportMode } from './types';

export const App: React.FC = () => {
  const [snapshot, setSnapshot] = useState<AppSnapshot | null>(null);
  const [outputDevices, setOutputDevices] = useState<string[]>([]);

  // Initial fetch of snapshot and output audio devices
  useEffect(() => {
    invoke<AppSnapshot>('get_app_snapshot')
      .then((data) => setSnapshot(data))
      .catch((err) => console.error('Failed to get snapshot:', err));

    invoke<string[]>('get_output_audio_devices')
      .then((devs) => setOutputDevices(devs))
      .catch((err) => console.error('Failed to get devices:', err));
  }, []);

  // Subscribe to split telemetry (`audio-levels` at 20Hz + on-change `app-state` + `config-updated`)
  useEffect(() => {
    let unlistenLevels: (() => void) | undefined;
    let unlistenState: (() => void) | undefined;
    let unlistenConfig: (() => void) | undefined;

    listen<AudioLevels>('audio-levels', (event) => {
      setSnapshot((prev) => (prev ? { ...prev, levels: event.payload } : null));
    }).then((unlisten) => {
      unlistenLevels = unlisten;
    }).catch((err) => {
      console.warn('Could not listen to audio-levels event:', err);
    });

    listen<AppSnapshot>('app-state', (event) => {
      setSnapshot(event.payload);
    }).then((unlisten) => {
      unlistenState = unlisten;
    }).catch((err) => {
      console.warn('Could not listen to app-state event:', err);
    });

    listen<AudioConfig>('config-updated', (event) => {
      setSnapshot((prev) => (prev ? { ...prev, config: event.payload } : null));
    }).then((unlisten) => {
      unlistenConfig = unlisten;
    }).catch((err) => {
      console.warn('Could not listen to config-updated event:', err);
    });

    return () => {
      if (unlistenLevels) unlistenLevels();
      if (unlistenState) unlistenState();
      if (unlistenConfig) unlistenConfig();
    };
  }, []);

  // Update backend audio configuration
  const handleUpdateConfig = useCallback((newConfig: AudioConfig) => {
    setSnapshot((prev) => (prev ? { ...prev, config: newConfig } : null));
    invoke<AudioConfig>('update_audio_config', { newConfig })
      .catch((err) => console.error('Failed to update config:', err));
  }, []);

  // Toggle Mic Mute
  const handleToggleMicMute = useCallback(() => {
    invoke<boolean>('toggle_mic_mute')
      .then((muted) => {
        setSnapshot((prev) =>
          prev ? { ...prev, config: { ...prev.config, mic_muted: muted } } : null
        );
      })
      .catch((err) => console.error('Failed to toggle mic mute:', err));
  }, []);

  // Toggle PC Mute
  const handleTogglePcMute = useCallback(() => {
    invoke<boolean>('toggle_pc_mute')
      .then((muted) => {
        setSnapshot((prev) =>
          prev ? { ...prev, config: { ...prev.config, pc_muted: muted } } : null
        );
      })
      .catch((err) => console.error('Failed to toggle pc mute:', err));
  }, []);

  // Push-to-Talk active state trigger
  const handleSetPttActive = useCallback((active: boolean) => {
    invoke<boolean>('set_push_to_talk_active', { active })
      .then((isActive) => {
        setSnapshot((prev) =>
          prev
            ? { ...prev, config: { ...prev.config, push_to_talk_active: isActive } }
            : null
        );
      })
      .catch((err) => console.error('Failed to set PTT active:', err));
  }, []);

  // Manual IP pairing
  const handleSetManualPeer = useCallback((ipAndPort: string) => {
    invoke<string>('set_manual_peer', { ipAndPort })
      .then(() => {
        invoke<AppSnapshot>('get_app_snapshot').then((s) => setSnapshot(s));
      })
      .catch((err) => {
        alert(`Failed to set manual peer: ${err}`);
      });
  }, []);

  // Transport mode selector (Auto / Wi-Fi UDP / USB Cable)
  const handleSetTransportMode = useCallback((mode: TransportMode) => {
    setSnapshot((prev) =>
      prev ? { ...prev, network: { ...prev.network, transport_mode: mode } } : null
    );
    invoke<TransportMode>('set_transport_mode', { mode })
      .catch((err) => console.error('Failed to set transport mode:', err));
  }, []);

  return (
    <div className="min-h-screen w-screen bg-[#F4F4F5] text-brand-dark flex flex-col p-6 overflow-x-hidden select-none">
      {/* Main Container matching Figma 960x600 layout */}
      <div className="max-w-5xl w-full mx-auto flex flex-col gap-4">
        {snapshot ? (
          <>
            {/* Top Single-Row Utility Bar */}
            <ConnectionCard
              network={snapshot.network}
              onSetManualPeer={handleSetManualPeer}
              onSetTransportMode={handleSetTransportMode}
            />

            {/* Dual Panel Grid (PC Output on Left, iPhone Mic on Right) */}
            <div className="grid grid-cols-1 md:grid-cols-2 gap-4 items-stretch">
              <PcAudioCard
                config={snapshot.config}
                levels={snapshot.levels}
                onToggleMute={handleTogglePcMute}
                onUpdateConfig={handleUpdateConfig}
              />

              <MicAudioCard
                config={snapshot.config}
                levels={snapshot.levels}
                vbcableDeviceName={snapshot.vbcable_device_name}
                vbcableDetected={snapshot.vbcable_detected}
                availableDevices={outputDevices}
                onToggleMute={handleToggleMicMute}
                onSetPttActive={handleSetPttActive}
                onUpdateConfig={handleUpdateConfig}
              />
            </div>
          </>
        ) : (
          <div className="clean-panel p-16 flex flex-col items-center justify-center gap-4 text-center">
            <div className="w-8 h-8 rounded-full border-2 border-brand-dark border-t-transparent animate-spin" />
            <div className="text-xs text-brand-muted font-medium">Initializing AudioSync Pro...</div>
          </div>
        )}
      </div>
    </div>
  );
};
