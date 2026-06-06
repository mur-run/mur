import SwiftUI

/// The orange "speech" button (spec):
///   • **hold**        → push-to-talk (press starts capture, release sends)
///   • **triple-tap**  → toggle hands-free / always-speech mode
///
/// Note (P2 shell): press-and-hold and triple-tap share one control, so a stray
/// quick tap can start+stop a zero-length capture. P3 debounces the hold
/// (only begin capture after ~250ms) to fully separate the two gestures.
struct OrangeButton: View {
    let state: MascotState
    let micMode: MicMode
    var onPressStart: () -> Void
    var onPressEnd: () -> Void
    var onTripleTap: () -> Void

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var pressed = false

    var body: some View {
        ZStack {
            Circle()
                .fill(buttonFill)
                .shadow(color: Color.murOrange.opacity(0.5), radius: pressed ? 4 : 14, y: 4)
                .scaleEffect(reduceMotion ? 1.0 : (pressed ? 0.94 : 1.0))
            VStack(spacing: 6) {
                Image(systemName: iconName).font(.system(size: 30, weight: .bold))
                Text(caption).font(.subheadline.weight(.semibold))
            }
            .foregroundStyle(.white)
        }
        .frame(width: 168, height: 168)
        .overlay(
            Circle().stroke(.white.opacity(0.9), lineWidth: micMode == .handsFree ? 4 : 0)
        )
        .contentShape(Circle())
        .gesture(
            DragGesture(minimumDistance: 0)
                .onChanged { _ in
                    guard !pressed else { return }
                    pressed = true
                    Haptics.start()
                    onPressStart()
                }
                .onEnded { _ in
                    pressed = false
                    onPressEnd()
                }
        )
        .simultaneousGesture(
            TapGesture(count: 3).onEnded {
                Haptics.toggle()
                onTripleTap()
            }
        )
        .animation(.easeOut(duration: 0.12), value: pressed)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityLabel)
        .accessibilityValue(accessibilityValue)
        .accessibilityHint(accessibilityHint)
        .accessibilityAddTraits(.isButton)
    }

    // MARK: - Derived

    private var buttonFill: Color {
        if case .offline = state { return Color.murOrange.opacity(0.45) }
        if case .error = state   { return Color.red.opacity(0.75) }
        return Color.murOrange
    }

    private var iconName: String {
        switch micMode {
        case .pushToTalk: return state.isCapturing ? "mic.fill" : "mic"
        case .handsFree:  return "infinity"
        }
    }

    private var caption: String {
        if micMode == .handsFree { return "hands-free" }
        return state.isCapturing ? "listening" : "hold"
    }

    private var accessibilityLabel: String {
        micMode == .handsFree ? "Microphone, hands-free mode" : "Speak"
    }

    private var accessibilityValue: String {
        switch state {
        case .offline:   return "Not connected"
        case .idle:      return "Ready"
        case .listening: return "Listening"
        case .thinking:  return "Processing"
        case .speaking:  return "MUR is speaking"
        case .error(let m): return "Error: \(m)"
        }
    }

    private var accessibilityHint: String {
        micMode == .handsFree
            ? "Triple tap to switch to push-to-talk."
            : "Hold to talk, release to send. Triple tap for hands-free."
    }
}
