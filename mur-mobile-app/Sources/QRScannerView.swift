import SwiftUI
import AVFoundation
import Network

/// Parsed contents of a `mur-pair://host:port/?wid=…&token=…&did=…&agent=…&v=2`
/// pairing URI. `wid`+`did` are present for the proof handshake (v=2); when
/// absent (older QR) the app falls back to the legacy bearer path.
struct PairingInfo: Equatable {
    let host: String
    let port: UInt16
    let token: String
    let agent: String
    /// Pairing-window id (v=2 proof handshake); nil for a legacy QR.
    let wid: String?
    /// Daemon Ed25519 identity, cross-checked during the proof handshake to bind
    /// the endpoint on the TLS-less LAN; nil for a legacy QR.
    let did: String?

    init?(uri: String) {
        guard let comps = URLComponents(string: uri),
              comps.scheme == "mur-pair",
              let host = comps.host, !host.isEmpty,
              let port = comps.port, port > 0, port <= 65535
        else { return nil }
        let items = comps.queryItems ?? []
        guard let token = items.first(where: { $0.name == "token" })?.value, !token.isEmpty
        else { return nil }
        self.host = host
        self.port = UInt16(port)
        self.token = token
        self.agent = items.first(where: { $0.name == "agent" })?.value ?? "mur"
        self.wid = items.first(where: { $0.name == "wid" })?.value.flatMap { $0.isEmpty ? nil : $0 }
        self.did = items.first(where: { $0.name == "did" })?.value.flatMap { $0.isEmpty ? nil : $0 }
    }
}

// MARK: - Bonjour discovery

/// Discovers `_mur._tcp` services on the LAN and vends `PairingInfo` for each.
@Observable
final class BonjourDiscovery {
    private(set) var services: [PairingInfo] = []
    private var browser: NWBrowser?

    func start() {
        let params = NWParameters()
        params.includePeerToPeer = true
        let browser = NWBrowser(for: .bonjourWithTXTRecord(type: "_mur._tcp", domain: nil), using: params)
        self.browser = browser
        browser.stateUpdateHandler = { _ in }
        browser.browseResultsChangedHandler = { [weak self] results, _ in
            Task { @MainActor [weak self] in
                self?.handleResults(results)
            }
        }
        browser.start(queue: .main)
    }

    func stop() { browser?.cancel(); browser = nil }

    @MainActor
    private func handleResults(_ results: Set<NWBrowser.Result>) {
        services = results.compactMap { result in
            guard case let .service(name, _, _, _) = result.endpoint else { return nil }
            guard case let .bonjour(record) = result.metadata else { return nil }
            let txt = record.dictionary
            guard let token = txt["token"], let agentName = txt["agent"] else { return nil }

            // Resolve host synchronously by connecting; for now build a best-effort URI.
            // The host will be filled in once NWConnection resolves it.
            let _ = name  // instance name (e.g. "MUR on MacBook")
            let portStr = txt["port"] ?? "9430"
            let port = UInt16(portStr) ?? 9430

            // Extract host from the endpoint description as a fallback.
            let host = endpointHost(result.endpoint) ?? name
            return PairingInfo(uri: "mur-pair://\(host):\(port)/?token=\(token)&agent=\(agentName)")
        }
    }

    private func endpointHost(_ endpoint: NWEndpoint) -> String? {
        if case let .service(_, type_, domain, _) = endpoint {
            return "\(type_).\(domain)"
        }
        return nil
    }
}

// MARK: - QR Scanner

