// Global shortcuts through Carbon's RegisterEventHotKey: no dependencies,
// and a registration that collides with another app's fails loudly
// (eventHotKeyExistsErr) instead of silently stealing the keys.
//
// UNVERIFIED: never compiled or run (written without the macOS SDK).

import Carbon.HIToolbox
import Foundation

enum CaptureKind: UInt32, CaseIterable {
    case display = 1, region = 2, window = 3

    var title: String {
        switch self {
        case .display: return "Capture Screen"
        case .region: return "Capture Region"
        case .window: return "Capture Window"
        }
    }

    var defaultsKey: String { "shortcut.\(self)" }
}

/// A shortcut as stored in UserDefaults: "opt+shift+cmd+4".
struct Shortcut: CustomStringConvertible {
    let keyCode: UInt32
    let modifiers: UInt32

    static let keys: [String: UInt32] = [
        "3": UInt32(kVK_ANSI_3), "4": UInt32(kVK_ANSI_4), "5": UInt32(kVK_ANSI_5),
        "6": UInt32(kVK_ANSI_6), "s": UInt32(kVK_ANSI_S), "x": UInt32(kVK_ANSI_X),
    ]

    /// Defaults that do not collide with macOS's own screenshot shortcuts
    /// (⇧⌘3/4/5 and ⌃⇧⌘3/4): option is added.
    static func fallback(for kind: CaptureKind) -> String {
        switch kind {
        case .display: return "opt+shift+cmd+3"
        case .region: return "opt+shift+cmd+4"
        case .window: return "opt+shift+cmd+5"
        }
    }

    init?(_ text: String) {
        var mods: UInt32 = 0
        var key: UInt32?
        for part in text.lowercased().split(separator: "+") {
            switch part {
            case "cmd": mods |= UInt32(cmdKey)
            case "shift": mods |= UInt32(shiftKey)
            case "opt", "alt": mods |= UInt32(optionKey)
            case "ctrl": mods |= UInt32(controlKey)
            default: key = Shortcut.keys[String(part)]
            }
        }
        guard let k = key, mods != 0 else { return nil }
        keyCode = k
        modifiers = mods
    }

    var description: String {
        var s = ""
        if modifiers & UInt32(controlKey) != 0 { s += "⌃" }
        if modifiers & UInt32(optionKey) != 0 { s += "⌥" }
        if modifiers & UInt32(shiftKey) != 0 { s += "⇧" }
        if modifiers & UInt32(cmdKey) != 0 { s += "⌘" }
        let name = Shortcut.keys.first { $0.value == keyCode }?.key.uppercased() ?? "?"
        return s + name
    }
}

final class Hotkeys {
    private var refs: [EventHotKeyRef] = []
    private static var handler: ((CaptureKind) -> Void)?
    /// Shortcuts that could not be registered, with why.
    private(set) var conflicts: [(CaptureKind, String)] = []
    private(set) var active: [(CaptureKind, Shortcut)] = []

    func register(_ onPress: @escaping (CaptureKind) -> Void) {
        Hotkeys.handler = onPress
        var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
        InstallEventHandler(GetApplicationEventTarget(), { _, event, _ -> OSStatus in
            var id = EventHotKeyID()
            let status = GetEventParameter(event, EventParamName(kEventParamDirectObject), EventParamType(typeEventHotKeyID),
                                           nil, MemoryLayout<EventHotKeyID>.size, nil, &id)
            if status == noErr, let kind = CaptureKind(rawValue: id.id) {
                DispatchQueue.main.async { Hotkeys.handler?(kind) }
            }
            return noErr
        }, 1, &spec, nil, nil)
        reload()
    }

    /// (Re)registers every shortcut from UserDefaults.
    func reload() {
        for r in refs { UnregisterEventHotKey(r) }
        refs = []
        conflicts = []
        active = []
        for kind in CaptureKind.allCases {
            let text = UserDefaults.standard.string(forKey: kind.defaultsKey) ?? Shortcut.fallback(for: kind)
            if text == "off" { continue }
            guard let s = Shortcut(text) else {
                conflicts.append((kind, "cannot parse \"\(text)\""))
                continue
            }
            var ref: EventHotKeyRef?
            let id = EventHotKeyID(signature: OSType(0x5845_4554), id: kind.rawValue) // 'XEET'
            let status = RegisterEventHotKey(s.keyCode, s.modifiers, id, GetApplicationEventTarget(), 0, &ref)
            if status == noErr, let ref {
                refs.append(ref)
                active.append((kind, s))
            } else if status == OSStatus(eventHotKeyExistsErr) {
                conflicts.append((kind, "\(s) is already registered by another application"))
            } else {
                conflicts.append((kind, "\(s) could not be registered (OSStatus \(status))"))
            }
        }
    }
}
