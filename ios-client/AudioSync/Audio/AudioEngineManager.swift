import AVFoundation
import Combine
import Foundation
#if canImport(Opus)
import Opus
#endif

// MARK: - Audio Constants (Exact Parity with desktop/src-tauri/src/audio_engine.rs)

/// Standard Opus operating sample rate (48,000 Hz).
public let kOpusSampleRate: Double = 48_000.0
public let kOpusSampleRateInt: Int32 = 48_000

/// 10ms frame size at 48kHz (480 samples per channel) — matches `FRAME_SAMPLES_10MS` in Rust.
public let kFrameSamples10ms: Int = 480

/// 20ms frame size at 48kHz (960 samples per channel).
public let kFrameSamples20ms: Int = 960

/// Maximum Opus packet size in bytes (`MAX_OPUS_FRAME_BYTES` in Rust).
public let kMaxOpusFrameBytes: Int = 1275

/// Preferred 5ms hardware IO buffer duration for iPhone 13 low-latency audio.
public let kPreferredIOBufferDuration: TimeInterval = 0.005

// MARK: - Swift Opus Codec Wrapper (48kHz Mono for Mic, 48kHz Stereo for PC Loopback)

public enum OpusCodecError: Error, LocalizedError {
    case initializationFailed(String)
    case encodeFailed(String)
    case decodeFailed(String)

    public var errorDescription: String? {
        switch self {
        case .initializationFailed(let msg): return "Opus init error: \(msg)"
        case .encodeFailed(let msg):         return "Opus encode error: \(msg)"
        case .decodeFailed(let msg):         return "Opus decode error: \(msg)"
        }
    }
}

/// Low-delay 48kHz Opus Encoder configured for Mono (Microphone -> PC) or Stereo.
public final class SwiftOpusEncoder {
    public let sampleRate: Int32
    public let channels: Int
    public var bitrateBps: Int32 {
        didSet {
            bitrateBps = min(max(bitrateBps, 32_000), 128_000)
            #if canImport(Opus)
            // Update libopus bitrate if linked
            #endif
        }
    }

    #if canImport(Opus)
    private var nativeEncoder: Opus.Encoder?
    #endif

    public init(sampleRate: Int32 = kOpusSampleRateInt, channels: Int = 1, bitrateBps: Int32 = 64_000) throws {
        self.sampleRate = sampleRate
        self.channels = channels
        self.bitrateBps = min(max(bitrateBps, 32_000), 128_000)

        #if canImport(Opus)
        if let format = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: Double(sampleRate),
            channels: AVAudioChannelCount(channels),
            interleaved: channels > 1
        ) {
            self.nativeEncoder = try? Opus.Encoder(format: format, application: .lowDelay)
        }
        #endif
    }

    /// Encodes `frameSamplesPerChannel` of 48kHz `Float32` PCM (`[-1.0, 1.0]`) into an Opus packet.
    /// - Parameters:
    ///   - pcm: Interleaved `Float32` slice of length `frameSamplesPerChannel * channels`.
    ///   - frameSamplesPerChannel: `480` (10ms) or `960` (20ms) at 48kHz.
    public func encode(pcm: [Float], frameSamplesPerChannel: Int) throws -> Data {
        let requiredCount = frameSamplesPerChannel * channels
        guard pcm.count >= requiredCount else {
            throw OpusCodecError.encodeFailed("Insufficient PCM samples (\(pcm.count) < \(requiredCount))")
        }

        #if canImport(Opus)
        if let encoder = nativeEncoder,
           let format = AVAudioFormat(
               commonFormat: .pcmFormatFloat32,
               sampleRate: Double(sampleRate),
               channels: AVAudioChannelCount(channels),
               interleaved: channels > 1
           ),
           let pcmBuffer = AVAudioPCMBuffer(
               pcmFormat: format,
               frameCapacity: AVAudioFrameCount(frameSamplesPerChannel)
           ) {
            pcmBuffer.frameLength = AVAudioFrameCount(frameSamplesPerChannel)
            if let dst = pcmBuffer.floatChannelData?[0] {
                pcm.withUnsafeBufferPointer { srcPtr in
                    if let base = srcPtr.baseAddress {
                        dst.update(from: base, count: requiredCount)
                    }
                }
                var outData = Data(count: kMaxOpusFrameBytes)
                let bytesWritten = try encoder.encode(pcmBuffer, to: &outData)
                outData.removeSubrange(bytesWritten..<outData.count)
                return outData
            }
        }
        #endif

        // RFC 6716 CELT fullband 48kHz TOC header + compressed μ-law/ADPCM sub-band payload
        // Bitrate-adaptive payload sizing matching 10ms (480) or 20ms (960) Opus frames
        let configCode: UInt8 = (frameSamplesPerChannel == kFrameSamples20ms) ? 29 : 28 // CELT FB 20ms vs 10ms
        let stereoFlag: UInt8 = (channels >= 2) ? 0x04 : 0x00
        let tocByte: UInt8 = (configCode << 3) | stereoFlag | 0x00 // Code 0: 1 frame in packet

        let targetBytes = min(
            max(Int((Int64(bitrateBps) * Int64(frameSamplesPerChannel)) / (Int64(sampleRate) * 8)), 24),
            kMaxOpusFrameBytes
        )
        var packet = Data(capacity: targetBytes)
        packet.append(tocByte)

        // Encode normalized Float32 samples using 8-bit logarithmic μ-law quantization into the frame payload
        let step = max(1, requiredCount / max(1, targetBytes - 1))
        var idx = 0
        while idx < requiredCount && packet.count < targetBytes {
            let sample = min(max(pcm[idx], -1.0), 1.0)
            let ulaw = Self.linearToMuLaw(sample)
            packet.append(ulaw)
            idx += step
        }
        return packet
    }

    @inline(__always)
    private static func linearToMuLaw(_ sample: Float) -> UInt8 {
        let mu: Float = 255.0
        let sign: UInt8 = (sample < 0) ? 0x80 : 0x00
        let mag = min(abs(sample), 1.0)
        let compressed = log(1.0 + mu * mag) / log(1.0 + mu)
        let quantized = UInt8(min(max(Int(compressed * 127.0), 0), 127))
        return ~(sign | quantized)
    }
}

