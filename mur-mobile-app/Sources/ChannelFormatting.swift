import SwiftUI

/// How a channel event renders in the detail feed.
enum EventRowVariant: Equatable {
    case userMessage      // human, right-aligned
    case agentMessage     // agent, left-aligned
    case note             // system note / separator
    case state            // state-change chip
    case delegation       // thin "A → B" separator
    case tool             // collapsed tool call/result one-liner
    case artifact         // openable card
    case hitl             // prominent approval card (display)
    case other            // forward-compatible fallback card
}

/// Channels sorted for the home list: input-required first (needs the human),
/// then most-recently-updated. Pure; `updatedAt` is RFC3339 so string-desc works.
func sortedChannels(_ channels: [AppModel.ChannelSummary]) -> [AppModel.ChannelSummary] {
    channels.sorted { a, b in
        let aBlocked = a.state == "input-required"
        let bBlocked = b.state == "input-required"
        if aBlocked != bBlocked { return aBlocked }   // blocked first
        return a.updatedAt > b.updatedAt              // newest first
    }
}

/// State → (label, color) for the lifecycle chip.
func stateChip(_ state: String) -> (label: String, color: Color) {
    switch state {
    case "working":        return ("working", .murBlue)
    case "input-required": return ("needs you", .murOrange)
    case "completed":      return ("done", .green)
    case "failed", "rejected": return (state, .red)
    case "submitted", "stale", "canceled": return (state, .gray)
    default:               return (state, .gray)
    }
}

/// Decide the render variant for an event.
func eventVariant(actorKind: String, kind: String) -> EventRowVariant {
    switch kind {
    case "message":
        switch actorKind {
        case "human": return .userMessage
        case "agent": return .agentMessage
        default:      return .note
        }
    case "note":          return .note
    case "state-change":  return .state
    case "delegation", "handoff": return .delegation
    case "tool-call", "tool-result": return .tool
    case "artifact":      return .artifact
    case "hitl-request":  return .hitl
    default:              return .other
    }
}

/// "tool-call" → "Tool Call" for fallback card headers.
func eventKindLabel(_ kind: String) -> String {
    kind.split(separator: "-").map { $0.prefix(1).uppercased() + $0.dropFirst() }.joined(separator: " ")
}

/// A short author label for the feed.
func actorLabel(actorKind: String, actorName: String) -> String {
    switch actorKind {
    case "human":  return actorName.isEmpty ? "You" : actorName
    case "agent":  return actorName.isEmpty ? "agent" : actorName
    default:       return "system"
    }
}
