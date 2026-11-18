import AVKit
import SwiftUI

// MARK: - Native iOS System AirPlay / Bluetooth Route Picker Wrapper

struct SystemAudioRoutePicker: UIViewRepresentable {
    func makeUIView(context: Context) -> AVRoutePickerView {
        let picker = AVRoutePickerView()
        picker.tintColor = UIColor(red: 24 / 255, green: 24 / 255, blue: 27 / 255, alpha: 1.0)
        picker.activeTintColor = UIColor(red: 22 / 255, green: 163 / 255, blue: 74 / 255, alpha: 1.0)
        picker.prioritizesVideoDevices = false
        return picker
    }

    func updateUIView(_ uiView: AVRoutePickerView, context: Context) {}
}

// MARK: - Minimalist Horizontal Segmented VU Meter View

struct MinimalVuMeterView: View, Equatable {
    let title: String
    let peak: Float
    let rms: Float
    let isMuted: Bool
    private let segmentCount: Int = 32

    static func == (lhs: MinimalVuMeterView, rhs: MinimalVuMeterView) -> Bool {
        let lhsPeakSeg = Int((min(lhs.effectivePeak, 1.0) * Float(lhs.segmentCount)).rounded())
        let rhsPeakSeg = Int((min(rhs.effectivePeak, 1.0) * Float(rhs.segmentCount)).rounded())
        let lhsRmsSeg = Int((lhs.effectiveRms * Float(lhs.segmentCount)).rounded())
        let rhsRmsSeg = Int((rhs.effectiveRms * Float(rhs.segmentCount)).rounded())
        return lhs.title == rhs.title
            && lhs.isMuted == rhs.isMuted
            && lhsPeakSeg == rhsPeakSeg
            && lhsRmsSeg == rhsRmsSeg
    }

    private var effectivePeak: Float {
        isMuted ? 0.0 : min(max(peak, 0.0), 1.2)
    }

    private var effectiveRms: Float {
        isMuted ? 0.0 : min(max(rms, 0.0), 1.0)
    }

    private var dbString: String {
        if isMuted { return "MUTED" }
        guard effectivePeak > 0.0001 else { return "-∞ dB" }
        return String(format: "%+.1f dB", 20.0 * log10(effectivePeak))
    }

    var body: some View {
        let activePeakSegments = Int((min(effectivePeak, 1.0) * Float(segmentCount)).rounded())
        let activeRmsSegments = Int((effectiveRms * Float(segmentCount)).rounded())

        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline) {
                Text(title)
                    .font(.system(size: 12.5, weight: .bold))
                    .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))

                Spacer()

                if isMuted {
                    Text("MUTED")
                        .font(.system(size: 11.5, weight: .bold, design: .monospaced))
                        .foregroundColor(Color(red: 220 / 255, green: 38 / 255, blue: 38 / 255))
                } else if effectivePeak > 0.0001 {
                    Text(dbString)
                        .font(.system(size: 11.5, weight: .semibold, design: .monospaced))
                        .foregroundColor(Color(red: 113 / 255, green: 113 / 255, blue: 122 / 255))
                } else {
                    HStack(spacing: 4) {
                        Circle()
                            .fill(Color(red: 22 / 255, green: 163 / 255, blue: 74 / 255))
                            .frame(width: 5, height: 5)
                        Text("Live")
                            .font(.system(size: 11.5, weight: .bold))
                            .foregroundColor(Color(red: 22 / 255, green: 163 / 255, blue: 74 / 255))
                    }
                }
            }

            // Minimalist horizontal bar matching Figma node 15:146
            HStack(spacing: 2) {
                ForEach(0..<segmentCount, id: \.self) { idx in
                    let isPeakLit = !isMuted && idx < activePeakSegments
                    let isRmsLit = !isMuted && idx < activeRmsSegments

                    let color: Color = (Double(idx) / Double(segmentCount) > 0.88)
                        ? Color(red: 220 / 255, green: 38 / 255, blue: 38 / 255) // red
                        : ((Double(idx) / Double(segmentCount) > 0.72)
                            ? Color(red: 217 / 255, green: 119 / 255, blue: 6 / 255) // amber
                            : Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255)) // black

                    RoundedRectangle(cornerRadius: 1)
                        .fill(isPeakLit ? color : Color.clear)
                        .opacity(isRmsLit ? 1.0 : (isPeakLit ? 0.65 : 0))
                        .frame(height: 7)
                }
            }
            .padding(2)
            .background(Color(red: 228 / 255, green: 228 / 255, blue: 231 / 255))
            .clipShape(Capsule())
        }
    }
}

