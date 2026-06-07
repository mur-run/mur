import SwiftUI

@main
struct MurVoiceApp: App {
    @State private var model = AppModel()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environment(model)
                .onOpenURL { url in
                    handleURL(url)
                }
        }
    }

    /// Handles `mur://pair?host=HOST&port=PORT&token=TOKEN&agent=AGENT`
    private func handleURL(_ url: URL) {
        print("[MurVoice] handleURL: \(url.absoluteString)")
        guard url.scheme == "mur",
              url.host == "pair",
              let comps = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let items = comps.queryItems else { return }
        let q = Dictionary(uniqueKeysWithValues: items.compactMap { i in
            i.value.map { (i.name, $0) }
        })
        guard let host = q["host"], let portStr = q["port"],
              let port = UInt16(portStr), let token = q["token"] else { return }
        model.connect(host: host, port: port, token: token)
    }
}
