import SwiftUI

/// The 椋鳥 (starling) mascot.
///
/// **P5 polished SwiftUI implementation.** Designed so a future Rive `.riv`
/// drop-in needs only to swap this view for `RiveStarlingMascot`; the inputs
/// used here map 1:1 to what the Rive state machine expects:
///   • `state`  ← `MascotState`  (idle / listening / thinking / speaking / error)
///   • `level`  ← `micLevel`     (live RMS 0...1 → amplitude ring)
///   • tap      ← bounce + chirp haptic
///
/// When `accessibilityReduceMotion` is true all continuous animations are
/// replaced by simple opacity crossfades.
struct StarlingMascot: View {
    let state: MascotState
    var micLevel: Double = 0

    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    @State private var appeared = false
    @State private var breatheScale: CGFloat = 1.0
    @State private var bounceScale: CGFloat = 1.0
    @State private var spinDegrees: Double = 0
    @State private var isSpinning = false
    @State private var thinkingPulse: Double = 1.0

    var body: some View {
        ZStack {
            amplitudeRing
            mascotBody
        }
        .frame(width: 200, height: 200)
        .scaleEffect(appeared ? 1.0 : 0.7)
        .opacity(appeared ? 1.0 : 0)
        .contentShape(Circle())
        .onTapGesture { onTouch() }
        .onAppear { startLaunchAnimation() }
        .onChange(of: state) { _, newState in applyStateAnimation(newState) }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("MUR Starling")
        .accessibilityValue(state.label)
        .accessibilityHint("Tap to chirp")
        .accessibilityAddTraits(.isImage)
    }

    // MARK: - Subviews

    private var amplitudeRing: some View {
        ZStack {
            // Outer glow ring (mic active)
            Circle()
                .stroke(symbolColor.opacity(0.18), lineWidth: 16)
                .scaleEffect(state.isCapturing ? 1.0 + micLevel * 0.45 : 0.85)
                .opacity(state.isCapturing ? 1 : 0)
                .animation(
                    reduceMotion ? .none : .easeOut(duration: 0.08),
                    value: micLevel
                )
                .animation(.easeInOut(duration: 0.3), value: state.isCapturing)

            // Inner crisp ring
            Circle()
                .stroke(symbolColor.opacity(0.55), lineWidth: 3)
                .scaleEffect(state.isCapturing ? 1.0 + micLevel * 0.25 : 0.8)
                .opacity(state.isCapturing ? 1 : 0)
                .animation(
                    reduceMotion ? .none : .easeOut(duration: 0.08),
                    value: micLevel
                )
                .animation(.easeInOut(duration: 0.3), value: state.isCapturing)

            // Thinking pulse ring (replaces spin when reduceMotion)
            if case .thinking = state, reduceMotion {
                Circle()
                    .stroke(symbolColor.opacity(thinkingPulse * 0.6), lineWidth: 4)
                    .onAppear {
                        withAnimation(.easeInOut(duration: 0.9).repeatForever(autoreverses: true)) {
                            thinkingPulse = 0.2
                        }
                    }
                    .onDisappear { thinkingPulse = 1.0 }
            }
        }
    }

    private var mascotBody: some View {
        Image(systemName: symbol)
            .font(.system(size: 80, weight: .light))
            .foregroundStyle(symbolColor)
            .scaleEffect(bounceScale * breatheScale)
            .rotationEffect(.degrees(tilt))
            .rotationEffect(.degrees(spinDegrees))
            .animation(.easeInOut(duration: 0.3), value: state)
            // colour transition
            .animation(.easeInOut(duration: 0.4), value: symbolColor)
    }

    // MARK: - Animations

    private func startLaunchAnimation() {
        if reduceMotion {
            withAnimation(.easeIn(duration: 0.2)) { appeared = true }
        } else {
            withAnimation(.spring(response: 0.5, dampingFraction: 0.62)) { appeared = true }
            startBreathing()
        }
        applyStateAnimation(state)
    }

    private func startBreathing() {
        guard !reduceMotion else { return }
        withAnimation(.easeInOut(duration: 1.8).repeatForever(autoreverses: true)) {
            breatheScale = 1.04
        }
    }

    private func applyStateAnimation(_ newState: MascotState) {
        if case .thinking = newState {
            startThinkingSpin()
        } else {
            stopThinkingSpin()
        }
    }

    private func startThinkingSpin() {
        guard !isSpinning else { return }
        isSpinning = true
        if reduceMotion { return }   // pulse ring handles it instead
        func tick() {
            guard isSpinning else { return }
            withAnimation(.linear(duration: 1.0)) {
                spinDegrees += 360
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) { tick() }
        }
        tick()
    }

    private func stopThinkingSpin() {
        isSpinning = false
        withAnimation(.easeOut(duration: 0.3)) { spinDegrees = 0 }
    }

    private func onTouch() {
        Haptics.tap()
        withAnimation(.spring(response: 0.22, dampingFraction: 0.38)) { bounceScale = 1.2 }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.22) {
            withAnimation(.spring(response: 0.3, dampingFraction: 0.6)) { bounceScale = 1.0 }
        }
    }

    // MARK: - Derived

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
        case .listening: return 12
        case .error:     return -8
        default:         return 0
        }
    }
}
