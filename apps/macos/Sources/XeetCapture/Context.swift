// The capture context handed to `xeet capture` (xeet_capture::image::CaptureContext).
// Only what the platform reports is recorded; what it does not, or what a
// permission withholds, is left out and the permission state says why.
//
// UNVERIFIED: never compiled or run (written without the macOS SDK).

import AppKit
import ApplicationServices
import ScreenCaptureKit

enum Permissions {
    static var screenRecording: String { CGPreflightScreenCaptureAccess() ? "granted" : "denied" }

    /// Never prompts: prompting happens only when the user turns the option on.
    static var accessibility: String {
        if !Settings.includeAccessibility { return "not-requested" }
        return AXIsProcessTrusted() ? "granted" : "denied"
    }

    static func requestAccessibility() {
        let key = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
        _ = AXIsProcessTrustedWithOptions([key: true] as CFDictionary)
    }
}

enum ContextBuilder {
    static let axNodeLimit = 1500
    static let axDepthLimit = 25

    static func build(_ c: Captured) -> [String: Any] {
        let os = ProcessInfo.processInfo.operatingSystemVersionString
        let version = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "dev"
        var ctx: [String: Any] = [
            "capturedBy": ["name": "Xeet Capture", "version": version],
            "method": "screencapturekit",
            "mode": "\(c.mode)",
            "platform": "macOS \(os)",
            "display": compact([
                "id": Int(c.display.displayID),
                "name": c.displayName,
                "frame": rect(CGDisplayBounds(c.display.displayID)),
                "scale": Double(c.scale),
            ]),
            "frame": rect(c.frame),
            "permissions": ["screenRecording": Permissions.screenRecording, "accessibility": Permissions.accessibility],
        ]
        if let front = NSWorkspace.shared.frontmostApplication {
            ctx["frontmostApp"] = compact(["name": front.localizedName, "bundleId": front.bundleIdentifier, "pid": Int(front.processIdentifier)])
        }
        if let w = c.window {
            var window: [String: Any] = ["id": Int(w.windowID), "frame": rect(w.frame)]
            if let t = w.title, !t.isEmpty { window["title"] = t }
            if let app = w.owningApplication {
                window["app"] = ["name": app.applicationName, "bundleId": app.bundleIdentifier, "pid": Int(app.processID)]
            }
            ctx["window"] = window
            if Permissions.accessibility == "granted", let app = w.owningApplication {
                ctx["accessibility"] = accessibilityTree(pid: app.processID)
            }
        }
        return ctx
    }

    /// Drops nil values: JSONSerialization cannot encode Optional.none.
    static func compact(_ d: [String: Any?]) -> [String: Any] {
        d.compactMapValues { $0 }
    }

    static func rect(_ r: CGRect) -> [Double] { [r.minX, r.minY, r.width, r.height].map { Double($0) } }

    /// The focused window's accessibility tree, bounded in size and depth.
    static func accessibilityTree(pid: pid_t) -> [String: Any]? {
        let app = AXUIElementCreateApplication(pid)
        var focused: CFTypeRef?
        guard AXUIElementCopyAttributeValue(app, kAXFocusedWindowAttribute as CFString, &focused) == .success,
              let window = focused
        else { return nil }
        var count = 0
        return node(window as! AXUIElement, depth: 0, count: &count)
    }

    private static func attr(_ e: AXUIElement, _ name: String) -> CFTypeRef? {
        var v: CFTypeRef?
        return AXUIElementCopyAttributeValue(e, name as CFString, &v) == .success ? v : nil
    }

    private static func node(_ e: AXUIElement, depth: Int, count: inout Int) -> [String: Any]? {
        guard count < axNodeLimit, depth < axDepthLimit else { return nil }
        count += 1
        var n: [String: Any] = ["role": (attr(e, kAXRoleAttribute) as? String) ?? "AXUnknown"]
        if let s = attr(e, kAXSubroleAttribute) as? String { n["subrole"] = s }
        if let t = attr(e, kAXTitleAttribute) as? String, !t.isEmpty { n["title"] = t }
        if let d = attr(e, kAXDescriptionAttribute) as? String, !d.isEmpty { n["label"] = d }
        if let v = attr(e, kAXValueAttribute) as? String, !v.isEmpty { n["value"] = String(v.prefix(2000)) }
        var origin = CGPoint.zero, size = CGSize.zero
        if let p = attr(e, kAXPositionAttribute), CFGetTypeID(p) == AXValueGetTypeID(),
           let s = attr(e, kAXSizeAttribute), CFGetTypeID(s) == AXValueGetTypeID(),
           AXValueGetValue(p as! AXValue, .cgPoint, &origin), AXValueGetValue(s as! AXValue, .cgSize, &size) {
            n["frame"] = [Double(origin.x), Double(origin.y), Double(size.width), Double(size.height)]
        }
        if let children = attr(e, kAXChildrenAttribute) as? [AXUIElement] {
            n["children"] = children.compactMap { node($0, depth: depth + 1, count: &count) }
        }
        return n
    }
}
