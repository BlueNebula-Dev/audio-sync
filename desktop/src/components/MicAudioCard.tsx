import React, { useState } from 'react';
import {
  MicIcon,
  MicOffIcon,
  SparklesIcon,
  ShieldCheckIcon,
  AlertCircleIcon,
  ChevronDownIcon,
} from './Icons';
import { VuMeter } from './VuMeter';
import { AudioConfig, AudioLevels } from '../types';

interface MicAudioCardProps {
  config: AudioConfig;
  levels: AudioLevels;
  vbcableDeviceName: string;
  vbcableDetected: boolean;
  availableDevices: string[];
  onToggleMute: () => void;
  onSetPttActive: (active: boolean) => void;
  onUpdateConfig: (newConfig: AudioConfig) => void;
}

const MicAudioCardComponent: React.FC<MicAudioCardProps> = ({
  config,
  levels,
  vbcableDeviceName,
  vbcableDetected,
  availableDevices,
  onToggleMute,
  onSetPttActive,
  onUpdateConfig,
}) => {
  const [showDeviceDropdown, setShowDeviceDropdown] = useState(false);

  const handleGainChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const val = parseFloat(e.target.value);
    onUpdateConfig({ ...config, mic_gain_db: val });
  };

  const handleVadChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const val = parseFloat(e.target.value);
    onUpdateConfig({ ...config, vad_gate_threshold: val });
  };

  const handleSelectDevice = (dev: string) => {
    onUpdateConfig({ ...config, virtual_mic_device_hint: dev });
    setShowDeviceDropdown(false);
  };

  const isVoiceGateOpen = levels.vad_probability >= config.vad_gate_threshold;

  return (
    <div className="clean-panel p-6 flex flex-col justify-between relative bg-white">
      <div>
        {/* Header matching Figma node 15:72 */}
        <div className="flex items-center justify-between pb-4 border-b border-border">
          <div className="flex items-center gap-3">
            <div className="w-10 h-10 rounded-xl bg-zinc-100 border border-border flex items-center justify-center text-brand-dark">
              <MicIcon size={20} />
            </div>
            <div>
              <h2 className="text-sm font-bold text-brand-dark">iPhone Microphone</h2>
              <p className="text-[11.5px] text-brand-muted">Route phone mic to Windows input</p>
            </div>
          </div>

          <div className="flex items-center gap-2">
            {/* Mute Button */}
            <button
              type="button"
              onClick={onToggleMute}
              disabled={!config.phone_to_pc_enabled}
              className={`px-3 py-1.5 rounded-lg text-xs font-bold transition-all border ${
                !config.phone_to_pc_enabled
                  ? 'opacity-40 text-zinc-400 border-border cursor-not-allowed bg-zinc-50'
                  : config.mic_muted
                  ? 'bg-red-50 text-brand-red border-red-200'
                  : 'bg-white text-brand-dark border-border hover:bg-zinc-50'
              }`}
            >
              {config.mic_muted ? 'Muted' : 'Mute'}
            </button>

            {/* Stream Power Switch */}
            <button
              type="button"
              onClick={() => onUpdateConfig({ ...config, phone_to_pc_enabled: !config.phone_to_pc_enabled })}
              className={`clean-toggle ${config.phone_to_pc_enabled ? 'active' : ''}`}
              title="Stream Power ON / OFF"
              aria-label="Toggle iPhone Mic Stream Power"
            >
              <div className="clean-toggle-thumb" />
            </button>
          </div>
        </div>

        {/* Virtual Device Row matching Figma node 15:83 */}
        <div className="py-3 border-b border-border flex items-center justify-between text-xs relative">
          <span className="text-brand-muted font-normal">Virtual Device</span>
          <div className="flex items-center gap-2">
            {vbcableDetected ? (
              <span className="font-bold text-brand-dark truncate max-w-[200px]">
                {vbcableDeviceName || 'VB-Audio Virtual Cable'}
              </span>
            ) : (
              <span className="inline-flex items-center gap-1 font-bold text-amber-700 bg-amber-50 px-2 py-0.5 rounded border border-amber-200 text-[11px]">
                <AlertCircleIcon size={12} />
                <span>Not Detected</span>
              </span>
            )}
            <button
              type="button"
              onClick={() => setShowDeviceDropdown(!showDeviceDropdown)}
              className="text-brand-muted hover:text-brand-dark font-bold underline transition-colors"
            >
              Change
            </button>
          </div>

          {/* Device Dropdown */}
          {showDeviceDropdown && (
            <div className="absolute top-full right-0 mt-1 bg-white border border-border rounded-xl shadow-modal z-20 py-1 min-w-[240px] max-h-48 overflow-y-auto animate-popover">
              <div className="px-3 py-1.5 text-[11px] font-bold text-brand-muted uppercase border-b border-border">
                Select Audio Output Device
              </div>
              {availableDevices.map((dev) => (
                <button
                  key={dev}
                  type="button"
                  onClick={() => handleSelectDevice(dev)}
                  className={`w-full text-left px-3 py-1.5 text-xs truncate transition-colors ${
                    config.virtual_mic_device_hint === dev || (vbcableDeviceName === dev && !config.virtual_mic_device_hint)
                      ? 'bg-zinc-100 text-brand-dark font-bold'
                      : 'text-brand-muted hover:bg-zinc-50 hover:text-brand-dark'
                  }`}
                >
                  {dev}
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Minimal Input Level VU Meter matching Figma node 15:88 */}
        <div className="py-3.5 border-b border-border">
          <VuMeter
            label="Input Level"
            peak={levels.mic_peak}
            rms={levels.mic_rms}
            muted={config.mic_muted || !config.phone_to_pc_enabled}
          />
        </div>

        {/* Sliders: Mic Boost & Voice Gate matching Figma node 15:91 & 15:96 */}
        <div className="py-3 border-b border-border flex flex-col gap-3">
          {/* Mic Boost Slider */}
          <div className="flex flex-col gap-1.5">
            <div className="flex items-center justify-between text-xs font-bold text-brand-dark">
              <span>Mic Boost</span>
              <span>{config.mic_gain_db.toFixed(1)} dB</span>
            </div>
            <input
              type="range"
              min="-12"
              max="18"
              step="0.5"
              value={config.mic_gain_db}
              onChange={handleGainChange}
              className="w-full"
            />
          </div>

          {/* Voice Gate Slider with Live Indicator Dot */}
          <div className="flex flex-col gap-1.5">
            <div className="flex items-center justify-between text-xs font-bold text-brand-dark">
              <div className="flex items-center gap-1.5">
                <span>Voice Gate</span>
                <span
                  className={`w-2 h-2 rounded-full transition-colors ${
                    isVoiceGateOpen && config.phone_to_pc_enabled && !config.mic_muted
                      ? 'bg-brand-green shadow-[0_0_6px_rgba(22,163,74,0.6)]'
                      : 'bg-zinc-300'
                  }`}
                  title={isVoiceGateOpen ? 'Voice Gate Open (Transmitting)' : 'Voice Gate Closed (Silent)'}
                />
              </div>
              <span>{Math.round(config.vad_gate_threshold * 100)}%</span>
            </div>
            <input
              type="range"
              min="0"
              max="0.8"
              step="0.01"
              value={config.vad_gate_threshold}
              onChange={handleVadChange}
              className="w-full"
            />
          </div>
        </div>

        {/* Push-to-Talk Row matching Figma node 15:102 */}
        <div className="py-3 border-b border-border flex items-center justify-between">
          <div>
            <div className="flex items-center gap-2">
              <span className="text-xs font-bold text-brand-dark">Push-to-Talk</span>
              <span className="px-2 py-0.5 rounded-md bg-[#F4F4F5] border border-border text-[10.5px] font-mono font-bold text-brand-dark">
                Ctrl+Shift+Space
              </span>
            </div>
            <p className="text-[11px] text-brand-muted mt-0.5">Hold hotkey to speak</p>
          </div>
          <button
            type="button"
            onClick={() => onUpdateConfig({ ...config, push_to_talk_enabled: !config.push_to_talk_enabled })}
            className={`clean-toggle ${config.push_to_talk_enabled ? 'active' : ''}`}
            aria-label="Toggle Push-to-Talk"
          >
            <div className="clean-toggle-thumb" />
          </button>
        </div>

        {/* DSP Toggles: Echo Cancellation & Noise Suppression matching Figma nodes 15:108 & 15:112 */}
        <div className="pt-3 flex flex-col gap-2.5">
          {/* Echo Cancellation */}
          <div className="flex items-center justify-between">
            <div>
              <span className="text-xs font-bold text-brand-dark">Echo Cancellation</span>
              <p className="text-[11px] text-brand-muted">Blocks PC speaker feedback</p>
            </div>
            <button
              type="button"
              onClick={() => onUpdateConfig({ ...config, aec_enabled: !config.aec_enabled })}
              className={`clean-toggle ${config.aec_enabled ? 'active' : ''}`}
              aria-label="Toggle Echo Cancellation"
            >
              <div className="clean-toggle-thumb" />
            </button>
          </div>

          {/* Noise Suppression */}
          <div className="flex items-center justify-between">
            <div>
              <span className="text-xs font-bold text-brand-dark">Noise Suppression</span>
              <p className="text-[11px] text-brand-muted">Reduces background room noise</p>
            </div>
            <button
              type="button"
              onClick={() => onUpdateConfig({ ...config, rnnoise_enabled: !config.rnnoise_enabled })}
              className={`clean-toggle ${config.rnnoise_enabled ? 'active' : ''}`}
              aria-label="Toggle Noise Suppression"
            >
              <div className="clean-toggle-thumb" />
            </button>
          </div>
        </div>
      </div>
    </div>
  );
};

export const MicAudioCard = React.memo(MicAudioCardComponent);
