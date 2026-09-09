import AppKit
import WebKit
import OSLog
import CoreFoundation

/// A native hit region that moves the whole window without resizing the sidebar.
@MainActor
private final class WindowDragStrip: NSView {
    private let logger = Logger(subsystem: "com.rozu.desktop", category: "WindowChrome")
    private var dragStart: (windowOrigin: NSPoint, screenPoint: NSPoint)?
    private var hasPushedCursor = false

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func resetCursorRects() {
        super.resetCursorRects()
        if !isHidden { addCursorRect(bounds, cursor: dragStart == nil ? .openHand : .closedHand) }
    }

    override func mouseDown(with event: NSEvent) {
        guard let window, !isHidden, event.type == .leftMouseDown else { return }
        cancelDrag()
        let origin = window.frame.origin
        dragStart = (origin, window.convertPoint(toScreen: event.locationInWindow))
        logger.notice("Window drag began at x=\(origin.x, privacy: .public), y=\(origin.y, privacy: .public)")
        NSCursor.closedHand.push()
        hasPushedCursor = true
    }

    override func mouseDragged(with event: NSEvent) {
        guard let window, let dragStart, !isHidden else { return }
        // Convert each delivered event through the current window position.
        // This preserves screen-space deltas as the window moves under the pointer.
        let point = window.convertPoint(toScreen: event.locationInWindow)
        window.setFrameOrigin(NSPoint(
            x: dragStart.windowOrigin.x + point.x - dragStart.screenPoint.x,
            y: dragStart.windowOrigin.y + point.y - dragStart.screenPoint.y
        ))
    }

    override func mouseUp(with event: NSEvent) {
        if dragStart != nil, let origin = window?.frame.origin {
            logger.notice("Window drag ended at x=\(origin.x, privacy: .public), y=\(origin.y, privacy: .public)")
        }
        cancelDrag()
    }

    override func viewWillMove(toWindow newWindow: NSWindow?) {
        if newWindow == nil { cancelDrag() }
        super.viewWillMove(toWindow: newWindow)
    }

    func cancelDrag() {
        dragStart = nil
        if hasPushedCursor {
            NSCursor.pop()
            hasPushedCursor = false
        }
        window?.invalidateCursorRects(for: self)
    }
}

@MainActor
private final class WindowContentView: NSView {
    private let webView: WKWebView
    private let dragStrip = WindowDragStrip(frame: .zero)

    init(webView: WKWebView, frame: NSRect) {
        self.webView = webView
        super.init(frame: frame)
        addSubview(webView)
        addSubview(dragStrip, positioned: .above, relativeTo: webView)
        needsLayout = true
    }

    required init?(coder: NSCoder) { return nil }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        needsLayout = true
    }

    override func layout() {
        super.layout()
        webView.frame = bounds
        let divider = min(max(bounds.width * 0.20, 232), 306)
        dragStrip.frame = NSRect(x: divider - 4, y: bounds.minY, width: 8, height: bounds.height)
        window?.invalidateCursorRects(for: dragStrip)
    }

    func setDragStripVisible(_ visible: Bool) {
        if !visible { dragStrip.cancelDrag() }
        dragStrip.isHidden = !visible
        window?.invalidateCursorRects(for: dragStrip)
    }
}

/// The controller retains this proxy, which holds the application delegate weakly.
@MainActor
private final class WindowChromeMessageHandler: NSObject, WKScriptMessageHandler {
    weak var owner: AppDelegate?

    init(owner: AppDelegate) { self.owner = owner }

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        owner?.receiveWindowChromeState(message)
    }
}

