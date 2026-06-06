import SwiftUI

/// The 椋鳥 (starling) mascot.
///
/// **Placeholder implementation (P2).** This is a pure-SwiftUI stand-in so the
/// app builds and the state machine is fully wired *before* the real artwork
/// exists. In P5 this view is replaced by a Rive `RiveViewModel` driving a
/// `.riv` state machine — the inputs map 1:1 to what we feed here:
///   • `state`  ← `MascotState` (idle/listening/thinking/speaking/error)
///   • `level`  ← `micLevel` (live RMS, 0...1) → body bob / amplitude ring
///   • `viseme` ← TTS visemes (P3) for lip-sync while speaking
///   • `onTouch`← tap trigger (bounce + chirp)
/// See docs/superpowers/specs/2026-06-05-mur-voice-mobile-app-design.md.
struct StarlingMascot: View {
    let state: MascotState
    /// Live mic amplitude 0...1 (drives the listening bob/ring).
    var micLevel: Double = 0

    @State private var breathe = false
    @State private var bounce = false
    @State private var spin = false

    var body: some View {
        ZStack {
            // Amplitude ring while listening.
            Circle()
                .stroke(Color.murOrange.opacity(0.35), lineWidth: 6)
                .scaleEffect(1.0 + (state.isCapturing ? micLevel * 0.6 : 0))
                .opacity(state.isCapturing ? 1 : 0)
                .animation(.easeOut(duration: 0.12), value: micLevel)

            Image(systemName: symbol)
                .font(.system(size: 80, weight: .light))
                .foregroundStyle(symbolColor)
                .scaleEffect(bounce ? 1.18 : (breathe ? 1.03 : 1.0))
                .rotationEffect(.degrees(tilt))
                .rotationEffect(.degrees(spin ? 360 : 0))
                .accessibilityLabel("MUR starling, \(state.label)")
        }
        .frame(width: 200, height: 200)
        .contentShape(Circle())
        .onTapGesture { onTouch() }
        .onAppear {
            withAnimation(.easeInOut(duration: 1.6).repeatForever(autoreverses: true)) {
                breathe = true
            }
        }
        .onChange(of: state) { _, newValue in
            if newValue == .thinking {
                withAnimation(.linear(duration: 1.1).repeatForever(autoreverses: false)) {
                    spin = true
                }
            } else {
                spin = false
            }
        }
    }

    /// SF Symbol stand-in; swapped for the Rive bird in P5.
    private var symbol: String {
        switch state {
        case .offline:   return "bird"
        case .idle:      return "bird.fill"
        case .listening: return "waveform"
        case .thinking:  return "bird"
        case .speaking:  return "bird.fill"
        case .error:     return "exclamationmark.triangle.fill"
        }
    }

    private var symbolColor: Color {
        switch state {
        case .offline:   return .secondary
        case .error:     return .red
        default:         return .murOrange
        }
    }

    private var tilt: Double {
        switch state {
        case .listening: return 12     // head cock
        case .error:     return -8     // droop
        default:         return 0
        }
    }

    private func onTouch() {
        withAnimation(.spring(response: 0.25, dampingFraction: 0.4)) { bounce = true }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) {
            withAnimation(.spring(response: 0.3, dampingFraction: 0.6)) { bounce = false }
        }
        Haptics.tap()
    }
}
