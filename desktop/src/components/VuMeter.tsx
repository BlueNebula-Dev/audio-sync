import React from 'react';

interface VuMeterProps {
  label: string;
  sublabel?: string;
  peak: number; // 0.0 to 1.0+
  rms: number;  // 0.0 to 1.0+
  muted?: boolean;
  color?: 'black' | 'cyan' | 'violet' | 'emerald';
  segmentsCount?: number;
}

const VuMeterComponent: React.FC<VuMeterProps> = ({
  label,
  sublabel,
  peak,
  rms,
  muted = false,
  segmentsCount = 36,
}) => {
  // Convert peak & rms to normalized bar display (0 to 1)
  const effectivePeak = muted ? 0 : Math.min(Math.max(peak, 0), 1.2);
  const effectiveRms = muted ? 0 : Math.min(Math.max(rms, 0), 1.0);

  const activeSegments = Math.round(effectivePeak * segmentsCount);
  const rmsSegments = Math.round(effectiveRms * segmentsCount);

  // Peak in dB for readout
  const db = effectivePeak > 0.0001 ? (20 * Math.log10(effectivePeak)).toFixed(1) : '-inf';

  return (
    <div className="w-full flex flex-col gap-1.5">
      <div className="flex items-center justify-between text-xs">
        <div className="flex items-center gap-1.5">
          <span className="font-semibold text-brand-dark text-xs">{label}</span>
          {sublabel && <span className="text-brand-muted text-[11px]">({sublabel})</span>}
        </div>
        <div className="font-semibold text-[11.5px]">
          {muted ? (
            <span className="text-brand-red">MUTED</span>
          ) : (
            <span className="text-brand-muted">{db} dB</span>
          )}
        </div>
      </div>

      {/* Minimalist Segmented Meter Bar matching Figma */}
      <div className="h-2.5 w-full bg-[#E4E4E7] rounded-full p-[1.5px] flex items-center gap-[2px] overflow-hidden">
        {Array.from({ length: segmentsCount }).map((_, idx) => {
          const isActive = !muted && idx < activeSegments;
          const isRms = !muted && idx < rmsSegments;

          let segColor = '#18181B';
          if (idx / segmentsCount > 0.88) {
            segColor = '#DC2626'; // red clip
          } else if (idx / segmentsCount > 0.72) {
            segColor = '#D97706'; // amber headroom
          }

          return (
            <div
              key={idx}
              className="h-full flex-1 rounded-[1px]"
              style={{
                backgroundColor: isActive ? segColor : 'transparent',
                opacity: isActive ? (isRms ? 1.0 : 0.6) : 0,
              }}
            />
          );
        })}
      </div>
    </div>
  );
};

export const VuMeter = React.memo(VuMeterComponent);
