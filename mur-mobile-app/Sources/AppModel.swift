import Foundation
import Observation
@preconcurrency import AVFoundation
import Speech

@MainActor
@Observable
final class AppModel {
    struct ChatLine: Identifiable, Equatable {
        let id = UUID()
        let role: String   // "user" | "agent"
        let text: String
    }

    private(set) var mascot: MascotState = .offline
    private(set) var micMode: MicMode = .pushToTalk
    private(set) var transcript: [ChatLine] = []
    private(set) var connectedAgent: String?
    var partial: String = ""
    var micLevel: Double = 0

    private var client: MobileClient?
    private var bridge: EventBridge?

    // TTS playback
    private var ttsEngine = AVAudioEngine()
    private var ttsPlayerNode = AVAudioPlayerNode()
    private var ttsBuffer: Data = Data()
    private var ttsSampleRate: AVAudioFrameCount = 24000

    // Voice capture
    private var audioEngine = AVAudioEngine()
    private var recognizer: SFSpeechRecognizer?
    private var recognitionRequest: SFSpeechAudioBufferRecognitionRequest?
    private var recognitionTask: SFSpeechRecognitionTask?

    // All locales the device's SFSpeech supports, sorted: preferred languages first,
    // then alphabetical by display name.
    static let availableLocales: [Locale] = {
        let preferred = Locale.preferredLanguages.compactMap { Locale(identifier: $0) }
        let all = SFSpeechRecognizer.supportedLocales()
            .sorted { a, b in
                let nameA = a.localizedString(forIdentifier: a.identifier) ?? a.identifier
                let nameB = b.localizedString(forIdentifier: b.identifier) ?? b.identifier
                return nameA < nameB
            }
        var seen = Set<String>()
        var ordered: [Locale] = []
        for locale in preferred where all.contains(locale) {
            if seen.insert(locale.identifier).inserted { ordered.append(locale) }
        }
        for locale in all {
            if seen.insert(locale.identifier).inserted { ordered.append(locale) }
        }
        return ordered
    }()

    private(set) var speechLocale: Locale = {
        let preferred = Locale.preferredLanguages.compactMap { Locale(identifier: $0) }
        let supported = SFSpeechRecognizer.supportedLocales()
        return preferred.first(where: { supported.contains($0) }) ?? Locale.current
    }()

    var speechLocaleLabel: String {
        // Show language name only (no region) so the header stays compact.
        let langCode = speechLocale.language.languageCode?.identifier ?? ""
        return Locale.current.localizedString(forLanguageCode: langCode)
            ?? speechLocale.localizedString(forIdentifier: speechLocale.identifier)
            ?? speechLocale.identifier
    }

    func setSpeechLocale(_ locale: Locale) {
        speechLocale = locale
        recognizer = SFSpeechRecognizer(locale: locale)
    }

    var isConnected: Bool { connectedAgent != nil }

    func start() {
        guard client == nil else { return }
        recognizer = SFSpeechRecognizer(locale: speechLocale)
        do {
            let config = MobileConfig(
                murHome: Self.appHome(),
                defaultAgent: "mur",
                relayBaseUrl: nil
            )
            let client = try MobileClient(config: config)
            let bridge = EventBridge { [weak self] event in
                Task { @MainActor in self?.handle(event) }
            }
            client.setListener(listener: bridge)
            self.client = client
            self.bridge = bridge
            mascot = .offline
        } catch {
            mascot = .error("init: \(error.localizedDescription)")
            return
        }
        // Debug: auto-connect from UserDefaults (set via xcrun simctl spawn defaults write)
        let ud = UserDefaults.standard
        if let host = ud.string(forKey: "debugConnectHost"),
           let token = ud.string(forKey: "debugConnectToken") {
            let port = UInt16(ud.integer(forKey: "debugConnectPort"))
            print("[MurVoice] auto-connect from UserDefaults host=\(host) port=\(port)")
            client?.connectLan(host: host, port: port == 0 ? 9430 : port, pairToken: token)
            // If a debug message is also set, send it after a short delay for connection to establish
            if let msg = ud.string(forKey: "debugSendMessage") {
                DispatchQueue.main.asyncAfter(deadline: .now() + 2.5) { [weak self] in
                    print("[MurVoice] auto-send debugSendMessage: \(msg)")
                    self?.sendTyped(msg)
                    ud.removeObject(forKey: "debugSendMessage")
                }
            }
        }
    }

    func connect(host: String, port: UInt16, token: String) {
        print("[MurVoice] connect host=\(host) port=\(port)")
        start()
        client?.connectLan(host: host, port: port, pairToken: token)
    }

    func disconnect() {
        client?.disconnect()
        connectedAgent = nil
        mascot = .offline
    }

    // MARK: Voice capture

    func beginCapture() {
        guard mascot != .offline else { return }
        SFSpeechRecognizer.requestAuthorization { [weak self] status in
            Task { @MainActor [weak self] in
                guard let self, status == .authorized else { return }
                self.startAudioCapture()
            }
        }
    }

    func endCaptureAndSend() {
        stopAudioCapture()
        let text = partial.trimmingCharacters(in: .whitespacesAndNewlines)
        partial = ""
        guard !text.isEmpty else {
            mascot = isConnected ? .idle : .offline
            return
        }
        send(text)
    }