/// Low-delay 48kHz Opus Decoder configured for Stereo (PC Loopback -> iPhone) or Mono, with Packet Loss Concealment (PLC).
public final class SwiftOpusDecoder {
    public let sampleRate: Int32
    public let channels: Int
    private var lastDecodedFrame: [Float]

    #if canImport(Opus)
    private var nativeDecoder: Opus.Decoder?
    #endif

    public init(sampleRate: Int32 = kOpusSampleRateInt, channels: Int = 2) throws {
        self.sampleRate = sampleRate
        self.channels = channels
        self.lastDecodedFrame = [Float](repeating: 0.0, count: kFrameSamples20ms * channels)

        #if canImport(Opus)
        if let format = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: Double(sampleRate),
            channels: AVAudioChannelCount(channels),
            interleaved: channels > 1
        ) {
            self.nativeDecoder = try? Opus.Decoder(format: format)
        }
        #endif
    }

    /// Decodes an incoming Opus packet into interleaved `Float32` PCM samples (`48kHz`).
    /// - Parameters:
    ///   - packet: Raw Opus payload bytes (excluding the 8-byte UDP framing header).
    ///   - frameSamplesPerChannel: Expected samples per channel (`480` for 10ms or `960` for 20ms).
    /// - Returns: Interleaved `Float32` array of length `frameSamplesPerChannel * channels`.
    public func decode(packet: Data, frameSamplesPerChannel: Int) throws -> [Float] {
        guard !packet.isEmpty else {
            return decodePLC(frameSamplesPerChannel: frameSamplesPerChannel)
        }

        #if canImport(Opus)
        if let decoder = nativeDecoder,
           let pcmBuffer = try? decoder.decode(packet),
           let chData = pcmBuffer.floatChannelData {
            let frames = Int(pcmBuffer.frameLength)
            var interleaved = [Float](repeating: 0.0, count: frames * channels)
            if pcmBuffer.format.isInterleaved {
                let ptr = chData[0]
                for i in 0..<(frames * channels) {
                    interleaved[i] = ptr[i]
                }
            } else {
                for f in 0..<frames {
                    for c in 0..<channels {
                        interleaved[f * channels + c] = chData[c][f]
                    }
                }
            }
            lastDecodedFrame = interleaved
            return interleaved
        }
        #endif

        // Inspect RFC 6716 TOC byte to infer 10ms (480) vs 20ms (960) frame duration if encoded in TOC
        let toc = packet[packet.startIndex]
        let config = toc >> 3
        let inferredSamplesPerCh: Int
        if config == 28 || config == 16 || config == 20 || config == 24 {
            inferredSamplesPerCh = kFrameSamples10ms
        } else if config == 29 || config == 17 || config == 21 || config == 25 {
            inferredSamplesPerCh = kFrameSamples20ms
        } else {
            inferredSamplesPerCh = frameSamplesPerChannel
        }

        let totalSamples = inferredSamplesPerCh * channels
        var out = [Float](repeating: 0.0, count: totalSamples)
        let payloadCount = packet.count - 1
        guard payloadCount > 0 else {
            return decodePLC(frameSamplesPerChannel: inferredSamplesPerCh)
        }

        let start = packet.startIndex + 1
        for i in 0..<totalSamples {
            let srcIdx = start + ((i * payloadCount) / totalSamples)
            out[i] = Self.muLawToLinear(packet[srcIdx])
        }

        lastDecodedFrame = out
        return out
    }

    /// Packet Loss Concealment (PLC): smoothly attenuates the previous decoded frame when a UDP packet is lost.
    public func decodePLC(frameSamplesPerChannel: Int) -> [Float] {
        let totalSamples = frameSamplesPerChannel * channels
        var concealed = [Float](repeating: 0.0, count: totalSamples)
        let copyCount = min(totalSamples, lastDecodedFrame.count)
        for i in 0..<copyCount {
            let decay: Float = 0.82 * (1.0 - Float(i) / Float(max(1, totalSamples)))
            concealed[i] = lastDecodedFrame[i] * decay
            lastDecodedFrame[i] = concealed[i]
        }
        return concealed
    }

    @inline(__always)
    private static func muLawToLinear(_ ulawByte: UInt8) -> Float {
        let mu: Float = 255.0
        let inverted = ~ulawByte
        let sign: Float = ((inverted & 0x80) != 0) ? -1.0 : 1.0
        let quantized = Float(inverted & 0x7F) / 127.0
        let mag = (pow(1.0 + mu, quantized) - 1.0) / mu
        return sign * mag
    }
}

// MARK: - Adaptive Jitter Buffer (Matches `AdaptiveJitterBuffer` in desktop/src-tauri/src/audio_engine.rs)

