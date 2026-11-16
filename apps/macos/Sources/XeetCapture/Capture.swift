// Screen, region and window capture with ScreenCaptureKit (macOS 14+).
//
// Coordinates: everything handed to the context is in Quartz global points
// (origin top-left of the main display), the space ScreenCaptureKit display
// frames and accessibility positions use. Cocoa's bottom-left screen
// coordinates appear only inside the region overlay and are converted there.
//
// UNVERIFIED: never compiled or run (written without the macOS SDK).

import AppKit
import CoreGraphics
import ScreenCaptureKit

enum CaptureError: Error, CustomStringConvertible {
    case cancelled
    case noPermission
    case noDisplay
    case noWindow
    case failed(String)

    var description: String {
        switch self {
        case .cancelled: return "Capture cancelled."
        case .noPermission: return "Xeet needs Screen Recording permission (System Settings › Privacy & Security › Screen & System Audio Recording)."
        case .noDisplay: return "No display found under the pointer."
        case .noWindow: return "No capturable window in the frontmost application."
        case .failed(let m): return "Capture failed: \(m)"
        }
    }
}

struct Captured {
    let image: CGImage
    let display: SCDisplay
    let scale: CGFloat
    let displayName: String?
    /// The captured rectangle in global points.
    let frame: CGRect
    let window: SCWindow?
    let mode: CaptureKind
}

enum Capturer {
    /// The display under the pointer, as NSScreen and its CGDirectDisplayID.
    static func screenUnderPointer() -> (NSScreen, CGDirectDisplayID)? {
        let p = NSEvent.mouseLocation
        guard let screen = NSScreen.screens.first(where: { NSMouseInRect(p, $0.frame, false) }) ?? NSScreen.main,
              let number = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber
        else { return nil }
        return (screen, CGDirectDisplayID(number.uint32Value))
    }

    static func capture(_ kind: CaptureKind) async throws -> Captured {
        guard CGPreflightScreenCaptureAccess() else {
            // Asking is what makes macOS list this app under Screen Recording.
            _ = CGRequestScreenCaptureAccess()
            if let u = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") {
                NSWorkspace.shared.open(u)
            }
            throw CaptureError.noPermission
        }
        let content: SCShareableContent
        do {
            content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        } catch {
            throw CaptureError.failed(error.localizedDescription)
        }
        guard let (screen, displayID) = await MainActor.run(body: { screenUnderPointer() }),
              let display = content.displays.first(where: { $0.displayID == displayID })
        else { throw CaptureError.noDisplay }
        let scale = await MainActor.run { screen.backingScaleFactor }
        let displayName = await MainActor.run { screen.localizedName }
        let displayFrame = CGDisplayBounds(displayID) // global points, top-left origin

        switch kind {
        case .display:
            let filter = SCContentFilter(display: display, excludingWindows: [])
            let config = SCStreamConfiguration()
            config.width = Int(CGFloat(display.width) * scale)
            config.height = Int(CGFloat(display.height) * scale)
            config.showsCursor = false
            config.captureResolution = .best
            let image = try await shoot(filter, config)
            return Captured(image: image, display: display, scale: scale, displayName: displayName, frame: displayFrame, window: nil, mode: kind)

        case .region:
            // Display-local points, top-left origin.
            let local = try await RegionSelector.select(on: screen)
            let filter = SCContentFilter(display: display, excludingWindows: [])
            let config = SCStreamConfiguration()
            config.sourceRect = local
            config.width = Int(local.width * scale)
            config.height = Int(local.height * scale)
            config.showsCursor = false
            config.captureResolution = .best
            let image = try await shoot(filter, config)
            let global = CGRect(x: displayFrame.minX + local.minX, y: displayFrame.minY + local.minY, width: local.width, height: local.height)
            return Captured(image: image, display: display, scale: scale, displayName: displayName, frame: global, window: nil, mode: kind)

        case .window:
            let pid = await MainActor.run { NSWorkspace.shared.frontmostApplication?.processIdentifier }
            guard let pid,
                  let window = content.windows.first(where: { $0.owningApplication?.processID == pid && $0.isOnScreen && $0.windowLayer == 0 })
            else { throw CaptureError.noWindow }
            let filter = SCContentFilter(desktopIndependentWindow: window)
            let config = SCStreamConfiguration()
            let pixelScale = CGFloat(filter.pointPixelScale)
            config.width = Int(filter.contentRect.width * pixelScale)
            config.height = Int(filter.contentRect.height * pixelScale)
            config.ignoreShadowsSingleWindow = true
            config.showsCursor = false
            config.captureResolution = .best
            let image = try await shoot(filter, config)
            return Captured(image: image, display: display, scale: pixelScale, displayName: displayName, frame: window.frame, window: window, mode: kind)
        }
    }

    private static func shoot(_ filter: SCContentFilter, _ config: SCStreamConfiguration) async throws -> CGImage {
        do {
            return try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
        } catch {
            throw CaptureError.failed(error.localizedDescription)
        }
    }
}

/// A full-screen transparent overlay to drag out a region; Escape cancels.
@MainActor
final class RegionSelector: NSWindow {
    private var start: NSPoint?
    private var current: NSRect = .zero
    private var continuation: CheckedContinuation<CGRect, Error>?
    private let box = NSView()

    static func select(on screen: NSScreen) async throws -> CGRect {
        try await withCheckedThrowingContinuation { (c: CheckedContinuation<CGRect, Error>) in
            let w = RegionSelector(screen: screen)
            w.continuation = c
            w.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            NSCursor.crosshair.push()
        }
    }

    init(screen: NSScreen) {
        super.init(contentRect: screen.frame, styleMask: .borderless, backing: .buffered, defer: false)
        level = .screenSaver
        isOpaque = false
        backgroundColor = NSColor.black.withAlphaComponent(0.15)
        ignoresMouseEvents = false
        box.wantsLayer = true
        box.layer?.borderColor = NSColor.white.cgColor
        box.layer?.borderWidth = 1
        box.layer?.backgroundColor = NSColor.white.withAlphaComponent(0.1).cgColor
        contentView?.addSubview(box)
    }

    override var canBecomeKey: Bool { true }

    override func mouseDown(with event: NSEvent) { start = event.locationInWindow }

    override func mouseDragged(with event: NSEvent) {
        guard let s = start else { return }
        let p = event.locationInWindow
        current = NSRect(x: min(s.x, p.x), y: min(s.y, p.y), width: abs(p.x - s.x), height: abs(p.y - s.y))
        box.frame = current
    }

    override func mouseUp(with event: NSEvent) {
        let height = frame.height
        let r = current
        finish(r.width < 4 || r.height < 4 ? .failure(CaptureError.cancelled)
               // Window coordinates are bottom-left; the display-local rect is top-left.
               : .success(CGRect(x: r.minX, y: height - r.maxY, width: r.width, height: r.height)))
    }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 { finish(.failure(CaptureError.cancelled)) } // Escape
    }

    private func finish(_ result: Result<CGRect, Error>) {
        NSCursor.pop()
        orderOut(nil)
        continuation?.resume(with: result)
        continuation = nil
    }
}
