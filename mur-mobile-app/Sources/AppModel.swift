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

    struct ChannelSummary: Identifiable, Equatable {
        let id: String
        let title: String
        let state: String       // kebab ChannelState
        let goal: String
        let updatedAt: String
        let agents: [String]
        let turns: Int
    }

    struct ChannelEventVM: Identifiable, Equatable {
        var id: UInt64 { seq }
        let seq: UInt64
        let ts: String
        let actorKind: String   // "human" | "agent" | "system"
        let actorName: String
        let kind: String        // "message" | "state-change" | …
        let text: String
        let hitlId: String      // set on HitlRequest events (v4c), else ""
    }

    private(set) var mascot: MascotState = .offline
    private(set) var micMode: MicMode = .pushToTalk
    private(set) var transcript: [ChatLine] = []
    private(set) var connectedAgent: String?
    private(set) var channels: [ChannelSummary] = []
    private(set) var detailChannelId: String?
    private(set) var detailEvents: [ChannelEventVM] = []
    var partial: String = ""
    var micLevel: Double = 0

    private var client: MobileClient?
    private var bridge: EventBridge?
    /// LAN endpoint of an in-progress pairing, persisted once `.connected` fires
    /// so the next launch can `resumeIfPaired`.
    private var pendingPair: (host: String, port: UInt16)?

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
    private var streamConverter: AVAudioConverter?   // native → 16 kHz mono f32
    private var isStreamingAudio = false

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
        } else {
            // Production launch: if a device was previously paired, reconnect by
            // key (no token needed) over the remembered LAN endpoint.
            resumeIfPaired()
        }
    }

    func connect(host: String, port: UInt16, token: String) {
        print("[MurVoice] connect host=\(host) port=\(port)")
        start()
        client?.connectLan(host: host, port: port, pairToken: token)
    }

    /// Pair from a scanned QR. A v=2 QR (`wid`+`did`) enrolls via the HMAC proof
    /// handshake — the token is never transmitted; an older QR falls back to the
    /// legacy bearer path. On success the LAN endpoint is remembered so the next
    /// launch reconnects by key (`resumeIfPaired`).
    func pair(_ info: PairingInfo) {
        start()
        pendingPair = (info.host, info.port)
        if let wid = info.wid, let did = info.did {
            print("[MurVoice] enroll (proof) host=\(info.host) port=\(info.port)")
            client?.enrollLan(
                host: info.host, port: info.port, token: info.token, wid: wid, daemonId: did)
        } else {
            print("[MurVoice] pair (legacy) host=\(info.host) port=\(info.port)")
            client?.connectLan(host: info.host, port: info.port, pairToken: info.token)
        }
    }

    /// Reconnect a previously-paired device by KEY (no token) — e.g. on launch.
    /// Uses the persisted LAN endpoint; the phone's identity (already on disk) is
    /// what authenticates, so no pairing window is needed.
    func resumeIfPaired() {
        let ud = UserDefaults.standard
        guard ud.bool(forKey: "isPaired"), let host = ud.string(forKey: "pairedHost") else {
            return
        }
        let stored = UInt16(ud.integer(forKey: "pairedPort"))
        let port = stored == 0 ? 9430 : stored
        print("[MurVoice] resume host=\(host) port=\(port)")
        start()
        client?.resumeLan(host: host, port: port)
    }

    /// Connect via mur-server relay (for use away from home Wi-Fi).
    func connectRelay(relayWsUrl: String, apiKey: String, pairToken: String) {
        print("[MurVoice] connectRelay url=\(relayWsUrl)")
        start()
        client?.connectRelay(relayWsUrl: relayWsUrl, jwt: apiKey, pairToken: pairToken)
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
        let wasStreaming = isStreamingAudio
        stopAudioCapture()
        if wasStreaming {
            // Audio path: tell Mac to run whisper on the accumulated frames.
            // The Mac sends ServerFrame::Transcript back, then the agent reply.
            Earcons.playDone()
            Haptics.release()
            client?.endAudioStream()
            partial = ""
            if isConnected { mascot = .thinking }
        } else {
            // Fallback text path (no audio stream, e.g. not connected when
            // capture started or SFSpeech-only without streaming).
            let text = partial.trimmingCharacters(in: .whitespacesAndNewlines)
            partial = ""
            guard !text.isEmpty else {
                mascot = isConnected ? .idle : .offline
                return
            }
            send(text)
        }
    }

    func sendTyped(_ text: String) {
        send(text.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    func refreshChannels() { client?.listChannels() }

    func openChannel(_ id: String) {
        detailChannelId = id
        detailEvents = []
        client?.fetchChannelEvents(channelId: id, sinceSeq: nil)
    }

    func closeChannel() { detailChannelId = nil; detailEvents = [] }

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

        // Converter: native hardware format → 16 kHz mono f32 for whisper on Mac.
        let whisperFormat = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: 16_000,
            channels: 1,
            interleaved: false
        )!
        streamConverter = AVAudioConverter(from: format, to: whisperFormat)

        inputNode.installTap(onBus: 0, bufferSize: 1024, format: format) { [weak self] buffer, _ in
            guard let self else { return }
            self.recognitionRequest?.append(buffer)
            // mic level for mascot ring
            if let channelData = buffer.floatChannelData?[0] {
                let frameLength = Int(buffer.frameLength)
                var sum: Float = 0
                for i in 0..<frameLength { sum += channelData[i] * channelData[i] }
                let rms = Double(sqrt(sum / Float(frameLength)))
                Task { @MainActor [weak self] in self?.micLevel = min(rms * 10, 1.0) }
            }
            // Stream 16 kHz mono f32 to Mac for whisper STT.
            guard let converter = self.streamConverter else { return }
            let ratio = 16_000.0 / format.sampleRate
            let outFrames = AVAudioFrameCount(Double(buffer.frameLength) * ratio + 1)
            guard let outBuf = AVAudioPCMBuffer(pcmFormat: converter.outputFormat,
                                                frameCapacity: outFrames) else { return }
            var convErr: NSError?
            var inputDone = false
            let status = converter.convert(to: outBuf, error: &convErr) { _, outStatus in
                if inputDone { outStatus.pointee = .noDataNow; return nil }
                inputDone = true
                outStatus.pointee = .haveData
                return buffer
            }
            guard status != .error, outBuf.frameLength > 0,
                  let ch = outBuf.floatChannelData?[0] else { return }
            let byteCount = Int(outBuf.frameLength) * 4
            let data = Data(bytes: UnsafeRawPointer(ch), count: byteCount)
            Task { @MainActor [weak self] in
                self?.client?.sendAudioFrame(data: data)
            }
        }

        do {
            try audioEngine.start()
            isStreamingAudio = true
            client?.beginAudioStream(sampleRate: 16_000)
            Earcons.playListening()
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
        isStreamingAudio = false
        streamConverter = nil
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
        client?.sendText(text: text, channelId: nil)
    }

    // MARK: Channel participation (v4c)

    /// Drop a turn into a SPECIFIC channel (a Hub/CLI-originated one, not just
    /// the concierge). The daemon persists into it and dials its router agent.
    func sendToChannel(_ text: String, channelId: String) {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty else { return }
        print("[MurVoice] sendToChannel \(channelId): \(t)")
        client?.sendText(text: t, channelId: channelId)
    }

    /// Authoritatively release a HITL gate from the phone (v4c). The daemon
    /// verifies the paired frame, then writes a v3d-signed HitlResponse the gate
    /// is waiting on. `reason` is a short human note for the audit log.
    func respondHitl(channelId: String, hitlId: String, allow: Bool) {
        guard !hitlId.isEmpty else { return }
        client?.hitlRespond(channelId: channelId, hitlId: hitlId, allow: allow,
                            reason: allow ? "approved on phone" : "denied on phone")
    }

    /// Participants (agents) of the open channel — the first @mention source.
    var detailParticipants: [String] {
        channels.first(where: { $0.id == detailChannelId })?.agents ?? []
    }

    /// Every agent seen locally across channels — the @mention fallback pool.
    var mentionableAgents: [String] {
        var set: [String] = []
        for c in channels {
            for a in c.agents where !set.contains(a) { set.append(a) }
        }
        return set
    }

    private func handle(_ event: MobileEvent) {
        switch event {
        case .connecting:
            mascot = isConnected ? mascot : .idle
        case let .connected(_, agent):
            connectedAgent = agent
            mascot = .idle
            // Remember the endpoint of a just-completed pairing so the next launch
            // reconnects by key via resumeIfPaired().
            if let (h, p) = pendingPair {
                let ud = UserDefaults.standard
                ud.set(h, forKey: "pairedHost")
                ud.set(Int(p), forKey: "pairedPort")
                ud.set(true, forKey: "isPaired")
                pendingPair = nil
            }
            client?.listChannels()
        case .disconnected:
            connectedAgent = nil
            mascot = .offline
        case let .transcript(role, text, isFinal):
            guard isFinal, !text.isEmpty else { break }
            if role == "user" {
                // Mac whisper authoritative transcript — add user bubble and update partial.
                transcript.append(.init(role: "user", text: text))
                partial = text
            } else {
                transcript.append(.init(role: "agent", text: text))
            }
        case let .reply(text):
            transcript.append(.init(role: "agent", text: text))
            Haptics.reply()
            Earcons.playReply()
            mascot = isConnected ? .speaking : .offline
        case let .audioChunk(data, sampleRate, done):
            ttsBuffer.append(contentsOf: data)
            ttsSampleRate = AVAudioFrameCount(sampleRate)
            if done {
                playTTSBuffer()
            }
        case let .error(message):
            Haptics.error()
            mascot = .error(message)
        case let .channelList(channels: items):
            channels = items.map {
                ChannelSummary(id: $0.id, title: $0.title, state: $0.state, goal: $0.goal,
                               updatedAt: $0.updatedAt, agents: $0.agents, turns: Int($0.turns))
            }
        case let .channelEvents(channelId, events):
            guard channelId == detailChannelId else { break }
            detailEvents = events.map {
                ChannelEventVM(seq: $0.seq, ts: $0.ts, actorKind: $0.actorKind,
                               actorName: $0.actorName, kind: $0.kind, text: $0.text,
                               hitlId: $0.hitlId)
            }
        case let .channelUpdate(channelId):
            client?.listChannels()
            if channelId == detailChannelId { client?.fetchChannelEvents(channelId: channelId, sinceSeq: nil) }
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