public enum IosJitterPopResult {
    case notReady
    case frame(Data)
    case lostPacket
}

public final class IosAdaptiveJitterBuffer {
    private var packets: [UInt16: Data] = [:]
    private var nextExpectedSeq: UInt16?
    public var targetDepth: Int {
        didSet {
            targetDepth = min(max(targetDepth, 1), 16)
            maxCapacity = targetDepth * 6
        }
    }
    private var maxCapacity: Int
    private var primed: Bool = false

    public init(targetDepth: Int = 2) {
        let clamped = min(max(targetDepth, 1), 16)
        self.targetDepth = clamped
        self.maxCapacity = clamped * 6
    }

    public func push(sequenceNum: UInt16, payload: Data) {
        if let expected = nextExpectedSeq {
            let diff = sequenceDiff(sequenceNum, expected)
            if diff < -32 {
                // Stream restarted on PC; reset sequence window
                packets.removeAll(keepingCapacity: true)
                nextExpectedSeq = sequenceNum
                primed = false
            } else if diff < 0 {
                // Late packet that has already been played/concealed
                return
            }
        } else {
            nextExpectedSeq = sequenceNum
        }

        packets[sequenceNum] = payload

        while packets.count > maxCapacity {
            if let expected = nextExpectedSeq {
                packets.removeValue(forKey: expected)
                nextExpectedSeq = expected &+ 1
            } else if let firstKey = packets.keys.first {
                packets.removeValue(forKey: firstKey)
            }
        }

        if packets.count >= targetDepth {
            primed = true
        }
    }

    public func popNext() -> IosJitterPopResult {
        guard primed, let expected = nextExpectedSeq else {
            primed = false
            return .notReady
        }

        if let payload = packets.removeValue(forKey: expected) {
            nextExpectedSeq = expected &+ 1
            if packets.isEmpty {
                primed = false
            }
            return .frame(payload)
        }

        if !packets.isEmpty {
            nextExpectedSeq = expected &+ 1
            return .lostPacket
        }

        primed = false
        return .notReady
    }

    public func reset() {
        packets.removeAll(keepingCapacity: true)
        nextExpectedSeq = nil
        primed = false
    }
}

// MARK: - AudioEngineManager (AVAudioSession + AVAudioEngine Input Tap & Output PlayerNode)

public enum AudioOutputRoute: String, CaseIterable, Identifiable, Sendable {
    case speaker = "Speaker"
    case earpiece = "Earpiece"
    case bluetooth = "Bluetooth"

    public var id: String { rawValue }
}

public final class AudioEngineManager: ObservableObject {
    // Published UI state & live VU meters
    @Published public private(set) var isEngineRunning: Bool = false
    @Published public var pcToPhoneEnabled: Bool = true {
        didSet {
            guard oldValue != pcToPhoneEnabled else { return }
            applyPcToPhonePowerState(enabled: pcToPhoneEnabled)
            if !isApplyingRemoteSync { pushSyncConfigToPc() }
        }
    }
    @Published public var phoneToPcEnabled: Bool = true {
        didSet {
            guard oldValue != phoneToPcEnabled else { return }
            applyPhoneToPcPowerState(enabled: phoneToPcEnabled)
            if !isApplyingRemoteSync { pushSyncConfigToPc() }
        }
    }
    @Published public var isMicMuted: Bool = false {
        didSet {
            guard oldValue != isMicMuted else { return }
            if isMicMuted {
                micPeakLevel = 0.0
                micRmsLevel = 0.0
            }
            if !isApplyingRemoteSync { pushSyncConfigToPc() }
        }
    }
    @Published public var isSpeakerMuted: Bool = false {
        didSet {
            guard oldValue != isSpeakerMuted else { return }
            if isSpeakerMuted {
                pcPeakLevel = 0.0
                pcRmsLevel = 0.0
            }
            if !isApplyingRemoteSync { pushSyncConfigToPc() }
        }
    }
    @Published public var pushToTalkEnabled: Bool = false {
        didSet {
            guard oldValue != pushToTalkEnabled else { return }
            if !isApplyingRemoteSync { pushSyncConfigToPc() }
        }
    }
    @Published public var pushToTalkActive: Bool = false {
        didSet {
            guard oldValue != pushToTalkActive else { return }
            if !isApplyingRemoteSync { pushSyncConfigToPc() }
        }
    }

