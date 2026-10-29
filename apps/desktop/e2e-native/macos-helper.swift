// macOS helper for run.mjs: reads one command per line on stdin, answers one JSON line.
// Keys and clicks as posted CGEvents, windows through CGWindowList (by owner pid; window titles
// would need Screen Recording) and Accessibility, the target's text through Accessibility.
// Built by macos.mjs with `swiftc`. Posting events and reading AX need the Accessibility grant
// the GitHub runner gives processes started from the job.
//
//   trigger                 double tap Right Command (flagsChanged, as the hardware sends it)
//   enter                   press Return
//   click <x> <y>           left click at a global point
//   windows <pid>           on-screen windows of <pid> (bounds in points) and its AX window titles
//   front                   the focused app (pid, bundle id, name), through Accessibility
//   activate <bundle id>    activate the app
//   read <bundle id>        the focused text area's value in that app (AX)
import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

func reply(_ obj: [String: Any]) {
    let data = try! JSONSerialization.data(withJSONObject: obj)
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write("\n".data(using: .utf8)!)
}

func post(_ ev: CGEvent?) {
    ev?.post(tap: .cghidEventTap)
    usleep(25_000)
}

// kVK_RightCommand, NX_DEVICERCMDKEYMASK.
let rightCommand: CGKeyCode = 54
let deviceRCmd: UInt64 = 0x10

func command(down: Bool) {
    let ev = CGEvent(keyboardEventSource: nil, virtualKey: rightCommand, keyDown: down)
    ev?.type = .flagsChanged
    ev?.setIntegerValueField(.keyboardEventKeycode, value: Int64(rightCommand))
    var flags = CGEventFlags.maskNonCoalesced.rawValue
    if down { flags |= CGEventFlags.maskCommand.rawValue | deviceRCmd }
    ev?.flags = CGEventFlags(rawValue: flags)
    post(ev)
}

func key(_ code: CGKeyCode) {
    for down in [true, false] {
        let ev = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down)
        ev?.flags = CGEventFlags(rawValue: 0)
        post(ev)
    }
    usleep(40_000)
}

func attr(_ el: AXUIElement, _ name: String) -> AnyObject? {
    var v: AnyObject?
    return AXUIElementCopyAttributeValue(el, name as CFString, &v) == .success ? v : nil
}

func app(_ bundle: String) -> NSRunningApplication? {
    NSRunningApplication.runningApplications(withBundleIdentifier: bundle).first
}

/// The first element with one of `roles` under `el`, depth first.
func find(_ el: AXUIElement, _ roles: Set<String>, depth: Int = 0) -> AXUIElement? {
    if depth > 12 { return nil }
    if let role = attr(el, kAXRoleAttribute) as? String, roles.contains(role) { return el }
    for child in (attr(el, kAXChildrenAttribute) as? [AXUIElement]) ?? [] {
        if let hit = find(child, roles, depth: depth + 1) { return hit }
    }
    return nil
}

while let line = readLine() {
    let parts = line.split(separator: " ", maxSplits: 1).map(String.init)
    let arg = parts.count > 1 ? parts[1] : ""
    switch parts.first ?? "" {
    case "trigger":
        for _ in 0..<2 {
            command(down: true)
            command(down: false)
            usleep(40_000)
        }
        reply(["ok": true])
    case "enter":
        key(36)
        reply(["ok": true])
    case "click":
        let xy = arg.split(separator: " ").compactMap { Double($0) }
        guard xy.count == 2 else { reply(["ok": false, "error": "click <x> <y>"]); continue }
        let p = CGPoint(x: xy[0], y: xy[1])
        post(CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: p, mouseButton: .left))
        post(CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown, mouseCursorPosition: p, mouseButton: .left))
        post(CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: p, mouseButton: .left))
        reply(["ok": true])
    case "windows":
        guard let pid = Int32(arg) else { reply(["ok": false, "error": "windows <pid>"]); continue }
        let list = (CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]]) ?? []
        var wins: [[String: Any]] = []
        for w in list where (w[kCGWindowOwnerPID as String] as? Int32) == pid {
            let b = (w[kCGWindowBounds as String] as? [String: Any]) ?? [:]
            wins.append([
                "id": "\(w[kCGWindowNumber as String] ?? 0)",
                "title": (w[kCGWindowName as String] as? String) ?? "",
                "layer": (w[kCGWindowLayer as String] as? Int) ?? 0,
                "alpha": (w[kCGWindowAlpha as String] as? Double) ?? 1,
                "x": (b["X"] as? Double) ?? 0, "y": (b["Y"] as? Double) ?? 0,
                "w": (b["Width"] as? Double) ?? 0, "h": (b["Height"] as? Double) ?? 0,
            ])
        }
        let axApp = AXUIElementCreateApplication(pid)
        let titles = ((attr(axApp, kAXWindowsAttribute) as? [AXUIElement]) ?? []).map { (attr($0, kAXTitleAttribute) as? String) ?? "" }
        reply(["ok": true, "windows": wins, "ax_titles": titles])
    case "front":
        let sys = AXUIElementCreateSystemWide()
        guard let focused = attr(sys, kAXFocusedApplicationAttribute) else { reply(["ok": true, "pid": 0, "bundle": "", "name": ""]); continue }
        var pid: pid_t = 0
        AXUIElementGetPid(focused as! AXUIElement, &pid)
        let running = NSRunningApplication(processIdentifier: pid)
        let window = attr(focused as! AXUIElement, kAXFocusedWindowAttribute)
        let title = window.flatMap { attr($0 as! AXUIElement, kAXTitleAttribute) as? String } ?? ""
        reply(["ok": true, "pid": Int(pid), "bundle": running?.bundleIdentifier ?? "", "name": running?.localizedName ?? "", "window": title])
    case "activate":
        guard let a = app(arg) else { reply(["ok": false, "error": "\(arg) is not running"]); continue }
        reply(["ok": a.activate(options: [])])
    case "read":
        guard let a = app(arg) else { reply(["ok": false, "error": "\(arg) is not running"]); continue }
        let axApp = AXUIElementCreateApplication(a.processIdentifier)
        var el = attr(axApp, kAXFocusedUIElementAttribute).map { $0 as! AXUIElement }
        if el.flatMap({ attr($0, kAXRoleAttribute) as? String }) != "AXTextArea" {
            let wins = (attr(axApp, kAXWindowsAttribute) as? [AXUIElement]) ?? []
            el = wins.lazy.compactMap { find($0, ["AXTextArea"]) }.first
        }
        guard let field = el else { reply(["ok": false, "error": "no text area in \(arg)"]); continue }
        reply(["ok": true, "text": (attr(field, kAXValueAttribute) as? String) ?? ""])
    default:
        reply(["ok": false, "error": "unknown command \(line)"])
    }
}
