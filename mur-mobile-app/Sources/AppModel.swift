import Foundation
import Observation

/// App-wide state + the bridge to the Rust `MobileClient`.
///
/// The SDK's `MobileEventListener` callbacks arrive on a background thread; we
/// hop to the main actor before touching any `@Observable` state.
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
    /// On-device partial transcript (filled by SFSpeech in P3).
    var partial: String = ""
    /// Live mic amplitude 0...1 (drives the mascot ring in P3).
    var micLevel: Double = 0

    private var client: MobileClient?
    private var bridge: EventBridge?

    var isConnected: Bool { connectedAgent != nil }

    /// Construct the Rust client + register the event listener (idempotent).
    func start() {
        guard client == nil else { return }
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
        }
    }

    /// Connect to a Mac endpoint discovered via the pairing QR.
    func connect(host: String, port: UInt16, token: String) {
        start()
        client?.connectLan(host: host, port: port, pairToken: token)
    }

    func disconnect() {
        client?.disconnect()
        connectedAgent = nil
        mascot = .offline
    }

    // MARK: Push-to-talk (P2 shell; real audio capture lands in P3)

    func beginCapture() {
        guard mascot != .offline else { return }
        mascot = .listening
        // P3: start AVAudioEngine capture + on-device SFSpeech partial here.
    }

    func endCaptureAndSend() {
        let text = partial.trimmingCharacters(in: .whitespacesAndNewlines)
        partial = ""
        guard !text.isEmpty else {
            mascot = isConnected ? .idle : .offline
            return
        }
        send(text)
    }

    /// Send a typed message (the text field stands in for voice until P3).
    func sendTyped(_ text: String) {
        send(text.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    func toggleMicMode() {
        micMode = (micMode == .pushToTalk) ? .handsFree : .pushToTalk
    }

    private func send(_ text: String) {
        guard !text.isEmpty else { return }
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
            // The user's own turn is shown locally on send; only surface the
            // agent's authoritative transcript here.
            if role != "user", isFinal, !text.isEmpty {
                transcript.append(.init(role: "agent", text: text))
            }
        case let .reply(text):
            transcript.append(.init(role: "agent", text: text))
            mascot = .idle
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

/// Adapts the SDK's `MobileEventListener` protocol to a Swift closure.
/// `final` + a `@Sendable` closure makes it safe to hand across the FFI thread.
final class EventBridge: MobileEventListener {
    private let handler: @Sendable (MobileEvent) -> Void
    init(_ handler: @escaping @Sendable (MobileEvent) -> Void) { self.handler = handler }
    func onEvent(event: MobileEvent) { handler(event) }
}
