// swift-tools-version:5.9
// Xeet for macOS: the screenshot front door and the Apple Vision OCR helper.
//
// UNVERIFIED: written in a Linux environment without Xcode; it has been
// syntax-checked with `swiftc -parse` only, never compiled against the macOS
// SDK or run. See docs/CAPTURE.md for the manual acceptance tests.
import PackageDescription

let package = Package(
    name: "XeetMac",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "xeet-vision-ocr", targets: ["XeetVisionOCR"]),
        .executable(name: "XeetCapture", targets: ["XeetCapture"]),
    ],
    targets: [
        .executableTarget(name: "XeetVisionOCR", path: "Sources/XeetVisionOCR"),
        .executableTarget(name: "XeetCapture", path: "Sources/XeetCapture"),
    ]
)
