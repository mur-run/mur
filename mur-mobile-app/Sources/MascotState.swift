import Foundation

/// High-level state of the conversation, which drives both the 椋鳥 mascot
/// animation and the orange button's appearance. Mirrors the states in the
/// design spec (idle / listening / thinking / speaking / error) plus the
/// launch animation handled separately.
enum MascotState: Equatable {
    case offline          // not connected / connection lost
    case idle             // connected, waiting
    case listening        // capturing the user's voice (PTT held or hands-free)
    case thinking         // request sent, awaiting the agent
    case speaking         // agent reply playing (P3)
    case error(String)    // recoverable error to surface

    /// Whether the mic is actively capturing in this state.
    var isCapturing: Bool { self == .listening }

    /// Short human label for accessibility / debug.
    var label: String {
        switch self {
        case .offline:   return "Offline"
        case .idle:      return "Ready"
        case .listening: return "Listening"
        case .thinking:  return "Thinking"
        case .speaking:  return "Speaking"
        case .error:     return "Error"
        }
    }
}

/// Whether the mic operates in push-to-talk (hold) or hands-free (open-mic).
enum MicMode: Equatable {
    case pushToTalk      // default: hold the button to speak
    case handsFree       // triple-tap toggles this: continuous, VAD-driven (P3)
}
