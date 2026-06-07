// RiveStarlingMascot.swift
//
// STUB — wiring only. Uncomment and activate once the `.riv` artwork arrives.
//
// To activate:
//   1. Drop `Starling.riv` into the Xcode project bundle.
//   2. Add the RiveRuntime Swift Package (https://github.com/rive-app/rive-ios).
//   3. Uncomment this file's body.
//   4. Replace `StarlingMascot` with `RiveStarlingMascot` in ContentView.swift.
//
// The Rive state machine expects these inputs (names must match the artboard):
//   Number  "Level"      — mic RMS 0.0…1.0 (update on every audio tap)
//   Trigger "Bounce"     — finger tap on the mascot
//   State   "MascotState"— set to one of: "offline","idle","listening",
//                          "thinking","speaking","error"
//
// import RiveRuntime
// import SwiftUI
//
// struct RiveStarlingMascot: View {
//     let state: MascotState
//     var micLevel: Double = 0
//
//     @StateObject private var rvm = RiveViewModel(
//         fileName: "Starling",
//         stateMachineName: "StarlingMachine"
//     )
//
//     var body: some View {
//         rvm.view()
//             .frame(width: 200, height: 200)
//             .onChange(of: state) { _, newState in
//                 rvm.setInput("MascotState", value: newState.riveStateName)
//             }
//             .onChange(of: micLevel) { _, level in
//                 rvm.setInput("Level", value: Float(level))
//             }
//             .accessibilityLabel("MUR Starling")
//             .accessibilityValue(state.label)
//             .accessibilityHint("Tap to chirp")
//     }
// }
//
// extension MascotState {
//     var riveStateName: String {
//         switch self {
//         case .offline:   return "offline"
//         case .idle:      return "idle"
//         case .listening: return "listening"
//         case .thinking:  return "thinking"
//         case .speaking:  return "speaking"
//         case .error:     return "error"
//         }
//     }
// }
