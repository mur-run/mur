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

    @State private var pressed = false

    var body: some View {
        ZStack {
            Circle()
                .fill(Color.murOrange)
                .shadow(color: Color.murOrange.opacity(0.5), radius: pressed ? 4 : 14, y: 4)
                .scaleEffect(pressed ? 0.94 : 1.0)
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
        .accessibilityLabel("Speak. Hold to talk; triple-tap for hands-free.")
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
}
