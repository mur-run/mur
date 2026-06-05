import SwiftUI

struct ContentView: View {
    @Environment(AppModel.self) private var model
    @State private var showPairing = false
    @State private var draft = ""

    var body: some View {
        ScrollView {
            VStack(spacing: 16) {
                header

                StarlingMascot(state: model.mascot, micLevel: model.micLevel)
                    .padding(.top, 4)

                transcriptView
                statusLine

                OrangeButton(
                    state: model.mascot,
                    micMode: model.micMode,
                    onPressStart: { model.beginCapture() },
                    onPressEnd: { model.endCaptureAndSend() },
                    onTripleTap: { model.toggleMicMode() }
                )
                .padding(.bottom, 8)
            }
            .padding()
        }
        .scrollDismissesKeyboard(.interactively)
        .safeAreaInset(edge: .bottom) {
            typeBar
                .padding(.horizontal)
                .padding(.vertical, 8)
                .background(.bar)
        }
        .sheet(isPresented: $showPairing) {
            PairingSheet { info in
                model.connect(host: info.host, port: info.port, token: info.token)
            }
        }
        .onAppear { model.start() }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Circle()
                .fill(model.isConnected ? Color.green : Color.gray)
                .frame(width: 10, height: 10)
            Text(model.isConnected ? "MUR · \(model.connectedAgent ?? "")" : "Not paired")
                .font(.footnote).foregroundStyle(.secondary)
            Spacer()
            Menu {
                ForEach(AppModel.availableLocales, id: \.identifier) { locale in
                    Button {
                        model.setSpeechLocale(locale)
                    } label: {
                        let name = locale.localizedString(forIdentifier: locale.identifier) ?? locale.identifier
                        if locale == model.speechLocale {
                            Label(name, systemImage: "checkmark")
                        } else {
                            Text(name)
                        }
                    }
                }
            } label: {
                Text(model.speechLocaleLabel)
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Button(model.isConnected ? "Re-pair" : "Pair") { showPairing = true }
                .font(.footnote.weight(.semibold))
        }
    }

    private var transcriptView: some View {
        ScrollViewReader { proxy in
            LazyVStack(alignment: .leading, spacing: 8) {
                ForEach(model.transcript) { line in
                    bubble(line).id(line.id)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .onChange(of: model.transcript.count) { _, _ in
                if let last = model.transcript.last {
                    withAnimation { proxy.scrollTo(last.id, anchor: .bottom) }
                }
            }
        }
    }

    private func bubble(_ line: AppModel.ChatLine) -> some View {
        let isUser = line.role == "user"
        return Text(line.text)
            .padding(10)
            .background(
                (isUser ? Color.murBlue : Color.murOrange).opacity(0.15),
                in: RoundedRectangle(cornerRadius: 12)
            )
            .frame(maxWidth: .infinity, alignment: isUser ? .trailing : .leading)
    }

    @ViewBuilder private var statusLine: some View {
        if !model.partial.isEmpty {
            Text(model.partial).italic().foregroundStyle(.secondary)
        } else if case let .error(message) = model.mascot {
            Text(message).font(.footnote).foregroundStyle(.red)
        }
    }

    private var typeBar: some View {
        HStack {
            TextField("Type a message…", text: $draft)
                .textFieldStyle(.roundedBorder)
                .disabled(!model.isConnected)
                .submitLabel(.send)
                .onSubmit {
                    guard !draft.trimmingCharacters(in: .whitespaces).isEmpty else { return }
                    model.sendTyped(draft)
                    draft = ""
                }
            Button {
                model.sendTyped(draft)
                draft = ""
            } label: {
                Image(systemName: "paperplane.fill")
            }
            .disabled(draft.trimmingCharacters(in: .whitespaces).isEmpty || !model.isConnected)
        }
    }
}
