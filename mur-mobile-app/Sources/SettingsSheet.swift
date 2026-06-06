import SwiftUI

/// Settings sheet: relay config + speech locale.
/// Accessible from the header gear button in ContentView.
struct SettingsSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @AppStorage("relayApiKey") private var relayApiKey: String = ""
    @AppStorage("relayPairToken") private var relayPairToken: String = ""

    private let relayWsUrl = "wss://relay.mur.run/api/v1/relay/mobile/ws"

    var body: some View {
        NavigationStack {
            Form {
                Section("Speech language") {
                    Picker("Language", selection: Binding(
                        get: { model.speechLocale },
                        set: { model.setSpeechLocale($0) }
                    )) {
                        ForEach(AppModel.availableLocales, id: \.identifier) { locale in
                            Text(localeName(locale))
                                .tag(locale)
                        }
                    }
                }

                Section {
                    SecureField("MUR API key (mur_…)", text: $relayApiKey)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    TextField("Pairing token from QR code", text: $relayPairToken)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Button("Connect via relay") {
                        guard !relayApiKey.isEmpty, !relayPairToken.isEmpty else { return }
                        model.connectRelay(
                            relayWsUrl: relayWsUrl,
                            apiKey: relayApiKey,
                            pairToken: relayPairToken
                        )
                        dismiss()
                    }
                    .disabled(relayApiKey.isEmpty || relayPairToken.isEmpty)
                } header: {
                    Text("Relay (connect away from home Wi-Fi)")
                } footer: {
                    Text("Get your API key at app.mur.run → Settings → API Keys.")
                        .font(.footnote)
                }

                if model.isConnected {
                    Section {
                        Button("Disconnect", role: .destructive) {
                            model.disconnect()
                        }
                    }
                }
            }
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

    private func localeName(_ locale: Locale) -> String {
        let langCode = locale.language.languageCode?.identifier ?? ""
        return Locale.current.localizedString(forLanguageCode: langCode)
            ?? locale.localizedString(forIdentifier: locale.identifier)
            ?? locale.identifier
    }
}
