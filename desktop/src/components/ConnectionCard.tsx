import React, { useState } from 'react';
import { WifiIcon, ActivityIcon, CheckIcon, XIcon, SmartphoneIcon, CableIcon } from './Icons';
import { NetworkStatus, TransportMode } from '../types';

interface ConnectionCardProps {
  network: NetworkStatus;
  onSetManualPeer: (ipAndPort: string) => void;
  onSetTransportMode: (mode: TransportMode) => void;
}

const TRANSPORT_OPTIONS: { mode: TransportMode; label: string }[] = [
  { mode: 'auto', label: 'Auto' },
  { mode: 'wifi_udp', label: 'Wi-Fi' },
  { mode: 'usb_cable', label: 'USB' },
];

const ConnectionCardComponent: React.FC<ConnectionCardProps> = ({
  network,
  onSetManualPeer,
  onSetTransportMode,
}) => {
  const [manualIp, setManualIp] = useState('');
  const [showManualModal, setShowManualModal] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const handleManualSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!manualIp.trim()) return;
    try {
      const formatted = manualIp.includes(':') ? manualIp.trim() : `${manualIp.trim()}:${network.udp_port || 54321}`;
      onSetManualPeer(formatted);
      setShowManualModal(false);
      setErrorMsg(null);
    } catch (err: unknown) {
      setErrorMsg(err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <>
      {/* Single-Row Focused Utility Bar matching Figma node 15:4 */}
      <div className="clean-panel px-5 py-3 flex items-center justify-between">
        {/* Left: App Logo + Title + Version */}
        <div className="flex items-center gap-3">
          <div className="w-8 h-8 rounded-xl bg-brand-dark text-white flex items-center justify-center shadow-sm">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M12 2v20M17 5v14M7 8v8M22 10v4M2 10v4" />
            </svg>
          </div>
          <div className="flex items-baseline gap-2">
            <span className="font-bold text-[15px] text-brand-dark tracking-tight">AudioSync</span>
            <span className="text-xs text-brand-muted font-normal">v1.0</span>
          </div>
        </div>

        {/* Center: Live Status Pill */}
        <div className="flex items-center">
          {network.connected ? (
            <div className="inline-flex items-center gap-2 px-3.5 py-1 rounded-full bg-emerald-50 border border-emerald-200 text-emerald-800 text-[11.5px] font-semibold">
              <span className="w-2 h-2 rounded-full bg-emerald-600 animate-pulse" />
              <span>
                Connected {network.peer_device_name ? `· ${network.peer_device_name}` : ''}
              </span>
              <span className="text-emerald-700 font-mono text-[11px] font-normal">
                ({network.rtt_ms.toFixed(1)} ms)
              </span>
            </div>
          ) : (
            <div className="inline-flex items-center gap-2 px-3.5 py-1 rounded-full bg-amber-50 border border-amber-200 text-amber-800 text-[11.5px] font-semibold">
              <span className="w-2 h-2 rounded-full bg-amber-600" />
              <span>Waiting for iPhone</span>
            </div>
          )}
        </div>

        {/* Right: Segmented Transport Controls & Manual IP Trigger */}
        <div className="flex items-center gap-3">
          {/* Segmented Transport Selector matching Figma node 15:14 */}
          <div className="flex items-center bg-[#F4F4F5] p-1 rounded-xl border border-[#E4E4E7]">
            {TRANSPORT_OPTIONS.map((opt) => {
              const selected = network.transport_mode === opt.mode;
              return (
                <button
                  key={opt.mode}
                  type="button"
                  onClick={() => onSetTransportMode(opt.mode)}
                  className={`px-3 py-1 rounded-lg text-[11.5px] transition-all ${
                    selected
                      ? 'bg-white text-brand-dark font-bold shadow-sm'
                      : 'text-brand-muted hover:text-brand-dark font-normal'
                  }`}
                >
                  {opt.label}
                </button>
              );
            })}
          </div>

          {/* Manual IP Button */}
          <button
            type="button"
            onClick={() => setShowManualModal(true)}
            className="clean-pill px-3.5 py-1.5 text-[11.5px] font-bold text-brand-dark hover:bg-zinc-100 flex items-center gap-1.5 shadow-sm"
          >
            <span>Manual IP</span>
          </button>
        </div>
      </div>

      {/* Manual IP & Network Diagnostics Modal / Drawer */}
      {showManualModal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 backdrop-blur-sm p-4">
          <div className="clean-panel max-w-md w-full p-6 flex flex-col gap-5 animate-popover shadow-modal bg-white">
            <div className="flex items-center justify-between pb-3 border-b border-border">
              <div className="flex items-center gap-2">
                <SmartphoneIcon size={18} className="text-brand-dark" />
                <h3 className="text-sm font-bold text-brand-dark">Network & Manual Pairing</h3>
              </div>
              <button
                type="button"
                onClick={() => setShowManualModal(false)}
                className="p-1 text-brand-muted hover:text-brand-dark rounded-lg hover:bg-zinc-100 transition-colors"
              >
                <XIcon size={18} />
              </button>
            </div>

            {/* Network Stats Grid */}
            <div className="grid grid-cols-2 gap-3 text-xs">
              <div className="bg-[#F4F4F5] p-3 rounded-xl border border-border">
                <div className="text-[10px] text-brand-muted uppercase font-semibold">PC Local IP</div>
                <div className="text-brand-dark mt-0.5 font-mono font-bold text-sm truncate">{network.local_ip}</div>
              </div>
              <div className="bg-[#F4F4F5] p-3 rounded-xl border border-border">
                <div className="text-[10px] text-brand-muted uppercase font-semibold">UDP Listen Port</div>
                <div className="text-brand-dark mt-0.5 font-mono font-bold text-sm">{network.udp_port}</div>
              </div>
              <div className="bg-[#F4F4F5] p-3 rounded-xl border border-border">
                <div className="text-[10px] text-brand-muted uppercase font-semibold">Packets In / Out</div>
                <div className="text-brand-dark mt-0.5 font-mono font-bold text-xs">
                  {network.packets_received} / {network.packets_sent}
                </div>
              </div>
              <div className="bg-[#F4F4F5] p-3 rounded-xl border border-border">
                <div className="text-[10px] text-brand-muted uppercase font-semibold">Packets Lost (PLC)</div>
                <div className={`mt-0.5 font-mono font-bold text-xs ${network.packets_lost > 0 ? 'text-brand-red' : 'text-brand-dark'}`}>
                  {network.packets_lost}
                </div>
              </div>
            </div>

            {/* USB Mux Status */}
            <div className="flex items-center gap-2 px-3 py-2 rounded-xl bg-[#F4F4F5] border border-border text-xs text-brand-muted">
              <CableIcon size={16} className="text-brand-dark shrink-0" />
              <span className="flex-1">
                {network.usbmux_available
                  ? (network.usb_device_name ? `USB Lightning: ${network.usb_device_name}` : 'USB tunnel service ready (127.0.0.1:27015)')
                  : 'Apple Mobile Device Service not detected'}
              </span>
              <span className="font-mono text-[10px] bg-white px-1.5 py-0.5 rounded border border-border">TCP :54322</span>
            </div>

            {/* Manual Form */}
            <form onSubmit={handleManualSubmit} className="flex flex-col gap-3">
              <label className="text-xs font-semibold text-brand-dark">
                Direct iPhone IP & Port Pairing:
              </label>
              <div className="flex items-center gap-2">
                <input
                  type="text"
                  placeholder="e.g. 192.168.1.50:54321"
                  value={manualIp}
                  onChange={(e) => setManualIp(e.target.value)}
                  className="flex-1 bg-[#F4F4F5] border border-border rounded-xl px-3.5 py-2 text-xs font-mono text-brand-dark placeholder:text-zinc-400 focus:outline-none focus:border-brand-dark focus:bg-white"
                />
                <button
                  type="submit"
                  className="bg-brand-dark text-white px-4 py-2 rounded-xl text-xs font-bold hover:bg-zinc-800 transition-colors"
                >
                  Connect
                </button>
              </div>
              {errorMsg && <div className="text-xs text-brand-red">{errorMsg}</div>}
              <p className="text-[11px] text-brand-muted">
                Tip: Enter your iPhone’s Wi-Fi IP address shown in the AudioSync iOS app to bypass local mDNS discovery.
              </p>
            </form>
          </div>
        </div>
      )}
    </>
  );
};

export const ConnectionCard = React.memo(ConnectionCardComponent);
