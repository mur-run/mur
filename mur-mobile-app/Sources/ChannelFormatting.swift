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

// MARK: - @mention (client-side scoping hint, v4c)
//
// `@name` is advisory to the concierge orchestrator — never authoritative to a
// worker and never opens a phone→specialist socket. These pure helpers drive the
// detail-bar autocomplete; the literal "@name …" text is delivered to the
// concierge channel, which (v3b) decides whether to honor it.

/// Index of the trailing "@" that *begins* a mention token: the last "@" that is
/// at the start of the draft or immediately preceded by whitespace, with no
/// whitespace between it and the cursor (end of draft). Returns nil otherwise —
/// so an email-like "user@host" mid-word never registers as a mention.
private func mentionAtIndex(in draft: String) -> String.Index? {
    guard let at = draft.lastIndex(of: "@") else { return nil }
    // "@" must start a token: beginning-of-line or preceded by whitespace.
    if at != draft.startIndex, !draft[draft.index(before: at)].isWhitespace {
        return nil
    }
    // …and the token must be unbroken up to the cursor (no whitespace after "@").
    let after = draft[draft.index(after: at)...]
    return after.contains(where: { $0.isWhitespace }) ? nil : at
}

/// Parse a trailing "@partial" token from the draft for autocomplete. Returns
/// the partial (without "@") if the cursor is in a mention token, else nil.
func mentionToken(in draft: String) -> String? {
    guard let at = mentionAtIndex(in: draft) else { return nil }
    return String(draft[draft.index(after: at)...])
}

/// Autocomplete candidates for a partial @mention: channel participants first,
/// then other known agents, filtered by prefix, deduped.
func mentionCandidates(partial: String, participants: [String], knownAgents: [String]) -> [String] {
    let p = partial.lowercased()
    var seen = Set<String>()
    var out: [String] = []
    for name in participants + knownAgents where name.lowercased().hasPrefix(p) {
        if seen.insert(name).inserted { out.append(name) }
    }
    return out
}

/// Replace the trailing "@partial" with "@chosen " in the draft. Only rewrites a
/// boundary-anchored token (same rule as `mentionToken`), so it never mangles an
/// email address the user happens to be typing.
func applyMention(_ draft: String, choosing name: String) -> String {
    guard let at = mentionAtIndex(in: draft) else { return draft }
    return String(draft[..<at]) + "@" + name + " "
}