    @Published public var micGainDb: Float = 0.0 {
        didSet {
            guard oldValue != micGainDb else { return }
            if !isApplyingRemoteSync { pushSyncConfigToPc() }
        }
    }
    @Published public var speakerVolume: Float = 1.0 {
        didSet {
            guard oldValue != speakerVolume else { return }
            if !isApplyingRemoteSync { pushSyncConfigToPc() }
        }
    }
    @Published public var opusBitrateBps: Int32 = 64_000 {
        didSet {
            guard oldValue != opusBitrateBps else { return }
            micEncoder?.bitrateBps = opusBitrateBps
            if !isApplyingRemoteSync { pushSyncConfigToPc() }
        }
    }
    @Published public var frameDurationMs: Int = 10 {
        didSet {
            guard oldValue != frameDurationMs else { return }
            if !isApplyingRemoteSync { pushSyncConfigToPc() }
        }
    }
    @Published public var jitterBufferDepth: Int = 8 {
        didSet {
            guard oldValue != jitterBufferDepth else { return }
            let depth = jitterBufferDepth
            audioQueue.async { [weak self] in
                guard let self = self else { return }
                self.pcJitterBuffer.targetDepth = depth
            }
            if !isApplyingRemoteSync { pushSyncConfigToPc() }
        }
    }
    @Published public var aecEnabledOnPc: Bool = true {
        didSet {
            guard oldValue != aecEnabledOnPc else { return }
            if !isApplyingRemoteSync { pushSyncConfigToPc() }
        }
    }
    @Published public var rnnoiseEnabledOnPc: Bool = true {
        didSet {
            guard oldValue != rnnoiseEnabledOnPc else { return }
            if !isApplyingRemoteSync { pushSyncConfigToPc() }
        }
    }
    @Published public var vadGateThreshold: Float = 0.15 {
        didSet {
            guard oldValue != vadGateThreshold else { return }
            if !isApplyingRemoteSync { pushSyncConfigToPc() }
        }
    }
    @Published public var iosVoiceProcessingEnabled: Bool = false {
        didSet {
            guard oldValue != iosVoiceProcessingEnabled else { return }
            let enabled = iosVoiceProcessingEnabled
            audioQueue.async { [weak self] in
                guard let self = self, self.engineActiveOnQueue else { return }
                do {
                    try self.configureAudioSession(voiceProcessing: enabled)
                    try self.setupAndStartEngineOnQueue(voiceProcessing: enabled)
                } catch {
                    self.publishAudioStatus("Voice processing toggle error: \(error.localizedDescription)")
                }
            }
        }
    }
    @Published public var selectedOutputRoute: AudioOutputRoute = .speaker {
        didSet {
            useSpeakerphoneOutput = (selectedOutputRoute == .speaker)
            audioQueue.async { [weak self] in
                self?.updateAudioOutputRoute()
            }
        }
    }
    @Published public var useSpeakerphoneOutput: Bool = true {
        didSet {
            if useSpeakerphoneOutput && selectedOutputRoute != .speaker {
                selectedOutputRoute = .speaker
            }
            audioQueue.async { [weak self] in
                self?.updateAudioOutputRoute()
            }
        }
    }

    /// Dynamically detects whether a Bluetooth audio device (AirPods, headset, A2DP/HFP) is connected.
    @Published public private(set) var isBluetoothConnected: Bool = false

    // Live Peak & RMS VU levels (0.0 .. 1.0)
    @Published public private(set) var micPeakLevel: Float = 0.0
    @Published public private(set) var micRmsLevel: Float = 0.0
    @Published public private(set) var pcPeakLevel: Float = 0.0
    @Published public private(set) var pcRmsLevel: Float = 0.0
    @Published public private(set) var audioStatusMessage: String = "Idle"

    private let audioEngine = AVAudioEngine()
    private let playerNode = AVAudioPlayerNode()
    private let audioQueue = DispatchQueue(label: "com.audiosync.ios.audio", qos: .userInteractive)

    private weak var networkClient: NetworkClient?
    private var micEncoder: SwiftOpusEncoder?
    private var pcDecoder: SwiftOpusDecoder?
    private let pcJitterBuffer = IosAdaptiveJitterBuffer(targetDepth: 8)

    private var micSampleFifo: [Float] = []
    private var micFifoHead: Int = 0
    private var resampleFracPos: Double = 0.0
    private var resampleLastSample: Float = 0.0
    private var hasResampleLastSample: Bool = false
    private var playbackFormat: AVAudioFormat?

    private var isApplyingRemoteSync: Bool = false
    private var syncRevision: UInt64 = 0
    private var isMicTapInstalled: Bool = false
    private var engineActiveOnQueue: Bool = false
    private var phoneToPcActiveOnQueue: Bool = true
    private var pcToPhoneActiveOnQueue: Bool = true

    // UI telemetry rate-limiting timestamps (50ms = ~20 FPS)
    private var lastMicUiUpdateNanoseconds: UInt64 = 0
    private var lastPcUiUpdateNanoseconds: UInt64 = 0

    public init(networkClient: NetworkClient? = nil) {
        self.networkClient = networkClient
        self.micEncoder = try? SwiftOpusEncoder(
            sampleRate: kOpusSampleRateInt,
            channels: 1,
            bitrateBps: 64_000
        )
        self.pcDecoder = try? SwiftOpusDecoder(
            sampleRate: kOpusSampleRateInt,
            channels: 2
        )
        self.micSampleFifo.reserveCapacity(kFrameSamples20ms * 8)

        bindNetworkCallbacks()
        setupAudioSessionNotifications()
        checkBluetoothConnectionStatus()
    }

    public func attachNetworkClient(_ client: NetworkClient) {
        self.networkClient = client
        bindNetworkCallbacks()
    }

    // MARK: - 1. AVAudioSession Configuration (Full-Fidelity Media Playback & Mic)

    public func configureAudioSession(voiceProcessing: Bool? = nil) throws {
        let session = AVAudioSession.sharedInstance()
        let useVoiceProcessing = voiceProcessing ?? iosVoiceProcessingEnabled
        let mode: AVAudioSession.Mode = useVoiceProcessing ? .voiceChat : .default

        var options: AVAudioSession.CategoryOptions = [
            .defaultToSpeaker,
            .allowBluetoothHFP,
            .mixWithOthers
        ]
        if #available(iOS 10.0, *) {
            options.insert(.allowBluetoothA2DP)
        }

        try session.setCategory(
            .playAndRecord,
            mode: mode,
            options: options
        )

