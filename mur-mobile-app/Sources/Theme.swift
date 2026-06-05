import SwiftUI

extension Color {
    /// Brand orange for the primary "speak" button (spec: orange CTA).
    static let murOrange = Color(red: 0.96, green: 0.49, blue: 0.20)
    /// MUR blue-starling brand accent.
    static let murBlue = Color(red: 0.20, green: 0.42, blue: 0.78)
}

enum Haptics {
    static func tap() {
        #if canImport(UIKit)
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        #endif
    }
    static func start() {
        #if canImport(UIKit)
        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        #endif
    }
    static func toggle() {
        #if canImport(UIKit)
        UINotificationFeedbackGenerator().notificationOccurred(.success)
        #endif
    }
}

#if canImport(UIKit)
import UIKit
#endif