struct QRScannerView: UIViewControllerRepresentable {
    var onScan: (String) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(onScan: onScan) }
    func makeUIViewController(context: Context) -> ScannerController {
        let vc = ScannerController(); vc.coordinator = context.coordinator; return vc
    }
    func updateUIViewController(_ vc: ScannerController, context: Context) {}

    final class Coordinator: NSObject, AVCaptureMetadataOutputObjectsDelegate {
        let onScan: (String) -> Void
        private var fired = false
        init(onScan: @escaping (String) -> Void) { self.onScan = onScan }

        func metadataOutput(_ output: AVCaptureMetadataOutput,
                            didOutput objects: [AVMetadataObject],
                            from connection: AVCaptureConnection) {
            guard !fired,
                  let obj = objects.first as? AVMetadataMachineReadableCodeObject,
                  let value = obj.stringValue else { return }
            fired = true
            DispatchQueue.main.async { self.onScan(value) }
        }
    }

    final class ScannerController: UIViewController {
        weak var coordinator: Coordinator?
        private let session = AVCaptureSession()
        private var preview: AVCaptureVideoPreviewLayer?

        override func viewDidLoad() {
            super.viewDidLoad()
            view.backgroundColor = .black
            guard let device = AVCaptureDevice.default(for: .video),
                  let input = try? AVCaptureDeviceInput(device: device),
                  session.canAddInput(input) else { return }
            session.addInput(input)
            let output = AVCaptureMetadataOutput()
            guard session.canAddOutput(output) else { return }
            session.addOutput(output)
            output.setMetadataObjectsDelegate(coordinator, queue: .main)
            output.metadataObjectTypes = [.qr]
            let layer = AVCaptureVideoPreviewLayer(session: session)
            layer.videoGravity = .resizeAspectFill
            layer.frame = view.layer.bounds
            view.layer.addSublayer(layer)
            self.preview = layer
        }
        override func viewWillAppear(_ animated: Bool) {
            super.viewWillAppear(animated)
            if !session.isRunning { DispatchQueue.global(qos: .userInitiated).async { self.session.startRunning() } }
        }
        override func viewWillDisappear(_ animated: Bool) {
            super.viewWillDisappear(animated)
            if session.isRunning { session.stopRunning() }
        }
        override func viewDidLayoutSubviews() {
            super.viewDidLayoutSubviews()
            preview?.frame = view.layer.bounds
        }
    }
}

// MARK: - Pairing Sheet

struct PairingSheet: View {
    var onPaired: (PairingInfo) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var discovery = BonjourDiscovery()
    @State private var manualHost = ""
    @State private var manualPort = "9430"
    @State private var manualToken = ""
    @State private var scanError: String?

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                QRScannerView { value in
                    if let info = PairingInfo(uri: value) {
                        onPaired(info); dismiss()
                    } else {
                        scanError = "That QR isn't a MUR pairing code."
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .overlay(alignment: .top) {
                    Text("Scan the QR from `mur agent pair` on your Mac")
                        .font(.callout).padding(8)
                        .background(.ultraThinMaterial, in: Capsule())
                        .padding(.top, 12)
                }

                Form {
                    if !discovery.services.isEmpty {
                        Section("Nearby MUR") {
                            ForEach(discovery.services, id: \.host) { info in
                                Button {
                                    onPaired(info); dismiss()
                                } label: {
                                    Label("MUR on \(info.host)", systemImage: "laptopcomputer.and.iphone")
                                }
                            }
                        }
                    }
                    Section("Enter manually") {
                        TextField("Host (e.g. 192.168.1.20)", text: $manualHost)
                            .textInputAutocapitalization(.never).autocorrectionDisabled()
                        TextField("Port", text: $manualPort).keyboardType(.numberPad)
                        TextField("Token", text: $manualToken)
                            .textInputAutocapitalization(.never).autocorrectionDisabled()
                        Button("Connect") {
                            guard let port = UInt16(manualPort),
                                  !manualHost.isEmpty, !manualToken.isEmpty
                            else { scanError = "Fill in host, port and token."; return }
                            onPaired(PairingInfo(uri: "mur-pair://\(manualHost):\(port)/?token=\(manualToken)&agent=mur")!)
                            dismiss()
                        }
                    }
                    if let e = scanError { Text(e).foregroundStyle(.red).font(.footnote) }
                }
                .frame(height: 300)
            }
            .navigationTitle("Pair with MUR")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } } }
        }
        .onAppear { discovery.start() }
        .onDisappear { discovery.stop() }
    }
}