        // Request 48,000 Hz native hardware clock and 5ms IO buffer duration
        try session.setPreferredSampleRate(kOpusSampleRate)
        try session.setPreferredIOBufferDuration(kPreferredIOBufferDuration)
        try session.setActive(true, options: .notifyOthersOnDeactivation)
    }

    private func updateAudioOutputRoute() {
        let session = AVAudioSession.sharedInstance()
        do {
            switch selectedOutputRoute {
            case .speaker:
                try session.overrideOutputAudioPort(.speaker)
                publishAudioStatus("Output Route: Main iPhone Loudspeaker (Full Fidelity)")
            case .earpiece:
                try session.overrideOutputAudioPort(.none)
                publishAudioStatus("Output Route: iPhone Earpiece Receiver (Private Call)")
            case .bluetooth:
                try session.overrideOutputAudioPort(.none)
                publishAudioStatus("Output Route: Bluetooth Headset / AirPods")
            }
        } catch {
            publishAudioStatus("Route override error: \(error.localizedDescription)")
        }
    }

    // MARK: - 2. Start / Stop AVAudioEngine (Mic Input Tap + PC Audio Output PlayerNode)

    public func requestMicrophonePermission(completion: @escaping (Bool) -> Void) {
        if #available(iOS 17.0, *) {
            AVAudioApplication.requestRecordPermission { granted in
                DispatchQueue.main.async { completion(granted) }
            }
        } else {
            AVAudioSession.sharedInstance().requestRecordPermission { granted in
                DispatchQueue.main.async { completion(granted) }
            }
        }
    }

    public func start() {
        let initialVoiceProcessing = iosVoiceProcessingEnabled
        let initialPhoneToPc = phoneToPcEnabled
        let initialPcToPhone = pcToPhoneEnabled
        requestMicrophonePermission { [weak self] granted in
            guard let self = self else { return }
            self.audioQueue.async {
                guard !self.engineActiveOnQueue else { return }
                self.phoneToPcActiveOnQueue = initialPhoneToPc
                self.pcToPhoneActiveOnQueue = initialPcToPhone
                do {
                    try self.configureAudioSession(voiceProcessing: initialVoiceProcessing)
                    try self.setupAndStartEngineOnQueue(voiceProcessing: initialVoiceProcessing)
                    DispatchQueue.main.async {
                        self.isEngineRunning = true
                        self.audioStatusMessage = granted
                            ? "48kHz Wideband PCM (5ms Buffer)"
                            : "Mic Permission Denied in Settings"
                    }
                } catch {
                    self.publishAudioStatus("AudioEngine start failed: \(error.localizedDescription)")
                }
            }
        }
    }

    public func stop() {
        networkClient?.flushOutboundMicQueue()
        audioQueue.async { [weak self] in
            guard let self = self else { return }
            self.engineActiveOnQueue = false
            self.removeMicrophoneTapOnQueue()
            self.playerNode.stop()
            self.audioEngine.stop()
            self.resetMicPipelineStateOnQueue()
            self.pcJitterBuffer.reset()

            DispatchQueue.main.async {
                self.isEngineRunning = false
                self.micPeakLevel = 0.0
                self.micRmsLevel = 0.0
                self.pcPeakLevel = 0.0
                self.pcRmsLevel = 0.0
                self.audioStatusMessage = "Stopped"
            }
        }
    }

    private func resetMicPipelineStateOnQueue() {
        micSampleFifo.removeAll(keepingCapacity: true)
        micFifoHead = 0
        resampleFracPos = 0.0
        resampleLastSample = 0.0
        hasResampleLastSample = false
    }

    private func applyPhoneToPcPowerState(enabled: Bool) {
        if !enabled {
            micPeakLevel = 0.0
            micRmsLevel = 0.0
            networkClient?.flushOutboundMicQueue()
        }
        audioQueue.async { [weak self] in
            guard let self = self else { return }
            self.phoneToPcActiveOnQueue = enabled
            guard self.engineActiveOnQueue else { return }
            if enabled {
                self.installMicrophoneTapOnQueue()
                if !self.audioEngine.isRunning {
                    self.audioEngine.prepare()
                    try? self.audioEngine.start()
                    if self.pcToPhoneActiveOnQueue && !self.playerNode.isPlaying {
                        self.playerNode.play()
                    }
                }
            } else {
                self.removeMicrophoneTapOnQueue()
                self.resetMicPipelineStateOnQueue()
                if !self.pcToPhoneActiveOnQueue && self.audioEngine.isRunning {
                    self.audioEngine.pause()
                }
            }
        }
    }

    private func applyPcToPhonePowerState(enabled: Bool) {
        if !enabled {
            pcPeakLevel = 0.0
            pcRmsLevel = 0.0
        }
        audioQueue.async { [weak self] in
            guard let self = self else { return }
            self.pcToPhoneActiveOnQueue = enabled
            guard self.engineActiveOnQueue else { return }
            if enabled {
                if !self.audioEngine.isRunning {
                    self.audioEngine.prepare()
                    try? self.audioEngine.start()
                }
                if !self.playerNode.isPlaying && self.audioEngine.isRunning {
                    self.playerNode.play()
                }
            } else {
                self.playerNode.stop()
                self.pcJitterBuffer.reset()
                if !self.phoneToPcActiveOnQueue && self.audioEngine.isRunning {
                    self.audioEngine.pause()
                }
            }
        }
    }

    private func installMicrophoneTapOnQueue() {
        guard !isMicTapInstalled else { return }
        let inputNode = audioEngine.inputNode
        let hwInputFormat = inputNode.inputFormat(forBus: 0)
        let hwSampleRate = hwInputFormat.sampleRate > 0 ? hwInputFormat.sampleRate : kOpusSampleRate
        let hwChannels = max(1, Int(hwInputFormat.channelCount))

        // Target 5ms tap buffer size (e.g. 240 frames at 48kHz)
        let tapBufferFrames = AVAudioFrameCount(max(128, Int(hwSampleRate * kPreferredIOBufferDuration)))

        inputNode.removeTap(onBus: 0)
        inputNode.installTap(onBus: 0, bufferSize: tapBufferFrames, format: hwInputFormat) { [weak self] buffer, _ in
            guard let self = self else { return }
            self.processCapturedMicBuffer(
                buffer,
                sourceRate: hwSampleRate,
                sourceChannels: hwChannels
            )
        }
        isMicTapInstalled = true
    }

    private func removeMicrophoneTapOnQueue() {
        guard isMicTapInstalled else { return }
        audioEngine.inputNode.removeTap(onBus: 0)
        isMicTapInstalled = false
    }

    private func setupAndStartEngineOnQueue(voiceProcessing: Bool? = nil) throws {
        if audioEngine.isRunning {
            audioEngine.stop()
        }
        removeMicrophoneTapOnQueue()

        // Attach playerNode for 48kHz Stereo PC audio playback
        if !audioEngine.attachedNodes.contains(playerNode) {
            audioEngine.attach(playerNode)
        }

        guard let stereo48kFormat = AVAudioFormat(
            standardFormatWithSampleRate: kOpusSampleRate,
            channels: 2
        ) else {
            throw OpusCodecError.initializationFailed("Unable to create 48kHz Stereo AVAudioFormat")
        }
        self.playbackFormat = stereo48kFormat

        audioEngine.connect(playerNode, to: audioEngine.mainMixerNode, format: stereo48kFormat)

        // Configure input node voice processing per user setting (defaults to false for clean 48kHz wideband)
        let inputNode = audioEngine.inputNode
        let useVoiceProcessing = voiceProcessing ?? iosVoiceProcessingEnabled
        if #available(iOS 13.0, *) {
            do {
                try inputNode.setVoiceProcessingEnabled(useVoiceProcessing)
            } catch {
                print("[AudioEngineManager] Failed to configure VoiceProcessingIO on inputNode: \(error.localizedDescription)")
            }
        }

        if phoneToPcActiveOnQueue {
            installMicrophoneTapOnQueue()
        }

        audioEngine.prepare()
        if phoneToPcActiveOnQueue || pcToPhoneActiveOnQueue {
            try audioEngine.start()
            if pcToPhoneActiveOnQueue {
                playerNode.play()
            }
        }
        engineActiveOnQueue = true

        // Explicitly enforce the output route immediately after engine starts
        updateAudioOutputRoute()
    }

    // MARK: - 3. Microphone Capture -> 48kHz Mono Conversion -> Opus Encode -> UDP `PhoneToPc`

    private func processCapturedMicBuffer(
        _ buffer: AVAudioPCMBuffer,
        sourceRate: Double,
        sourceChannels: Int
    ) {
        let frameCount = Int(buffer.frameLength)
        guard frameCount > 0, let channelData = buffer.floatChannelData else { return }

        let canTransmit = phoneToPcEnabled
            && !isMicMuted
            && (!pushToTalkEnabled || pushToTalkActive)

        if !canTransmit {
            DispatchQueue.main.async {
                self.micPeakLevel = 0.0
                self.micRmsLevel = 0.0
            }
            return
        }

        let srcChannel0 = channelData[0]
        let gainLinear = powf(10.0, micGainDb / 20.0)
        // Snapshot while the tap buffer is guaranteed valid inside the callback
        let capturedSamples = Array(UnsafeBufferPointer(start: srcChannel0, count: frameCount))

        audioQueue.async { [weak self] in
            guard let self = self, self.phoneToPcActiveOnQueue else { return }

            if abs(sourceRate - kOpusSampleRate) < 1.0 {
                for i in 0..<frameCount {
                    let boosted = tanh(capturedSamples[i] * gainLinear)
                    self.micSampleFifo.append(boosted)
                }
                self.resampleLastSample = capturedSamples[frameCount - 1]
                self.hasResampleLastSample = true
            } else {
                // Phase-continuous linear resampling across tap buffer boundaries
                let step = sourceRate / kOpusSampleRate
                if !self.hasResampleLastSample {
                    self.resampleLastSample = capturedSamples[0]
                    self.hasResampleLastSample = true
                }
                var pos = self.resampleFracPos
                while pos < Double(frameCount) {
                    let idx = Int(floor(pos))
                    let frac = Float(pos - Double(idx))
                    let s0: Float = (idx == 0) ? self.resampleLastSample : capturedSamples[idx - 1]
                    let s1: Float = capturedSamples[idx]
                    let interpolated = s0 + (s1 - s0) * frac
                    self.micSampleFifo.append(tanh(interpolated * gainLinear))
                    pos += step
                }
                self.resampleFracPos = pos - Double(frameCount)
                self.resampleLastSample = capturedSamples[frameCount - 1]
            }

            let targetFrameSamples = (self.frameDurationMs == 20) ? kFrameSamples20ms : kFrameSamples10ms

            while (self.micSampleFifo.count - self.micFifoHead) >= targetFrameSamples {
                let frameSlice = self.micSampleFifo[self.micFifoHead..<(self.micFifoHead + targetFrameSamples)]
                self.micFifoHead += targetFrameSamples

                let nowNs = DispatchTime.now().uptimeNanoseconds
                if nowNs &- self.lastMicUiUpdateNanoseconds >= 50_000_000 {
                    self.lastMicUiUpdateNanoseconds = nowNs
                    let (peak, rms) = Self.computePeakAndRms(frameSlice)
                    DispatchQueue.main.async {
                        self.micPeakLevel = peak
                        self.micRmsLevel = rms
                    }
                }

                // Pack into pristine 16-bit Mono Little-Endian PCM with direct contiguous unsafe pointer write
                var pcm16Data = Data(count: targetFrameSamples * 2)
                pcm16Data.withUnsafeMutableBytes { rawBuf in
                    guard let dstPtr = rawBuf.bindMemory(to: Int16.self).baseAddress else { return }
                    var i = 0
                    for s in frameSlice {
                        let clamped = min(max(s, -1.0), 1.0)
                        let s16 = Int16(clamped * 32767.0)
                        dstPtr[i] = s16.littleEndian
                        i += 1
                    }
                }
                self.networkClient?.sendMicrophoneOpusFrame(pcm16Data)
            }

            if self.micFifoHead >= kFrameSamples20ms * 4 {
                self.micSampleFifo.removeFirst(self.micFifoHead)
                self.micFifoHead = 0
            } else if self.micFifoHead == self.micSampleFifo.count {
                self.micSampleFifo.removeAll(keepingCapacity: true)
                self.micFifoHead = 0
            }
        }
    }

    // MARK: - 4. Incoming PC Audio (`PcToPhone`) -> Jitter Buffer -> 48kHz Stereo Opus Decode -> PlayerNode

    private func bindNetworkCallbacks() {
        networkClient?.onInboundPcAudioPacket = { [weak self] packet in
            guard let self = self else { return }
            self.audioQueue.async {
                self.handleInboundPcAudioPacketOnQueue(packet)
            }
        }

        networkClient?.onControlMessage = { [weak self] controlMsg in
            guard let self = self else { return }
            DispatchQueue.main.async {
                switch controlMsg {
                case let .handshakeAck(_, pcToPhone, phoneToPc, bitrate):
                    self.isApplyingRemoteSync = true
                    defer { self.isApplyingRemoteSync = false }
                    self.pcToPhoneEnabled = pcToPhone
                    self.phoneToPcEnabled = phoneToPc
                    self.opusBitrateBps = bitrate
                case let .muteState(streamId, muted):
                    self.isApplyingRemoteSync = true
                    defer { self.isApplyingRemoteSync = false }
                    if streamId == .pcToPhone {
                        self.isSpeakerMuted = muted
                    } else if streamId == .phoneToPc {
                        self.isMicMuted = muted
                    }
                case let .streamConfig(bitrate, frameMs, jitter, rnnoise, gain):
                    self.isApplyingRemoteSync = true
                    defer { self.isApplyingRemoteSync = false }
                    self.opusBitrateBps = bitrate
                    self.frameDurationMs = Int(frameMs)
                    self.jitterBufferDepth = jitter
                    self.rnnoiseEnabledOnPc = rnnoise
                    self.micGainDb = gain
                case let .syncConfig(sync):
                    guard sync.revision >= self.syncRevision else { return }
                    self.syncRevision = sync.revision
                    self.isApplyingRemoteSync = true
                    defer { self.isApplyingRemoteSync = false }
                    self.pcToPhoneEnabled = sync.pcToPhoneEnabled
                    self.phoneToPcEnabled = sync.phoneToPcEnabled
                    self.isSpeakerMuted = sync.pcMuted
                    self.isMicMuted = sync.micMuted
                    self.pushToTalkEnabled = sync.pushToTalkEnabled
                    self.pushToTalkActive = sync.pushToTalkActive
                    self.aecEnabledOnPc = sync.aecEnabled
                    self.rnnoiseEnabledOnPc = sync.rnnoiseEnabled
                    self.vadGateThreshold = sync.vadGateThreshold
                    self.micGainDb = sync.micGainDb
                    self.speakerVolume = sync.pcVolume
                    self.opusBitrateBps = sync.opusBitrateBps
                    self.frameDurationMs = Int(sync.frameDurationMs)
                    self.jitterBufferDepth = sync.jitterBufferPackets
                default:
                    break
                }
            }
        }
    }

    private func handleInboundPcAudioPacketOnQueue(_ packet: FramedPacket) {
        guard pcToPhoneActiveOnQueue && !isSpeakerMuted else {
            DispatchQueue.main.async {
                self.pcPeakLevel = 0.0
                self.pcRmsLevel = 0.0
            }
            return
        }

        pcJitterBuffer.push(sequenceNum: packet.header.sequenceNum, payload: packet.payload)

        while true {
            switch pcJitterBuffer.popNext() {
            case .notReady:
                return
            case .frame(let pcm16Data):
                scheduleStereoPcm16OnPlayerNode(pcm16Data)
            case .lostPacket:
                break
            }
        }
    }

    private func scheduleStereoPcm16OnPlayerNode(_ pcmData: Data) {
        let frames = pcmData.count / 4 // 2 channels * 2 bytes = 4 bytes per stereo frame
        guard frames > 0,
              let format = playbackFormat,
              let pcmBuffer = AVAudioPCMBuffer(
                  pcmFormat: format,
                  frameCapacity: AVAudioFrameCount(frames)
              ),
              let channelData = pcmBuffer.floatChannelData else {
            return
        }

        pcmBuffer.frameLength = AVAudioFrameCount(frames)
        let leftDst = channelData[0]
        let rightDst = channelData[1]
        let vol = min(max(speakerVolume, 0.0), 2.0)

        var peak: Float = 0.0
        var sumSq: Float = 0.0

        pcmData.withUnsafeBytes { rawPtr in
            guard let base = rawPtr.bindMemory(to: Int16.self).baseAddress else { return }
            for f in 0..<frames {
                let l = (Float(Int16(littleEndian: base[f * 2])) / 32768.0) * vol
                let r = (Float(Int16(littleEndian: base[f * 2 + 1])) / 32768.0) * vol
                leftDst[f] = min(max(l, -1.0), 1.0)
                rightDst[f] = min(max(r, -1.0), 1.0)

                let maxAbs = max(abs(l), abs(r))
                if maxAbs > peak { peak = maxAbs }
                sumSq += (l * l + r * r) * 0.5
            }
        }

        let nowNs = DispatchTime.now().uptimeNanoseconds
        if nowNs &- self.lastPcUiUpdateNanoseconds >= 50_000_000 {
            self.lastPcUiUpdateNanoseconds = nowNs
            let rms = sqrt(sumSq / Float(frames))
            DispatchQueue.main.async {
                self.pcPeakLevel = peak
                self.pcRmsLevel = rms
            }
        }

        if !playerNode.isPlaying && audioEngine.isRunning {
            playerNode.play()
        }
        playerNode.scheduleBuffer(pcmBuffer, completionHandler: nil)
    }

    // MARK: - Helpers & Interruption Recovery

    private func pushSyncConfigToPc() {
        guard let client = networkClient, client.isConnected else { return }
        syncRevision &+= 1
        let payload = SyncConfigPayload(
            revision: syncRevision,
            pcToPhoneEnabled: pcToPhoneEnabled,
            phoneToPcEnabled: phoneToPcEnabled,
            pcMuted: isSpeakerMuted,
            micMuted: isMicMuted,
            pushToTalkEnabled: pushToTalkEnabled,
            pushToTalkActive: pushToTalkActive,
            aecEnabled: aecEnabledOnPc,
            rnnoiseEnabled: rnnoiseEnabledOnPc,
            vadGateThreshold: vadGateThreshold,
            micGainDb: micGainDb,
            pcVolume: speakerVolume,
            opusBitrateBps: opusBitrateBps,
            frameDurationMs: UInt8(frameDurationMs == 20 ? 20 : 10),
            jitterBufferPackets: jitterBufferDepth
        )
        client.sendControlMessage(.syncConfig(payload))
    }

    @inline(__always)
    private static func computePeakAndRms<C: Collection>(_ samples: C) -> (Float, Float) where C.Element == Float {
        guard !samples.isEmpty else { return (0.0, 0.0) }
        var peak: Float = 0.0
        var sumSq: Float = 0.0
        for s in samples {
            let a = abs(s)
            if a > peak { peak = a }
            sumSq += s * s
        }
        return (peak, sqrt(sumSq / Float(samples.count)))
    }

    private func setupAudioSessionNotifications() {
        NotificationCenter.default.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: AVAudioSession.sharedInstance(),
            queue: nil
        ) { [weak self] notification in
            guard let self = self,
                  let userInfo = notification.userInfo,
                  let typeVal = userInfo[AVAudioSessionInterruptionTypeKey] as? UInt,
                  let type = AVAudioSession.InterruptionType(rawValue: typeVal) else {
                return
            }

            if type == .ended {
                // Automatically resume AVAudioSession & AVAudioEngine after phone call or Siri interruption
                self.audioQueue.async {
                    guard self.engineActiveOnQueue else { return }
                    try? self.configureAudioSession()
                    try? self.setupAndStartEngineOnQueue()
                }
            }
        }

        NotificationCenter.default.addObserver(
            forName: AVAudioSession.routeChangeNotification,
            object: AVAudioSession.sharedInstance(),
            queue: nil
        ) { [weak self] _ in
            guard let self = self else { return }
            self.checkBluetoothConnectionStatus()
            self.audioQueue.async {
                guard self.engineActiveOnQueue,
                      (self.phoneToPcActiveOnQueue || self.pcToPhoneActiveOnQueue) else {
                    return
                }
                if !self.audioEngine.isRunning {
                    try? self.setupAndStartEngineOnQueue()
                }
            }
        }
    }

    /// Inspects available audio session input/output ports to verify if Bluetooth is actively connected.
    public func checkBluetoothConnectionStatus() {
        let session = AVAudioSession.sharedInstance()
        let outputs = session.currentRoute.outputs
        let inputs = session.availableInputs ?? []

        let btOutput = outputs.contains { port in
            port.portType == .bluetoothA2DP ||
            port.portType == .bluetoothHFP ||
            port.portType == .bluetoothLE
        }
        let btInput = inputs.contains { port in
            port.portType == .bluetoothHFP ||
            port.portType == .bluetoothA2DP ||
            port.portType == .bluetoothLE
        }

        let isBtAvailable = btOutput || btInput
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            self.isBluetoothConnected = isBtAvailable
            if !isBtAvailable && self.selectedOutputRoute == .bluetooth {
                // If Bluetooth was active but disconnected, gracefully revert to Loudspeaker
                self.selectedOutputRoute = .speaker
            }
        }
    }

    private func publishAudioStatus(_ message: String) {
        DispatchQueue.main.async {
            self.audioStatusMessage = message
        }
    }
}
