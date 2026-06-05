import SwiftUI
import AVFoundation

/// Parsed contents of a `mur-pair://host:port/?token=…&agent=…` pairing URI
/// (printed by `mur agent pair` on the Mac).
struct PairingInfo: Equatable {
    let host: String
    let port: UInt16
    let token: String
    let agent: String

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
    }
}

/// Live-camera QR scanner. Calls `onScan` once with the first decoded payload.
struct QRScannerView: UIViewControllerRepresentable {
    var onScan: (String) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(onScan: onScan) }

    func makeUIViewController(context: Context) -> ScannerController {
        let vc = ScannerController()
        vc.coordinator = context.coordinator
        return vc
    }

    func updateUIViewController(_ uiViewController: ScannerController, context: Context) {}

    final class Coordinator: NSObject, AVCaptureMetadataOutputObjectsDelegate {
        let onScan: (String) -> Void
        private var fired = false
        init(onScan: @escaping (String) -> Void) { self.onScan = onScan }

        func metadataOutput(_ output: AVCaptureMetadataOutput,
                            didOutput metadataObjects: [AVMetadataObject],
                            from connection: AVCaptureConnection) {
            guard !fired,
                  let obj = metadataObjects.first as? AVMetadataMachineReadableCodeObject,
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

            let preview = AVCaptureVideoPreviewLayer(session: session)
            preview.videoGravity = .resizeAspectFill
            preview.frame = view.layer.bounds
            view.layer.addSublayer(preview)
            self.preview = preview
        }

        override func viewWillAppear(_ animated: Bool) {
            super.viewWillAppear(animated)
            if !session.isRunning {
                DispatchQueue.global(qos: .userInitiated).async { self.session.startRunning() }
            }
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

/// Pairing sheet: scan a QR, or enter the address/token by hand (useful on the
/// simulator, which has no camera).
struct PairingSheet: View {
    var onPaired: (PairingInfo) -> Void
    @Environment(\.dismiss) private var dismiss

    @State private var manualHost = ""
    @State private var manualPort = "9430"
    @State private var manualToken = ""
    @State private var scanError: String?

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                QRScannerView { value in
                    if let info = PairingInfo(uri: value) {
                        onPaired(info)
                        dismiss()
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
                    Section("Or enter manually") {
                        TextField("Host (e.g. 192.168.1.20)", text: $manualHost)
                            .textInputAutocapitalization(.never).autocorrectionDisabled()
                        TextField("Port", text: $manualPort).keyboardType(.numberPad)
                        TextField("Token", text: $manualToken)
                            .textInputAutocapitalization(.never).autocorrectionDisabled()
                        Button("Connect") {
                            guard let port = UInt16(manualPort), !manualHost.isEmpty, !manualToken.isEmpty
                            else { scanError = "Fill in host, port and token."; return }
                            onPaired(PairingInfo(uri: "mur-pair://\(manualHost):\(port)/?token=\(manualToken)&agent=mur")!)
                            dismiss()
                        }
                    }
                    if let scanError { Text(scanError).foregroundStyle(.red).font(.footnote) }
                }
                .frame(height: 260)
            }
            .navigationTitle("Pair with MUR")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } } }
        }
    }
}
