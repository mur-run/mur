import SwiftUI

/// The home's Channel list (below the talk zone). Cards are NavigationLinks into
/// the channel-detail feed. Sorted input-required-first via `sortedChannels`.
struct ChannelListView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        LazyVStack(spacing: 8) {
            ForEach(sortedChannels(model.channels)) { ch in
                NavigationLink(value: ch.id) {
                    ChannelCard(channel: ch)
                }
                .buttonStyle(.plain)
            }
            if model.channels.isEmpty {
                Text("No channels yet — talk to MUR above to start.")
                    .font(.footnote).foregroundStyle(.tertiary)
                    .frame(maxWidth: .infinity).padding(.vertical, 24)
            }
        }
    }
}

struct ChannelCard: View {
    let channel: AppModel.ChannelSummary

    var body: some View {
        let chip = stateChip(channel.state)
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(channel.goal.isEmpty ? channel.title : channel.goal)
                    .font(.subheadline.weight(.semibold)).lineLimit(1)
                Spacer()
                Text(chip.label)
                    .font(.caption2.weight(.semibold))
                    .padding(.horizontal, 8).padding(.vertical, 2)
                    .background(chip.color.opacity(0.18), in: Capsule())
                    .foregroundStyle(chip.color)
            }
            HStack {
                Text(channel.agents.joined(separator: ", "))
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                Spacer()
                Text("\(channel.turns) turns").font(.caption2).foregroundStyle(.tertiary)
            }
        }
        .padding(12)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(channel.goal). \(chip.label). agents \(channel.agents.joined(separator: ", "))")
    }
}
