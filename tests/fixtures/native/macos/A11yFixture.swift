// Native AppKit test app for the nugget-a11y AX integration tests
// (crates/nugget-a11y/tests/macos_ax.rs). Built on the runner with `swiftc`: see build.sh.
//
// Usage: NuggetA11yFixture <state-file>
//
// The window "Nugget A11y Fixture" holds a multi-line composer (NSTextView, "Hello world" with
// "world" selected and focused), a single-line Name field, an Apply button (sets the status
// label and the window title), a Notifications check box and a Fruits table. Every 50 ms the app
// writes its own view of every control to <state-file> as one JSON object (atomically), so the
// test can check what really happened independently of AX.

import AppKit

let baseTitle = "Nugget A11y Fixture"
let fruitNames = ["Apple", "Banana", "Cherry"]

final class Fixture: NSObject, NSApplicationDelegate, NSTableViewDataSource, NSTableViewDelegate {
    let statePath: String
    var window: NSWindow!
    var composer: NSTextView!
    var nameField: NSTextField!
    var check: NSButton!
    var table: NSTableView!
    var status: NSTextField!
    var presses = 0
    var lastState = ""
    var activateTries = 0

    init(statePath: String) {
        self.statePath = statePath
    }

    /// The standard app and Edit menus: AppKit routes Cmd+V (the paste fallback) and the other
    /// editing shortcuts through the Edit menu's key equivalents, as in every real app.
    func installMenus() {
        let main = NSMenu()
        let appItem = NSMenuItem()
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "Quit", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu
        main.addItem(appItem)
        let editItem = NSMenuItem()
        let edit = NSMenu(title: "Edit")
        edit.addItem(withTitle: "Undo", action: Selector(("undo:")), keyEquivalent: "z")
        edit.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        edit.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        edit.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        edit.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editItem.submenu = edit
        main.addItem(editItem)
        NSApp.mainMenu = main
    }

    func applicationDidFinishLaunching(_ note: Notification) {
        installMenus()
        window = NSWindow(
            contentRect: NSRect(x: 120, y: 200, width: 520, height: 420),
            styleMask: [.titled, .closable, .resizable],
            backing: .buffered, defer: false)
        window.title = baseTitle
        let content = window.contentView!

        let composerLabel = NSTextField(labelWithString: "Message composer")
        composerLabel.frame = NSRect(x: 12, y: 384, width: 200, height: 20)
        content.addSubview(composerLabel)

        let scroll = NSScrollView(frame: NSRect(x: 12, y: 270, width: 490, height: 110))
        scroll.hasVerticalScroller = true
        composer = NSTextView(frame: NSRect(x: 0, y: 0, width: 490, height: 110))
        composer.isRichText = false
        composer.isAutomaticQuoteSubstitutionEnabled = false
        composer.isAutomaticDashSubstitutionEnabled = false
        composer.isAutomaticTextReplacementEnabled = false
        composer.isAutomaticSpellingCorrectionEnabled = false
        composer.isContinuousSpellCheckingEnabled = false
        composer.string = "Hello world"
        composer.setAccessibilityLabel("Message composer")
        composer.setAccessibilityIdentifier("composer")
        scroll.documentView = composer
        content.addSubview(scroll)

        let nameLabel = NSTextField(labelWithString: "Name")
        nameLabel.frame = NSRect(x: 12, y: 236, width: 60, height: 20)
        content.addSubview(nameLabel)
        nameField = NSTextField(frame: NSRect(x: 80, y: 232, width: 220, height: 24))
        nameField.placeholderString = "Your name"
        nameField.setAccessibilityLabel("Name")
        nameField.setAccessibilityIdentifier("name")
        content.addSubview(nameField)

        let apply = NSButton(title: "Apply", target: self, action: #selector(applyPressed))
        apply.frame = NSRect(x: 12, y: 192, width: 100, height: 30)
        apply.setAccessibilityIdentifier("apply")
        content.addSubview(apply)

        check = NSButton(checkboxWithTitle: "Notifications", target: nil, action: nil)
        check.frame = NSRect(x: 130, y: 196, width: 160, height: 24)
        check.setAccessibilityIdentifier("notifications")
        content.addSubview(check)

        let tableScroll = NSScrollView(frame: NSRect(x: 12, y: 80, width: 200, height: 100))
        table = NSTableView(frame: tableScroll.bounds)
        let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("fruit"))
        col.title = "Fruit"
        col.width = 180
        table.addTableColumn(col)
        table.headerView = nil
        table.dataSource = self
        table.delegate = self
        table.setAccessibilityLabel("Fruits")
        table.setAccessibilityIdentifier("fruits")
        tableScroll.documentView = table
        tableScroll.hasVerticalScroller = true
        content.addSubview(tableScroll)

        status = NSTextField(labelWithString: "Status: idle")
        status.frame = NSRect(x: 12, y: 40, width: 300, height: 24)
        status.setAccessibilityIdentifier("status")
        content.addSubview(status)

        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(composer)
        composer.setSelectedRange(NSRange(location: 6, length: 5))
        NSApp.activate(ignoringOtherApps: true)

        Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { [weak self] _ in
            self?.tick()
        }
    }

    @objc func applyPressed() {
        presses += 1
        status.stringValue = "Status: applied \(presses)"
        window.title = "\(baseTitle) - applied \(presses)"
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        fruitNames.count
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = NSTableCellView()
        let text = NSTextField(labelWithString: fruitNames[row])
        text.frame = NSRect(x: 2, y: 0, width: 176, height: 17)
        cell.addSubview(text)
        cell.textField = text
        return cell
    }

    func tick() {
        // Take the front once at startup: launched from a background process, the app may
        // not be activated by the system on its own.
        if activateTries < 40 {
            if NSApp.isActive && window.isKeyWindow {
                activateTries = 40
            } else {
                activateTries += 1
                NSApp.activate(ignoringOtherApps: true)
                window.makeKeyAndOrderFront(nil)
                window.makeFirstResponder(composer)
            }
        }
        writeState()
    }

    func writeState() {
        let sel = composer.selectedRange()
        let text = composer.string as NSString
        // Selection as character (Unicode scalar) offsets, like the test's API.
        func chars(_ utf16: Int) -> Int {
            text.substring(to: min(utf16, text.length)).unicodeScalars.count
        }
        let row = table.selectedRow
        let responder = window.firstResponder
        var focus = ""
        if responder === composer {
            focus = "composer"
        } else if let editor = responder as? NSTextView, editor.delegate === nameField {
            focus = "name"
        } else if responder === table {
            focus = "fruits"
        }
        let state: [String: Any] = [
            "pid": Int(ProcessInfo.processInfo.processIdentifier),
            "title": window.title,
            "composer": composer.string,
            "selection": [chars(sel.location), chars(sel.location + sel.length)],
            "name": nameField.stringValue,
            "checked": check.state == .on,
            "fruit": row >= 0 && row < fruitNames.count ? fruitNames[row] : NSNull(),
            "status": status.stringValue,
            "presses": presses,
            "focus": focus,
            "foreground": NSApp.isActive,
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: state, options: [.sortedKeys]),
              let s = String(data: data, encoding: .utf8), s != lastState
        else { return }
        if (try? data.write(to: URL(fileURLWithPath: statePath), options: .atomic)) != nil {
            lastState = s
        }
    }
}

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write("usage: NuggetA11yFixture <state-file>\n".data(using: .utf8)!)
    exit(2)
}
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let fixture = Fixture(statePath: args[1])
app.delegate = fixture
app.run()
