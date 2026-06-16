import SwiftUI

/// Event feed for one channel (the phone projection of the Hub Work view's three
/// panes, collapsed to one timeline). Live-refreshed while connected. v4c adds a
/// compose bar (drop a turn into THIS channel + @mention autocomplete) and makes
/// the HITL card actionable (approve/deny releases the gate).
struct ChannelDetailView: View {
    @Environment(AppModel.self) private var model
    let channelId: String
    @State private var draft = ""

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 8) {
                    ForEach(model.detailEvents) { ev in
                        ChannelEventRow(event: ev) { allow in
                            model.respondHitl(channelId: channelId, hitlId: ev.hitlId, allow: allow)
                        }
                        .id(ev.id)
                    }
                }
                .padding()
                .onChange(of: model.detailEvents.count) { _, _ in
                    if let last = model.detailEvents.last {
                        withAnimation { proxy.scrollTo(last.id, anchor: .bottom) }
                    }
                }
            }
        }
        .navigationTitle("Channel")
        .navigationBarTitleDisplayMode(.inline)
        .safeAreaInset(edge: .bottom) { composeBar }
        .onAppear { model.openChannel(channelId) }
        .onDisappear { model.closeChannel() }
    }

    /// Bottom compose bar: a trailing "@partial" surfaces an autocomplete strip
    /// of participants/known agents (advisory scoping hint to the concierge — it
    /// never opens a phone→specialist socket).
    @ViewBuilder private var composeBar: some View {
        VStack(spacing: 4) {
            if let partial = mentionToken(in: draft) {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack {
                        ForEach(mentionCandidates(partial: partial,
                                                  participants: model.detailParticipants,
                                                  knownAgents: model.mentionableAgents), id: \.self) { name in
                            Button("@\(name)") { draft = applyMention(draft, choosing: name) }
                                .font(.caption).buttonStyle(.bordered)
                        }
                    }
                    .padding(.horizontal)
                }
            }
            HStack(spacing: 8) {
                TextField("Message this channel…", text: $draft)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(sendDraft)
                Button(action: sendDraft) {
                    Image(systemName: "paperplane.fill")
                }
                .disabled(draft.trimmingCharacters(in: .whitespaces).isEmpty)
            }
            .padding(.horizontal).padding(.bottom, 8)
        }
        .background(.bar)
    }

    private func sendDraft() {
        model.sendToChannel(draft, channelId: channelId)
        draft = ""
    }
}

struct ChannelEventRow: View {
    let event: AppModel.ChannelEventVM
    /// Called when the user approves (`true`) / denies (`false`) a HITL gate.
    /// Defaults to a no-op so non-detail callers can construct a row plainly.
    var onRespond: (Bool) -> Void = { _ in }
    @State private var expanded = false

    var body: some View {
        switch eventVariant(actorKind: event.actorKind, kind: event.kind) {
        case .userMessage:
            bubble(text: event.text, color: .murBlue, alignment: .trailing)
        case .agentMessage:
            VStack(alignment: .leading, spacing: 2) {
                Text(actorLabel(actorKind: event.actorKind, actorName: event.actorName))
                    .font(.caption2).foregroundStyle(.secondary)
                bubble(text: event.text, color: .murOrange, alignment: .leading)
            }
        case .note:
            Text(event.text).font(.footnote).italic().foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .center)
        case .state, .delegation:
            Text(separatorText).font(.caption2).foregroundStyle(.tertiary)
                .frame(maxWidth: .infinity, alignment: .center)
        case .tool:
            DisclosureGroup(isExpanded: $expanded) {
                Text(event.text).font(.caption.monospaced()).foregroundStyle(.secondary)
            } label: {
                Label(eventKindLabel(event.kind), systemImage: "wrench.and.screwdriver")
                    .font(.caption)
            }
        case .artifact:
            card(title: "Artifact", body: event.text, accent: .murBlue)
        case .hitl:
            hitlCard
        case .other:
            card(title: eventKindLabel(event.kind), body: event.text, accent: .gray)
        }
    }

    private var separatorText: String {
        event.text.isEmpty ? eventKindLabel(event.kind) : event.text
    }

    private func bubble(text: String, color: Color, alignment: Alignment) -> some View {
        Text(text)
            .padding(10)
            .background(color.opacity(0.15), in: RoundedRectangle(cornerRadius: 12))
            .frame(maxWidth: .infinity, alignment: alignment)
    }

    private func card(title: String, body: String, accent: Color) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption.weight(.semibold)).foregroundStyle(accent)
            if !body.isEmpty { Text(body).font(.footnote) }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(accent.opacity(0.4)))
    }

    // HITL: actionable in v4c. Approve/deny calls `onRespond`, which routes to the
    // daemon; it writes a v3d-signed HitlResponse that the v3c gate verifies and
    // releases. The feed live-updates (gate appends + channel.updated push), so the
    // card resolves on approval. Buttons disable if the event carries no hitl_id.
    private var hitlCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Approval needed", systemImage: "exclamationmark.shield")
                .font(.subheadline.weight(.semibold)).foregroundStyle(Color.murOrange)
            if !event.text.isEmpty { Text(event.text).font(.footnote) }
            HStack {
                Button("Approve") { onRespond(true) }
                    .buttonStyle(.borderedProminent).tint(.green)
                Button("Deny") { onRespond(false) }
                    .buttonStyle(.bordered).tint(.red)
            }
            .disabled(event.hitlId.isEmpty)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.murOrange.opacity(0.12), in: RoundedRectangle(cornerRadius: 12))
    }
}