/// A local presentation shell. Its sole UI bridge controls drag-strip visibility.
/// No agent, server, native execution, or remote content is implemented.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate, WKNavigationDelegate, WKUIDelegate {
    private var window: NSWindow!
    private var webView: WKWebView!
    private var contentView: WindowContentView!
    private var uiDirectory: URL?
    private let logger = Logger(subsystem: "com.rozu.desktop", category: "WebView")
    private let windowLogger = Logger(subsystem: "com.rozu.desktop", category: "WindowChrome")

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.appearance = NSAppearance(named: .darkAqua)
        makeMenu()

        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        configuration.mediaTypesRequiringUserActionForPlayback = .all
        configuration.userContentController.add(WindowChromeMessageHandler(owner: self), name: "windowChrome")
        webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.underPageBackgroundColor = NSColor(calibratedWhite: 0.067, alpha: 1)
        webView.allowsBackForwardNavigationGestures = false

        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1240, height: 820),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false
        )
        window.title = "Rozu"
        window.delegate = self
        window.titleVisibility = .hidden
        window.backgroundColor = NSColor(calibratedWhite: 0.067, alpha: 1)
        window.minSize = NSSize(width: 900, height: 620)
        contentView = WindowContentView(webView: webView, frame: NSRect(x: 0, y: 0, width: 1240, height: 820))
        window.contentView = contentView
        window.isReleasedWhenClosed = false
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)

        guard let resources = Bundle.main.resourceURL else { return }
        let directory = resources.appendingPathComponent("UI", isDirectory: true)
        let index = directory.appendingPathComponent("index.html")
        guard FileManager.default.fileExists(atPath: index.path) else {
            showMissingUI()
            return
        }
        uiDirectory = directory.resolvingSymlinksInPath().standardizedFileURL
        // Bundled CSP denies connections; the client entitlement is required for
        // WebKit's subprocess startup, even though this UI loads only local files.
        // Loading local resources must not depend on a persistent WebKit rule store.
        webView.loadFileURL(index, allowingReadAccessTo: directory)
        logger.notice("Loading bundled interface")
    }

    fileprivate func receiveWindowChromeState(_ message: WKScriptMessage) {
        guard message.name == "windowChrome",
              message.webView === webView,
              message.frameInfo.isMainFrame,
              let url = message.frameInfo.request.url,
              url.isFileURL,
              let uiDirectory,
              url.resolvingSymlinksInPath().standardizedFileURL.path.hasPrefix(uiDirectory.path + "/"),
              let body = message.body as? [String: Any],
              Set(body.keys) == Set(["sidebarVisible", "modalOpen"]),
              let sidebarVisible = body["sidebarVisible"] as? NSNumber,
              let modalOpen = body["modalOpen"] as? NSNumber,
              CFGetTypeID(sidebarVisible) == CFBooleanGetTypeID(),
              CFGetTypeID(modalOpen) == CFBooleanGetTypeID() else {
            logger.error("Rejected invalid window chrome state")
            return
        }
        contentView.setDragStripVisible(sidebarVisible.boolValue && !modalOpen.boolValue)
    }

    private func showMissingUI() {
        let alert = NSAlert()
        alert.messageText = "Rozu preview unavailable"
        alert.informativeText = "The bundled interface is missing. Rebuild Rozu using script/build_and_run.sh."
        alert.addButton(withTitle: "Quit")
        alert.runModal()
        NSApp.terminate(nil)
    }

    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard let url = navigationAction.request.url, url.isFileURL, let uiDirectory else {
            logger.error("Blocked navigation outside bundled interface")
            decisionHandler(.cancel)
            return
        }
        let path = url.resolvingSymlinksInPath().standardizedFileURL.path
        decisionHandler(path.hasPrefix(uiDirectory.path + "/") ? .allow : .cancel)
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        logger.notice("Bundled interface navigation finished")
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        logger.error("Navigation failed: \(error.localizedDescription, privacy: .public)")
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        logger.error("Initial navigation failed: \(error.localizedDescription, privacy: .public)")
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        logger.error("WebKit content process terminated")
    }

    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        nil
    }

    func webView(_ webView: WKWebView, runOpenPanelWith parameters: WKOpenPanelParameters,
                 initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping ([URL]?) -> Void) {
        completionHandler(nil)
    }

    func webView(_ webView: WKWebView, requestMediaCapturePermissionFor origin: WKSecurityOrigin,
                 initiatedByFrame frame: WKFrameInfo, type: WKMediaCaptureType,
                 decisionHandler: @escaping (WKPermissionDecision) -> Void) {
        decisionHandler(.deny)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func windowDidMove(_ notification: Notification) {
        guard let movedWindow = notification.object as? NSWindow, movedWindow === window else { return }
        let origin = movedWindow.frame.origin
        windowLogger.notice("Window moved to x=\(origin.x, privacy: .public), y=\(origin.y, privacy: .public)")
    }

    func windowDidEndLiveResize(_ notification: Notification) {
        guard let resizedWindow = notification.object as? NSWindow, resizedWindow === window else { return }
        let frame = resizedWindow.frame
        windowLogger.notice("Window resize ended at x=\(frame.origin.x, privacy: .public), y=\(frame.origin.y, privacy: .public), width=\(frame.width, privacy: .public), height=\(frame.height, privacy: .public)")
    }

    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool { true }

    private func makeMenu() {
        let menu = NSMenu()
        let applicationItem = NSMenuItem()
        let applicationMenu = NSMenu()
        applicationMenu.addItem(withTitle: "About Rozu", action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)), keyEquivalent: "")
        applicationMenu.addItem(.separator())
        applicationMenu.addItem(withTitle: "Hide Rozu", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
        applicationMenu.addItem(.separator())
        applicationMenu.addItem(withTitle: "Quit Rozu", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        applicationItem.submenu = applicationMenu
        menu.addItem(applicationItem)

        let editItem = NSMenuItem(title: "Edit", action: nil, keyEquivalent: "")
        let editMenu = NSMenu(title: "Edit")
        editMenu.addItem(withTitle: "Undo", action: Selector(("undo:")), keyEquivalent: "z")
        editMenu.addItem(.separator())
        editMenu.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        editMenu.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        editMenu.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        editMenu.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editItem.submenu = editMenu
        menu.addItem(editItem)

        let windowItem = NSMenuItem(title: "Window", action: nil, keyEquivalent: "")
        let windowMenu = NSMenu(title: "Window")
        windowMenu.addItem(withTitle: "Minimize", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
        windowMenu.addItem(withTitle: "Zoom", action: #selector(NSWindow.performZoom(_:)), keyEquivalent: "")
        windowMenu.addItem(withTitle: "Close", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
        windowItem.submenu = windowMenu
        menu.addItem(windowItem)
        NSApp.windowsMenu = windowMenu
        NSApp.mainMenu = menu
    }
}

MainActor.assumeIsolated {
    let app = NSApplication.shared
    let delegate = AppDelegate()
    app.delegate = delegate
    withExtendedLifetime(delegate) { app.run() }
}
