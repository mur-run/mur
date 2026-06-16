import SwiftUI

/// Read-only event feed for one channel (the phone projection of the Hub Work
/// view's three panes, collapsed to one timeline). Live-refreshed while
/// connected. Sending INTO an arbitrary channel is v4c — this view is read +
/// HITL-display only.
struct ChannelDetailView: View {
    @Environment(AppModel.self) private var model
    let channelId: String

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 8) {
                    ForEach(model.detailEvents) { ev in
                        ChannelEventRow(event: ev).id(ev.id)
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
        .onAppear { model.openChannel(channelId) }
        .onDisappear { model.closeChannel() }
    }
}

struct ChannelEventRow: View {
    let event: AppModel.ChannelEventVM
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

    // HITL: DISPLAY only in v4b. Authoritative approve/deny needs v3c (the gate)
    // + a mobile write RPC + v3d signing for high-risk authority — deferred.
    private var hitlCard: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("Approval needed", systemImage: "exclamationmark.shield")
                .font(.subheadline.weight(.semibold)).foregroundStyle(Color.murOrange)
            if !event.text.isEmpty { Text(event.text).font(.footnote) }
            Text("Respond from the MUR desktop app.")
                .font(.caption2).foregroundStyle(.secondary)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.murOrange.opacity(0.12), in: RoundedRectangle(cornerRadius: 12))
    }
}