// MARK: - Main ContentView (Functional Minimalism Redesign matching Figma node 15:117)

struct ContentView: View {
    @ObservedObject var networkClient: NetworkClient
    @ObservedObject var audioManager: AudioEngineManager
    @State private var showManualIpSheet: Bool = false
    @State private var showSettingsSheet: Bool = false

    private var packetLossPercentage: Double {
        let total = networkClient.packetsReceived + networkClient.packetsLost
        guard total > 0 else { return 0.0 }
        return (Double(networkClient.packetsLost) / Double(total)) * 100.0
    }

    var body: some View {
        ZStack {
            // Clean bright canvas (#FFFFFF) matching Figma
            Color.white.ignoresSafeArea()

            ScrollView(.vertical, showsIndicators: false) {
                VStack(spacing: 14) {
                    mobileTopBar
                    cardConnectionStatus
                    cardIPhoneMic
                    cardPCAudioOut
                    bottomDisconnectButton
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
            }
        }
        .sheet(isPresented: $showManualIpSheet) {
            manualIpSheet
        }
        .sheet(isPresented: $showSettingsSheet) {
            advancedSettingsSheet
        }
    }

    // MARK: - 1. Mobile Top Bar matching Figma node 15:119

    private var mobileTopBar: some View {
        HStack {
            HStack(spacing: 8) {
                // Waveform app icon
                ZStack {
                    RoundedRectangle(cornerRadius: 8)
                        .fill(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                        .frame(width: 28, height: 28)
                    Image(systemName: "waveform")
                        .font(.system(size: 13, weight: .bold))
                        .foregroundColor(.white)
                }

                Text("AudioSync")
                    .font(.system(size: 16, weight: .bold))
                    .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
            }

            Spacer()

            Button(action: {
                showManualIpSheet = true
            }) {
                Text("Manual IP")
                    .font(.system(size: 12, weight: .bold))
                    .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .background(Color(red: 244 / 255, green: 244 / 255, blue: 245 / 255))
                    .clipShape(Capsule())
                    .overlay(
                        Capsule()
                            .stroke(Color(red: 228 / 255, green: 228 / 255, blue: 231 / 255), lineWidth: 1)
                    )
            }
        }
        .padding(.vertical, 4)
    }

    // MARK: - 2. Card Connection Status matching Figma node 15:127

    private var cardConnectionStatus: some View {
        VStack(spacing: 0) {
            HStack(spacing: 12) {
                // Device Icon
                ZStack {
                    RoundedRectangle(cornerRadius: 12)
                        .fill(Color.white)
                        .frame(width: 38, height: 38)
                        .shadow(color: Color.black.opacity(0.04), radius: 2, x: 0, y: 1)
                    Image(systemName: "desktopcomputer")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                }

                VStack(alignment: .leading, spacing: 2) {
                    Text(networkClient.connectedServerName.isEmpty || networkClient.connectedServerName == "Not Connected" ? "DESKTOP-PC" : networkClient.connectedServerName)
                        .font(.system(size: 14, weight: .bold))
                        .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))

                    Text(networkClient.isConnected ? "Connected · \(networkClient.manualHost)" : networkClient.connectionStateText)
                        .font(.system(size: 12))
                        .foregroundColor(Color(red: 113 / 255, green: 113 / 255, blue: 122 / 255))
                        .lineLimit(1)
                }

                Spacer()

                // RTT latency badge & Transport selector menu
                HStack(spacing: 6) {
                    if networkClient.isConnected {
                        Text(String(format: "%.1f ms", networkClient.rttMilliseconds))
                            .font(.system(size: 11, weight: .bold, design: .monospaced))
                            .foregroundColor(Color(red: 22 / 255, green: 163 / 255, blue: 74 / 255))
                            .padding(.horizontal, 7)
                            .padding(.vertical, 4)
                            .background(Color(red: 22 / 255, green: 163 / 255, blue: 74 / 255).opacity(0.1))
                            .clipShape(Capsule())
                    }

                    Menu {
                        ForEach(TransportMode.allCases) { mode in
                            Button(action: {
                                networkClient.transportMode = mode
                            }) {
                                HStack {
                                    Text(mode.rawValue)
                                    if networkClient.transportMode == mode {
                                        Image(systemName: "checkmark")
                                    }
                                }
                            }
                        }
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: networkClient.activeTransportLabel.contains("USB") ? "cable.connector" : "wifi")
                                .font(.system(size: 10))
                            Text(networkClient.activeTransportLabel.contains("USB") ? "USB" : "Wi-Fi")
                                .font(.system(size: 11.5, weight: .bold))
                            Image(systemName: "chevron.down")
                                .font(.system(size: 8, weight: .bold))
                        }
                        .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                        .padding(.horizontal, 9)
                        .padding(.vertical, 5)
                        .background(Color.white)
                        .clipShape(Capsule())
                        .shadow(color: Color.black.opacity(0.04), radius: 2, x: 0, y: 1)
                    }
                }
            }
            .padding(14)
        }
        .background(Color(red: 244 / 255, green: 244 / 255, blue: 245 / 255))
        .cornerRadius(18)
        .overlay(
            RoundedRectangle(cornerRadius: 18)
                .stroke(Color(red: 228 / 255, green: 228 / 255, blue: 231 / 255), lineWidth: 1)
        )
    }

    // MARK: - 3. Card iPhone Mic matching Figma node 15:135

    private var cardIPhoneMic: some View {
        VStack(spacing: 12) {
            // Header with Power Switch
            HStack {
                ZStack {
                    RoundedRectangle(cornerRadius: 12)
                        .fill(Color.white)
                        .frame(width: 38, height: 38)
                        .shadow(color: Color.black.opacity(0.04), radius: 2, x: 0, y: 1)
                    Image(systemName: "mic.fill")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                }

                VStack(alignment: .leading, spacing: 2) {
                    Text("iPhone Microphone")
                        .font(.system(size: 15, weight: .bold))
                        .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                    Text("Sending voice to Windows")
                        .font(.system(size: 12))
                        .foregroundColor(Color(red: 113 / 255, green: 113 / 255, blue: 122 / 255))
                }

                Spacer()

                Toggle("", isOn: $audioManager.phoneToPcEnabled)
                    .labelsHidden()
                    .tint(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
            }

            Divider().background(Color(red: 228 / 255, green: 228 / 255, blue: 231 / 255))

            // Minimalist Mic VU Meter
            MinimalVuMeterView(
                title: "Mic Signal",
                peak: audioManager.micPeakLevel,
                rms: audioManager.micRmsLevel,
                isMuted: audioManager.isMicMuted || !audioManager.phoneToPcEnabled
            )
            .equatable()

            // Input Source Row with Call Beamforming option
            HStack {
                Text("Input Source")
                    .font(.system(size: 12))
                    .foregroundColor(Color(red: 113 / 255, green: 113 / 255, blue: 122 / 255))

                Spacer()

                Menu {
                    Button(action: {
                        audioManager.iosVoiceProcessingEnabled.toggle()
                    }) {
                        HStack {
                            Text(audioManager.iosVoiceProcessingEnabled ? "Voice Isolation: ON (Beamforming)" : "Voice Isolation: OFF (Wideband)")
                            if audioManager.iosVoiceProcessingEnabled {
                                Image(systemName: "checkmark")
                            }
                        }
                    }

                    Divider()

                    Button(action: {}) {
                        Text("Default Hardware Mic Array")
                    }
                } label: {
                    HStack(spacing: 4) {
                        Text(audioManager.iosVoiceProcessingEnabled ? "Beamforming Mic" : "Bottom Mic")
                            .font(.system(size: 12.5, weight: .bold))
                            .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                        Image(systemName: "chevron.down")
                            .font(.system(size: 9, weight: .bold))
                            .foregroundColor(Color(red: 113 / 255, green: 113 / 255, blue: 122 / 255))
                    }
                }
            }
            .padding(.top, 2)

            // Prominent Full-Width Mute Button matching Figma node 15:166
            Button(action: {
                audioManager.isMicMuted.toggle()
            }) {
                HStack(spacing: 8) {
                    Image(systemName: audioManager.isMicMuted ? "mic.slash.fill" : "mic.fill")
                        .font(.system(size: 14, weight: .bold))
                    Text(audioManager.isMicMuted ? "Microphone Muted" : "Mute Microphone")
                        .font(.system(size: 14, weight: .bold))
                }
                .foregroundColor(.white)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 13)
                .background(
                    !audioManager.phoneToPcEnabled
                        ? Color(red: 161 / 255, green: 161 / 255, blue: 170 / 255)
                        : (audioManager.isMicMuted ? Color(red: 220 / 255, green: 38 / 255, blue: 38 / 255) : Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                )
                .clipShape(Capsule())
            }
            .disabled(!audioManager.phoneToPcEnabled)

            // Bottom Caption matching Figma node 15:170
            Text("Echo cancellation & noise filter managed by PC")
                .font(.system(size: 11))
                .foregroundColor(Color(red: 113 / 255, green: 113 / 255, blue: 122 / 255))
                .padding(.top, -2)
        }
        .padding(16)
        .background(Color(red: 244 / 255, green: 244 / 255, blue: 245 / 255))
        .cornerRadius(20)
        .overlay(
            RoundedRectangle(cornerRadius: 20)
                .stroke(Color(red: 228 / 255, green: 228 / 255, blue: 231 / 255), lineWidth: 1)
        )
    }

    // MARK: - 4. Card PC Audio Out matching Figma node 15:171

    private var cardPCAudioOut: some View {
        VStack(spacing: 12) {
            // Header with Power Switch
            HStack {
                ZStack {
                    RoundedRectangle(cornerRadius: 12)
                        .fill(Color.white)
                        .frame(width: 38, height: 38)
                        .shadow(color: Color.black.opacity(0.04), radius: 2, x: 0, y: 1)
                    Image(systemName: "speaker.wave.2.fill")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                }

                VStack(alignment: .leading, spacing: 2) {
                    Text("PC Audio Output")
                        .font(.system(size: 15, weight: .bold))
                        .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                    Text("Playing Windows sound")
                        .font(.system(size: 12))
                        .foregroundColor(Color(red: 113 / 255, green: 113 / 255, blue: 122 / 255))
                }

                Spacer()

                Toggle("", isOn: $audioManager.pcToPhoneEnabled)
                    .labelsHidden()
                    .tint(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
            }

            Divider().background(Color(red: 228 / 255, green: 228 / 255, blue: 231 / 255))

            // Playback Volume Slider matching Figma node 15:179
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text("Playback Volume")
                        .font(.system(size: 12.5, weight: .bold))
                        .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                    Spacer()
                    Text("\(Int((audioManager.speakerVolume * 100).rounded()))%")
                        .font(.system(size: 12.5, weight: .bold))
                        .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                }

                Slider(value: $audioManager.speakerVolume, in: 0.0...1.5, step: 0.05)
                    .tint(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
            }

            Divider().background(Color(red: 228 / 255, green: 228 / 255, blue: 231 / 255))

            // Audio Route Row matching Figma node 15:185
            HStack {
                Text("Audio Route")
                    .font(.system(size: 12))
                    .foregroundColor(Color(red: 113 / 255, green: 113 / 255, blue: 122 / 255))

                Spacer()

                HStack(spacing: 8) {
                    Menu {
                        ForEach(AudioOutputRoute.allCases) { route in
                            Button(action: {
                                audioManager.selectedOutputRoute = route
                            }) {
                                HStack {
                                    Text(route.rawValue)
                                    if audioManager.selectedOutputRoute == route {
                                        Image(systemName: "checkmark")
                                    }
                                }
                            }
                        }
                    } label: {
                        HStack(spacing: 5) {
                            Image(systemName: audioManager.selectedOutputRoute == .bluetooth ? "airpodspro" : (audioManager.selectedOutputRoute == .earpiece ? "iphone" : "speaker.wave.3.fill"))
                                .font(.system(size: 11))
                            Text(audioManager.selectedOutputRoute.rawValue)
                                .font(.system(size: 12, weight: .bold))
                            Image(systemName: "chevron.down")
                                .font(.system(size: 8, weight: .bold))
                        }
                        .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                        .padding(.horizontal, 10)
                        .padding(.vertical, 5)
                        .background(Color.white)
                        .clipShape(Capsule())
                        .shadow(color: Color.black.opacity(0.04), radius: 2, x: 0, y: 1)
                    }

                    // Native system AirPlay picker
                    SystemAudioRoutePicker()
                        .frame(width: 24, height: 24)
                }
            }
        }
        .padding(16)
        .background(Color(red: 244 / 255, green: 244 / 255, blue: 245 / 255))
        .cornerRadius(20)
        .overlay(
            RoundedRectangle(cornerRadius: 20)
                .stroke(Color(red: 228 / 255, green: 228 / 255, blue: 231 / 255), lineWidth: 1)
        )
    }

    // MARK: - 5. Bottom Disconnect Button matching Figma node 15:188

    private var bottomDisconnectButton: some View {
        Button(action: {
            if networkClient.isConnected {
                networkClient.disconnect()
            } else {
                networkClient.connectToManualEndpoint()
            }
        }) {
            Text(networkClient.isConnected ? "Disconnect from PC" : "Connect to PC")
                .font(.system(size: 13.5, weight: .bold))
                .foregroundColor(networkClient.isConnected ? Color(red: 220 / 255, green: 38 / 255, blue: 38 / 255) : Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                .padding(.vertical, 10)
        }
        .padding(.top, 4)
    }

    // MARK: - Manual IP Modal Sheet

    private var manualIpSheet: some View {
        NavigationView {
            VStack(alignment: .leading, spacing: 18) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("MANUAL IP ADDRESS & PORT")
                        .font(.system(size: 11, weight: .bold))
                        .foregroundColor(Color(red: 113 / 255, green: 113 / 255, blue: 122 / 255))

                    HStack(spacing: 8) {
                        TextField("192.168.1.13", text: $networkClient.manualHost)
                            .font(.system(size: 14, design: .monospaced))
                            .padding(10)
                            .background(Color(red: 244 / 255, green: 244 / 255, blue: 245 / 255))
                            .cornerRadius(12)
                            .keyboardType(.numbersAndPunctuation)

                        TextField("54321", text: $networkClient.manualPort)
                            .font(.system(size: 14, design: .monospaced))
                            .frame(width: 80)
                            .padding(10)
                            .background(Color(red: 244 / 255, green: 244 / 255, blue: 245 / 255))
                            .cornerRadius(12)
                            .keyboardType(.numberPad)
                    }

                    Button(action: {
                        networkClient.connectToManualEndpoint()
                        showManualIpSheet = false
                    }) {
                        Text("Pair and Connect")
                            .font(.system(size: 13.5, weight: .bold))
                            .foregroundColor(.white)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 12)
                            .background(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                            .cornerRadius(12)
                    }
                }

                if !networkClient.discoveredHosts.isEmpty {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("DISCOVERED BONJOUR PCS")
                            .font(.system(size: 11, weight: .bold))
                            .foregroundColor(Color(red: 113 / 255, green: 113 / 255, blue: 122 / 255))

                        ForEach(networkClient.discoveredHosts) { host in
                            Button(action: {
                                networkClient.connect(to: host)
                                showManualIpSheet = false
                            }) {
                                HStack {
                                    Image(systemName: "desktopcomputer")
                                    Text(host.name)
                                        .font(.system(size: 13, weight: .semibold))
                                    Spacer()
                                    Text("Connect")
                                        .font(.system(size: 12, weight: .bold))
                                }
                                .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                                .padding(12)
                                .background(Color(red: 244 / 255, green: 244 / 255, blue: 245 / 255))
                                .cornerRadius(12)
                            }
                        }
                    }
                }

                VStack(alignment: .leading, spacing: 6) {
                    Text("CONNECTION TELEMETRY")
                        .font(.system(size: 11, weight: .bold))
                        .foregroundColor(Color(red: 113 / 255, green: 113 / 255, blue: 122 / 255))

                    HStack(spacing: 8) {
                        telemetryTile(label: "Packets In/Out", value: "\(networkClient.packetsReceived) / \(networkClient.packetsSent)")
                        telemetryTile(label: "Packets Lost", value: "\(networkClient.packetsLost) (\(String(format: "%.1f%%", packetLossPercentage)))", highlightWarning: networkClient.packetsLost > 0)
                    }
                }

                Spacer()
            }
            .padding(18)
            .navigationTitle("Network Pairing")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .navigationBarTrailing) {
                    Button("Done") {
                        showManualIpSheet = false
                    }
                    .font(.system(size: 14, weight: .bold))
                    .foregroundColor(Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
                }
            }
        }
    }

    // MARK: - Advanced Tuning Sheet

    private var advancedSettingsSheet: some View {
        NavigationView {
            Form {
                Section(header: Text("Audio Latency & Buffer")) {
                    Picker("Frame Duration", selection: $audioManager.frameDurationMs) {
                        Text("10 ms (Low Latency)").tag(10)
                        Text("20 ms (Stable)").tag(20)
                    }

                    Stepper(value: $audioManager.jitterBufferDepth, in: 2...20) {
                        HStack {
                            Text("Jitter Buffer")
                            Spacer()
                            Text("\(audioManager.jitterBufferDepth * 10) ms (\(audioManager.jitterBufferDepth) pkts)")
                                .foregroundColor(.secondary)
                        }
                    }
                }

                Section(header: Text("Microphone Hardware Mode")) {
                    Toggle("iOS Call Beamforming (.voiceChat)", isOn: $audioManager.iosVoiceProcessingEnabled)
                }
            }
            .navigationTitle("Audio Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .navigationBarTrailing) {
                    Button("Done") {
                        showSettingsSheet = false
                    }
                }
            }
        }
    }

    private func telemetryTile(label: String, value: String, highlightWarning: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(label)
                .font(.system(size: 10, weight: .bold))
                .foregroundColor(Color(red: 113 / 255, green: 113 / 255, blue: 122 / 255))
            Text(value)
                .font(.system(size: 13, weight: .semibold, design: .monospaced))
                .foregroundColor(highlightWarning ? Color(red: 220 / 255, green: 38 / 255, blue: 38 / 255) : Color(red: 24 / 255, green: 24 / 255, blue: 27 / 255))
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(10)
        .background(Color(red: 244 / 255, green: 244 / 255, blue: 245 / 255))
        .cornerRadius(10)
    }
}