    func sendTyped(_ text: String) {
        send(text.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    func toggleMicMode() {
        micMode = (micMode == .pushToTalk) ? .handsFree : .pushToTalk
        if micMode == .handsFree {
            beginCapture()
        } else {
            endCaptureAndSend()
        }
    }

    // MARK: Private

    private func startAudioCapture() {
        stopAudioCapture()

        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(.record, mode: .measurement, options: .duckOthers)
            try session.setActive(true, options: .notifyOthersOnDeactivation)
        } catch {
            mascot = .error("mic: \(error.localizedDescription)")
            return
        }

        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        request.requiresOnDeviceRecognition = false
        recognitionRequest = request

        let inputNode = audioEngine.inputNode
        let format = inputNode.outputFormat(forBus: 0)

        recognitionTask = recognizer?.recognitionTask(with: request) { [weak self] result, error in
            Task { @MainActor [weak self] in
                guard let self else { return }
                if let result {
                    self.partial = result.bestTranscription.formattedString
                    // hands-free: auto-send when speech pauses (isFinal)
                    if result.isFinal && self.micMode == .handsFree {
                        self.endCaptureAndSend()
                    }
                }
                if error != nil {
                    self.stopAudioCapture()
                    if self.micMode == .handsFree {
                        // restart listening loop
                        self.beginCapture()
                    }
                }
            }
        }

        inputNode.installTap(onBus: 0, bufferSize: 1024, format: format) { [weak self] buffer, _ in
            self?.recognitionRequest?.append(buffer)
            // mic level for mascot ring
            guard let channelData = buffer.floatChannelData?[0] else { return }
            let frameLength = Int(buffer.frameLength)
            var sum: Float = 0
            for i in 0..<frameLength { sum += channelData[i] * channelData[i] }
            let rms = Double(sqrt(sum / Float(frameLength)))
            Task { @MainActor [weak self] in self?.micLevel = min(rms * 10, 1.0) }
        }

        do {
            try audioEngine.start()
            mascot = .listening
        } catch {
            stopAudioCapture()
            mascot = .error("audio engine: \(error.localizedDescription)")
        }
    }

    private func playTTSBuffer() {
        let bytes = ttsBuffer
        ttsBuffer = Data()
        let sr = ttsSampleRate
        guard !bytes.isEmpty else { return }

        // f32 LE PCM → AVAudioPCMBuffer at 24 kHz mono
        let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: Double(sr), channels: 1, interleaved: false)!
        let frameCount = AVAudioFrameCount(bytes.count / 4)
        guard let buf = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frameCount) else { return }
        buf.frameLength = frameCount
        bytes.withUnsafeBytes { ptr in
            if let f32 = ptr.baseAddress?.assumingMemoryBound(to: Float.self),
               let ch = buf.floatChannelData?[0] {
                ch.update(from: f32, count: Int(frameCount))
            }
        }

        ttsEngine.attach(ttsPlayerNode)
        let outputFormat = ttsEngine.outputNode.inputFormat(forBus: 0)
        ttsEngine.connect(ttsPlayerNode, to: ttsEngine.outputNode, format: outputFormat)

        do {
            try ttsEngine.start()
        } catch {
            tracing(error); return
        }

        let convertFormat = outputFormat
        guard let converter = AVAudioConverter(from: format, to: convertFormat),
              let outBuf = AVAudioPCMBuffer(pcmFormat: convertFormat,
                                            frameCapacity: AVAudioFrameCount(Double(frameCount) * convertFormat.sampleRate / Double(sr)))
        else { return }

        var convError: NSError?
        converter.convert(to: outBuf, error: &convError) { _, status in
            status.pointee = .haveData; return buf
        }

        ttsPlayerNode.scheduleBuffer(outBuf) { [weak self] in
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.mascot = self.isConnected ? .idle : .offline
                self.ttsEngine.stop()
                if self.micMode == .handsFree { self.beginCapture() }
            }
        }
        ttsPlayerNode.play()
        mascot = .speaking
    }

    private func tracing(_ error: Error) {
        mascot = .error(error.localizedDescription)
    }

    private func stopAudioCapture() {
        audioEngine.stop()
        audioEngine.inputNode.removeTap(onBus: 0)
        recognitionRequest?.endAudio()
        recognitionRequest = nil
        recognitionTask?.cancel()
        recognitionTask = nil
        micLevel = 0
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        if mascot == .listening {
            mascot = isConnected ? .idle : .offline
        }
    }

    private func send(_ text: String) {
        guard !text.isEmpty else { return }
        print("[MurVoice] send: \(text)")
        transcript.append(.init(role: "user", text: text))
        mascot = .thinking
        client?.sendText(text: text)
    }

    private func handle(_ event: MobileEvent) {
        switch event {
        case .connecting:
            mascot = isConnected ? mascot : .idle
        case let .connected(_, agent):
            connectedAgent = agent
            mascot = .idle
        case .disconnected:
            connectedAgent = nil
            mascot = .offline
        case let .transcript(role, text, isFinal):
            if role != "user", isFinal, !text.isEmpty {
                transcript.append(.init(role: "agent", text: text))
            }
        case let .reply(text):
            transcript.append(.init(role: "agent", text: text))
            mascot = isConnected ? .speaking : .offline
        case let .audioChunk(data, sampleRate, done):
            ttsBuffer.append(contentsOf: data)
            ttsSampleRate = AVAudioFrameCount(sampleRate)
            if done {
                playTTSBuffer()
            }
        case let .error(message):
            mascot = .error(message)
        }
    }

    private static func appHome() -> String {
        let base = FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask).first
        let dir = (base ?? URL(fileURLWithPath: NSTemporaryDirectory()))
            .appendingPathComponent("mur", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.path
    }
}

final class EventBridge: MobileEventListener {
    private let handler: @Sendable (MobileEvent) -> Void
    init(_ handler: @escaping @Sendable (MobileEvent) -> Void) { self.handler = handler }
    func onEvent(event: MobileEvent) { handler(event) }
}
