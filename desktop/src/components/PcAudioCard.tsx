import React, { useState } from 'react';
import { HeadphoneIcon, Volume2Icon, VolumeXIcon } from './Icons';
import { VuMeter } from './VuMeter';
import { AudioConfig, AudioLevels } from '../types';

interface PcAudioCardProps {
  config: AudioConfig;
  levels: AudioLevels;
  onToggleMute: () => void;
  onUpdateConfig: (newConfig: AudioConfig) => void;
}

const BITRATE_OPTIONS = [
  { value: 32000, label: 'Low Bandwidth (32 kbps)' },
  { value: 64000, label: 'Balanced (64 kbps)' },
  { value: 96000, label: 'High Quality (96 kbps)' },
  { value: 128000, label: 'Studio (128 kbps)' },
];

const PcAudioCardComponent: React.FC<PcAudioCardProps> = ({
  config,
  levels,
  onToggleMute,
  onUpdateConfig,
}) => {
  const [showQualityMenu, setShowQualityMenu] = useState(false);

  const handleVolumeChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const val = parseFloat(e.target.value);
    onUpdateConfig({ ...config, pc_volume: val });
  };

  const handleLatencyChange = (durationMs: number) => {
    onUpdateConfig({ ...config, frame_duration_ms: durationMs });
  };

  const handleBitrateSelect = (bps: number) => {
    onUpdateConfig({ ...config, opus_bitrate_bps: bps });
    setShowQualityMenu(false);
  };

  const currentBitrateLabel =
    BITRATE_OPTIONS.find((b) => b.value === config.opus_bitrate_bps)?.label ||
    `${Math.round(config.opus_bitrate_bps / 1000)} kbps`;

  return (
    <div className="clean-panel p-6 flex flex-col justify-between relative bg-white">
      {/* Header Section matching Figma node 15:21 */}
      <div>
        <div className="flex items-center justify-between pb-4 border-b border-border">
          <div className="flex items-center gap-3">
            <div className="w-10 h-10 rounded-xl bg-zinc-100 border border-border flex items-center justify-center text-brand-dark">
              <HeadphoneIcon size={20} />
            </div>
            <div>
              <h2 className="text-sm font-bold text-brand-dark">PC Audio to iPhone</h2>
              <p className="text-[11.5px] text-brand-muted">Stream Windows sound to phone</p>
            </div>
          </div>

          <div className="flex items-center gap-2">
            {/* Mute Button */}
            <button
              type="button"
              onClick={onToggleMute}
              disabled={!config.pc_to_phone_enabled}
              className={`px-3 py-1.5 rounded-lg text-xs font-bold transition-all border ${
                !config.pc_to_phone_enabled
                  ? 'opacity-40 text-zinc-400 border-border cursor-not-allowed bg-zinc-50'
                  : config.pc_muted
                  ? 'bg-red-50 text-brand-red border-red-200'
                  : 'bg-white text-brand-dark border-border hover:bg-zinc-50'
              }`}
            >
              {config.pc_muted ? 'Muted' : 'Mute'}
            </button>

            {/* Stream Power Switch */}
            <button
              type="button"
              onClick={() => onUpdateConfig({ ...config, pc_to_phone_enabled: !config.pc_to_phone_enabled })}
              className={`clean-toggle ${config.pc_to_phone_enabled ? 'active' : ''}`}
              title="Stream Power ON / OFF"
              aria-label="Toggle PC Audio Stream Power"
            >
              <div className="clean-toggle-thumb" />
            </button>
          </div>
        </div>

        {/* Minimal Output Level VU Meter matching Figma node 15:31 */}
        <div className="py-4 border-b border-border">
          <VuMeter
            label="Output Level"
            peak={levels.pc_peak}
            rms={levels.pc_rms}
            muted={config.pc_muted || !config.pc_to_phone_enabled}
          />
        </div>

        {/* Volume Slider matching Figma node 15:54 */}
        <div className="py-4 border-b border-border flex flex-col gap-2">
          <div className="flex items-center justify-between text-xs font-bold text-brand-dark">
            <span>Volume</span>
            <span>{Math.round(config.pc_volume * 100)}%</span>
          </div>
          <input
            type="range"
            min="0"
            max="1.5"
            step="0.01"
            value={config.pc_volume}
            onChange={handleVolumeChange}
            className="w-full"
          />
        </div>

        {/* Latency & Quality Row matching Figma nodes 15:60 - 15:68 */}
        <div className="py-4 grid grid-cols-2 gap-4">
          {/* Latency Selection */}
          <div className="flex flex-col gap-2">
            <span className="text-[11px] font-bold text-brand-muted uppercase tracking-wider">Latency</span>
            <div className="flex items-center bg-[#F4F4F5] p-1 rounded-xl border border-border">
              <button
                type="button"
                onClick={() => handleLatencyChange(10)}
                className={`flex-1 py-1.5 rounded-lg text-[11.5px] transition-all ${
                  config.frame_duration_ms === 10
                    ? 'bg-white text-brand-dark font-bold shadow-sm'
                    : 'text-brand-muted hover:text-brand-dark font-normal'
                }`}
              >
                Low (10ms)
              </button>
              <button
                type="button"
                onClick={() => handleLatencyChange(20)}
                className={`flex-1 py-1.5 rounded-lg text-[11.5px] transition-all ${
                  config.frame_duration_ms === 20
                    ? 'bg-white text-brand-dark font-bold shadow-sm'
                    : 'text-brand-muted hover:text-brand-dark font-normal'
                }`}
              >
                Stable (20ms)
              </button>
            </div>
          </div>

          {/* Quality Dropdown */}
          <div className="flex flex-col gap-2 relative">
            <span className="text-[11px] font-bold text-brand-muted uppercase tracking-wider">Quality</span>
            <button
              type="button"
              onClick={() => setShowQualityMenu(!showQualityMenu)}
              className="flex items-center justify-between bg-[#F4F4F5] px-3 py-2 rounded-xl border border-border text-xs text-brand-dark font-normal hover:bg-zinc-200/60 transition-colors"
            >
              <span className="truncate">{currentBitrateLabel}</span>
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="text-brand-muted shrink-0 ml-1">
                <path d="m6 9 6 6 6-6" />
              </svg>
            </button>

            {showQualityMenu && (
              <div className="absolute top-full left-0 right-0 mt-1 bg-white border border-border rounded-xl shadow-modal z-20 py-1 overflow-hidden animate-popover">
                {BITRATE_OPTIONS.map((opt) => (
                  <button
                    key={opt.value}
                    type="button"
                    onClick={() => handleBitrateSelect(opt.value)}
                    className={`w-full text-left px-3 py-1.5 text-xs transition-colors flex items-center justify-between ${
                      config.opus_bitrate_bps === opt.value
                        ? 'bg-zinc-100 text-brand-dark font-bold'
                        : 'text-brand-muted hover:bg-zinc-50 hover:text-brand-dark'
                    }`}
                  >
                    <span>{opt.label}</span>
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Footer Info note matching Figma node 15:71 */}
      <div className="pt-3 border-t border-border flex items-center gap-2 text-[11.5px] text-brand-muted">
        <span className="w-1.5 h-1.5 rounded-full bg-brand-green shrink-0" />
        <span>Auto-discovery ready on local network</span>
      </div>
    </div>
  );
};

export const PcAudioCard = React.memo(PcAudioCardComponent);
