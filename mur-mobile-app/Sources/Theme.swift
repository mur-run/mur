import SwiftUI
import AudioToolbox

extension Color {
    /// Brand orange for the primary "speak" button (spec: orange CTA).
    static let murOrange = Color(red: 0.96, green: 0.49, blue: 0.20)
    /// MUR blue-starling brand accent.
    static let murBlue = Color(red: 0.20, green: 0.42, blue: 0.78)
}

// MARK: - Haptics

enum Haptics {
    static func tap() {
        #if canImport(UIKit)
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        #endif
    }
    static func start() {
        #if canImport(UIKit)
        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        #endif
    }
    static func toggle() {
        #if canImport(UIKit)
        UINotificationFeedbackGenerator().notificationOccurred(.success)
        #endif
    }
    /// Agent reply received.
    static func reply() {
        #if canImport(UIKit)
        UINotificationFeedbackGenerator().notificationOccurred(.success)
        #endif
    }
    /// Recoverable error surfaced to the user.
    static func error() {
        #if canImport(UIKit)
        UINotificationFeedbackGenerator().notificationOccurred(.error)
        #endif
    }
    /// PTT press released / stream ended.
    static func release() {
        #if canImport(UIKit)
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        #endif
    }
}

// MARK: - Earcons

/// Short audio cues for key voice-interaction moments.
///
/// Uses standard iOS system sounds so no bundled audio assets are required.
/// System sound IDs that mirror iOS dictation:
///   1113 — begin recording (Voice Memos / Siri begin)
///   1114 — end recording   (Voice Memos / Siri end)
///   1057 — short click     (calendar alert, used for "thinking")
enum Earcons {
    /// Played when the mic opens and audio streaming begins.
    static func playListening() {
        AudioServicesPlaySystemSound(1113)
    }
    /// Played when the audio stream ends and we're waiting for the agent.
    static func playDone() {
        AudioServicesPlaySystemSound(1114)
    }
    /// Subtle cue when the agent's reply has arrived.
    static func playReply() {
        AudioServicesPlaySystemSound(1057)
    }
}

#if canImport(UIKit)
import UIKit
#endif
