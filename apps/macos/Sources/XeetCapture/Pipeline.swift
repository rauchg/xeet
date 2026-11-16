// After the pixels: save the original, hand it with its context to
// `xeet capture` (artifact + local OCR), then the familiar conveniences —
// clipboard and a conventional PNG — which are projections of the artifact.
//
// UNVERIFIED: never compiled or run (written without the macOS SDK).

import AppKit
import UniformTypeIdentifiers

enum Settings {
    static var copyImage: Bool {
        get { UserDefaults.standard.object(forKey: "copyImage") as? Bool ?? false }
        set { UserDefaults.standard.set(newValue, forKey: "copyImage") }
    }
    static var savePNG: Bool {
        get { UserDefaults.standard.object(forKey: "savePNG") as? Bool ?? true }
        set { UserDefaults.standard.set(newValue, forKey: "savePNG") }
    }
    static var includeAccessibility: Bool {
        get { UserDefaults.standard.bool(forKey: "includeAccessibility") }
        set { UserDefaults.standard.set(newValue, forKey: "includeAccessibility") }
    }
    /// Where conventional PNGs go (macOS's own default is the Desktop).
    static var exportFolder: URL {
        if let p = UserDefaults.standard.string(forKey: "exportFolder") { return URL(fileURLWithPath: p) }
        return FileManager.default.urls(for: .desktopDirectory, in: .userDomainMask)[0]
    }
    static var support: URL {
        let u = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("Xeet", isDirectory: true)
        try? FileManager.default.createDirectory(at: u.appendingPathComponent("captures"), withIntermediateDirectories: true)
        return u
    }
    /// The `xeet` CLI: bundled beside this app's executable, or configured.
    static var xeetBinary: URL {
        if let p = UserDefaults.standard.string(forKey: "xeetBinary") { return URL(fileURLWithPath: p) }
        return Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/xeet")
    }
}

struct CaptureReport {
    let artifact: String?
    let textRead: Bool
    let ocrLines: Int
    let uncertainLines: Int
    let notes: [String]
    let exported: URL?
    let raw: [String: Any]
}

enum Pipeline {
    static func run(_ c: Captured) throws -> CaptureReport {
        let stamp = ISO8601DateFormatter().string(from: Date()).replacingOccurrences(of: ":", with: "-")
        let dir = Settings.support.appendingPathComponent("captures")
        let png = dir.appendingPathComponent("\(stamp).png")
        let ctx = dir.appendingPathComponent("\(stamp).context.json")
        let reportURL = dir.appendingPathComponent("\(stamp).report.json")

        // 1. The original pixels, exactly as captured.
        let data = try pngData(c.image)
        try data.write(to: png)
        let context = ContextBuilder.build(c)
        try JSONSerialization.data(withJSONObject: context, options: [.sortedKeys]).write(to: ctx)

        // 2. The artifact and local OCR, through the same engine everything else uses.
        let p = Process()
        p.executableURL = Settings.xeetBinary
        p.arguments = ["capture", "--image", png.path, "--context", ctx.path,
                       "--store", Settings.support.appendingPathComponent("store").path,
                       "--grant", "store:read,store:write,fs:read,ocr:local",
                       "--allow-read", dir.path, "--report", reportURL.path]
        let err = Pipe()
        p.standardError = err
        try p.run()
        p.waitUntilExit()
        guard let reportData = try? Data(contentsOf: reportURL),
              let report = try JSONSerialization.jsonObject(with: reportData) as? [String: Any]
        else {
            let msg = String(data: err.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
            throw CaptureError.failed("xeet capture exited \(p.terminationStatus): \(msg)")
        }

        // 3. Conventional projections of the capture, as configured.
        if Settings.copyImage {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setData(data, forType: .png)
        }
        var exported: URL?
        if Settings.savePNG {
            let f = DateFormatter()
            f.dateFormat = "yyyy-MM-dd 'at' HH.mm.ss"
            let url = Settings.exportFolder.appendingPathComponent("Xeet Capture \(f.string(from: Date())).png")
            try data.write(to: url)
            exported = url
        }
        let summary = report["summary"] as? [String: Any]
        return CaptureReport(
            artifact: report["artifact"] as? String,
            textRead: report["textRead"] as? Bool ?? false,
            ocrLines: summary?["ocrLines"] as? Int ?? 0,
            uncertainLines: summary?["uncertainLines"] as? Int ?? 0,
            notes: report["notes"] as? [String] ?? [],
            exported: exported,
            raw: report
        )
    }

    static func pngData(_ image: CGImage) throws -> Data {
        let out = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(out as CFMutableData, UTType.png.identifier as CFString, 1, nil) else {
            throw CaptureError.failed("cannot create a PNG encoder")
        }
        CGImageDestinationAddImage(dest, image, nil)
        guard CGImageDestinationFinalize(dest) else { throw CaptureError.failed("PNG encoding failed") }
        return out as Data
    }
}
