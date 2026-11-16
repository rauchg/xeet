// Xeet Capture: a menu-bar app that makes every screenshot a Xeet artifact.
//
// UNVERIFIED: never compiled or run (written without the macOS SDK).

import AppKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let status = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    let hotkeys = Hotkeys()
    var last: CaptureReport?
    var lastError: String?

    func applicationDidFinishLaunching(_ notification: Notification) {
        status.button?.title = "◎"
        // Left click: the menu. Right click: capture a region right away.
        status.button?.target = self
        status.button?.action = #selector(statusClicked(_:))
        status.button?.sendAction(on: [.leftMouseUp, .rightMouseUp])
        hotkeys.register { [weak self] kind in self?.capture(kind) }
        rebuildMenu()
        if !UserDefaults.standard.bool(forKey: "firstRunDone") {
            firstRun()
            UserDefaults.standard.set(true, forKey: "firstRunDone")
        }
    }

    @objc func statusClicked(_ sender: NSStatusBarButton) {
        if NSApp.currentEvent?.type == .rightMouseUp {
            capture(.region)
        } else {
            status.menu = menu
            status.button?.performClick(nil)
            status.menu = nil
        }
    }

    func capture(_ kind: CaptureKind) {
        Task {
            do {
                let shot = try await Capturer.capture(kind)
                let report = try Pipeline.run(shot)
                last = report
                lastError = nil
                status.button?.title = report.textRead ? "◎ ✓" : "◎ •"
            } catch CaptureError.cancelled {
                lastError = nil
            } catch {
                lastError = "\(error)"
                status.button?.title = "◎ !"
                try? "\(Date()): \(error)\n".write(to: Settings.support.appendingPathComponent("last-error.txt"), atomically: true, encoding: .utf8)
            }
            rebuildMenu()
            if lastError == nil {
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                status.button?.title = "◎"
            }
        }
    }

    var menu = NSMenu()

    func rebuildMenu() {
        let m = NSMenu()
        for (kind, shortcut) in hotkeys.active {
            m.addItem(item("\(kind.title)   \(shortcut)") { [weak self] in self?.capture(kind) })
        }
        for kind in CaptureKind.allCases where !hotkeys.active.contains(where: { $0.0 == kind }) {
            m.addItem(item(kind.title) { [weak self] in self?.capture(kind) })
        }
        for (kind, why) in hotkeys.conflicts {
            let i = NSMenuItem(title: "⚠︎ \(kind.title) shortcut: \(why)", action: nil, keyEquivalent: "")
            i.isEnabled = false
            m.addItem(i)
        }
        m.addItem(.separator())
        if let r = last, let artifact = r.artifact {
            let summary = r.textRead ? "\(r.ocrLines) text lines, \(r.uncertainLines) uncertain" : "no text read"
            m.addItem(withTitle: "Last capture: \(summary)", action: nil, keyEquivalent: "").isEnabled = false
            m.addItem(item("Copy Artifact Reference") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(artifact, forType: .string)
            })
            m.addItem(item("Show Capture Files") { NSWorkspace.shared.open(Settings.support.appendingPathComponent("captures")) })
            if let png = r.exported {
                m.addItem(item("Open Exported PNG") { NSWorkspace.shared.open(png) })
            }
            for n in r.notes { m.addItem(withTitle: "· \(n)", action: nil, keyEquivalent: "").isEnabled = false }
        }
        if let e = lastError {
            m.addItem(withTitle: "Last capture failed: \(e)", action: nil, keyEquivalent: "").isEnabled = false
        }
        m.addItem(.separator())
        m.addItem(toggle("Copy Image to Clipboard", Settings.copyImage) { Settings.copyImage.toggle() })
        m.addItem(toggle("Save PNG to Desktop", Settings.savePNG) { Settings.savePNG.toggle() })
        m.addItem(toggle("Include Accessibility Context", Settings.includeAccessibility) {
            Settings.includeAccessibility.toggle()
            if Settings.includeAccessibility { Permissions.requestAccessibility() }
        })
        m.addItem(item("Shortcuts & Permissions…") { [weak self] in self?.firstRun() })
        m.addItem(.separator())
        m.addItem(item("Quit Xeet") { NSApp.terminate(nil) })
        menu = m
    }

    func firstRun() {
        let a = NSAlert()
        a.messageText = "Xeet Capture"
        let shortcuts = hotkeys.active.map { "\($0.0.title): \($0.1)" }.joined(separator: "\n")
        a.informativeText = """
        Every capture becomes a Xeet artifact: the original pixels, plus text read on this Mac and the capture's context, each with its own provenance.

        Shortcuts:
        \(shortcuts.isEmpty ? "none registered" : shortcuts)

        Screen Recording: \(Permissions.screenRecording). Xeet asks for it on first capture.
        Accessibility: \(Permissions.accessibility). Used only if you turn on "Include Accessibility Context".

        To make Xeet your screenshot tool, turn off the built-in shortcuts in System Settings › Keyboard › Keyboard Shortcuts › Screenshots, then set Xeet's with:
          defaults write \(Bundle.main.bundleIdentifier ?? "dev.xeet.capture") shortcut.region "shift+cmd+4"
        and restart Xeet. To go back, turn the built-in shortcuts on again and remove Xeet's with defaults delete.
        """
        a.addButton(withTitle: "OK")
        if !CGPreflightScreenCaptureAccess() { a.addButton(withTitle: "Grant Screen Recording…") }
        if a.runModal() == .alertSecondButtonReturn { _ = CGRequestScreenCaptureAccess() }
    }

    // Menu items that run closures.
    private var actions: [ClosureTarget] = []

    private func item(_ title: String, _ run: @escaping () -> Void) -> NSMenuItem {
        let t = ClosureTarget(run)
        actions.append(t)
        let i = NSMenuItem(title: title, action: #selector(ClosureTarget.fire), keyEquivalent: "")
        i.target = t
        return i
    }

    private func toggle(_ title: String, _ on: Bool, _ run: @escaping () -> Void) -> NSMenuItem {
        let i = item(title) { [weak self] in
            run()
            self?.rebuildMenu()
        }
        i.state = on ? .on : .off
        return i
    }
}

final class ClosureTarget: NSObject {
    let run: () -> Void
    init(_ run: @escaping () -> Void) { self.run = run }
    @objc func fire() { run() }
}

MainActor.assumeIsolated {
    let app = NSApplication.shared
    let delegate = AppDelegate()
    app.delegate = delegate
    app.setActivationPolicy(.accessory)
    app.run()
}
