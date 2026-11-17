// xeet-vision-ocr: Apple Vision text recognition speaking the xeet-ocr/1
// protocol (see crates/xeet-ocr/src/lib.rs), so the Rust `OcrProvider`
// can use it as a `HelperBackend`.
//
//   xeet-vision-ocr --identify     prints EngineInfo JSON
//   xeet-vision-ocr IMAGE          prints OcrResult JSON
//
// Text recognition only. Boxes are normalized with a TOP-LEFT origin (Vision
// reports bottom-left; converted here). Vision reports one confidence per
// line; words carry their line's confidence, and that is declared.
//
// UNVERIFIED: never compiled or run (written without the macOS SDK).

import Foundation
import ImageIO
import Vision

struct EngineInfo: Encodable {
    let name: String
    let version: String
    let languages: [String]
    let config: [String: String]
    let formats: [String]
    let platform: String
    let unsupported: [String]
}

struct Word: Encodable {
    let text: String
    let box: [Double]
    let confidence: Double
}

struct Line: Encodable {
    let text: String
    let box: [Double]
    let confidence: Double
    var order: Int
    let words: [Word]
}

struct ImageInfo: Encodable {
    let width: Int
    let height: Int
    let orientation: String
}

struct OcrResult: Encodable {
    let `protocol`: String
    let engine: EngineInfo
    let image: ImageInfo
    let readingOrder: String
    let lines: [Line]
    let warnings: [String]
}

func makeRequest() -> VNRecognizeTextRequest {
    let r = VNRecognizeTextRequest()
    r.recognitionLevel = .accurate
    r.usesLanguageCorrection = true
    r.automaticallyDetectsLanguage = true
    return r
}

func engineInfo() -> EngineInfo {
    let r = makeRequest()
    let languages = (try? r.supportedRecognitionLanguages()) ?? []
    let os = ProcessInfo.processInfo.operatingSystemVersion
    let osVersion = "\(os.majorVersion).\(os.minorVersion).\(os.patchVersion)"
    return EngineInfo(
        name: "apple-vision",
        version: "VNRecognizeTextRequest-r\(r.revision)-macOS-\(osVersion)",
        languages: languages,
        config: [
            "recognitionLevel": "accurate",
            "usesLanguageCorrection": "true",
            "automaticallyDetectsLanguage": "true",
            "revision": String(r.revision),
        ],
        formats: ["image/png", "image/jpeg", "image/heic", "image/tiff", "image/gif"],
        platform: "macos",
        unsupported: [
            "per-word confidence (Vision reports one confidence per line; words carry their line's)",
            "engine reading order (lines are ordered geometrically: top to bottom, then left to right)",
            "scene or object understanding",
        ]
    )
}

func emit<T: Encodable>(_ value: T) {
    let enc = JSONEncoder()
    enc.outputFormatting = [.sortedKeys]
    let data = try! enc.encode(value)
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write("\n".data(using: .utf8)!)
}

func fail(_ message: String) -> Never {
    FileHandle.standardError.write("xeet-vision-ocr: \(message)\n".data(using: .utf8)!)
    exit(1)
}

func round4(_ v: Double) -> Double { (v * 10_000).rounded() / 10_000 }

/// Vision's normalized rect (origin bottom-left) → [x, y, w, h] top-left.
func topLeft(_ r: CGRect) -> [Double] {
    [round4(r.minX), round4(1 - r.maxY), round4(r.width), round4(r.height)]
}

func orientationName(_ o: CGImagePropertyOrientation) -> String {
    switch o {
    case .up: return "up"
    case .down: return "down"
    case .left: return "left"
    case .right: return "right"
    case .upMirrored: return "up-mirrored"
    case .downMirrored: return "down-mirrored"
    case .leftMirrored: return "left-mirrored"
    case .rightMirrored: return "right-mirrored"
    @unknown default: return "unknown"
    }
}

let args = CommandLine.arguments
if args.count == 2 && args[1] == "--identify" {
    emit(engineInfo())
    exit(0)
}
guard args.count == 2 else { fail("usage: xeet-vision-ocr --identify | IMAGE") }

let url = URL(fileURLWithPath: args[1])
guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
      let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
else { fail("cannot decode \(args[1])") }

var orientation = CGImagePropertyOrientation.up
if let props = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
   let raw = props[kCGImagePropertyOrientation] as? UInt32,
   let o = CGImagePropertyOrientation(rawValue: raw) {
    orientation = o
}

let request = makeRequest()
let handler = VNImageRequestHandler(cgImage: image, orientation: orientation, options: [:])
do {
    try handler.perform([request])
} catch {
    fail("recognition failed: \(error.localizedDescription)")
}

var lines: [Line] = []
for observation in request.results ?? [] {
    guard let candidate = observation.topCandidates(1).first else { continue }
    let text = candidate.string
    let confidence = round4(Double(candidate.confidence))
    var words: [Word] = []
    var index = text.startIndex
    for token in text.split(separator: " ", omittingEmptySubsequences: true) {
        guard let range = text.range(of: token, range: index..<text.endIndex) else { continue }
        index = range.upperBound
        if let rect = try? candidate.boundingBox(for: range)?.boundingBox {
            words.append(Word(text: String(token), box: topLeft(rect), confidence: confidence))
        }
    }
    lines.append(Line(text: text, box: topLeft(observation.boundingBox), confidence: confidence, order: 0, words: words))
}
// Geometric reading order: by line top (to 1% of the height), then left edge.
lines.sort { a, b in
    let ya = (a.box[1] * 100).rounded(), yb = (b.box[1] * 100).rounded()
    return ya == yb ? a.box[0] < b.box[0] : ya < yb
}
for i in lines.indices { lines[i].order = i }

emit(OcrResult(
    protocol: "xeet-ocr/1",
    engine: engineInfo(),
    image: ImageInfo(width: image.width, height: image.height, orientation: orientationName(orientation)),
    readingOrder: "geometric",
    lines: lines,
    warnings: []
))
