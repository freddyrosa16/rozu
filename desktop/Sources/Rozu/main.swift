import AppKit
import WebKit
import OSLog

/// A local presentation shell. No agent, server, native bridge, or remote content.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, WKNavigationDelegate, WKUIDelegate {
    private var window: NSWindow!
    private var webView: WKWebView!
    private var uiDirectory: URL?
    private let logger = Logger(subsystem: "com.rozu.desktop", category: "WebView")

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.appearance = NSAppearance(named: .darkAqua)
        makeMenu()

        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        configuration.mediaTypesRequiringUserActionForPlayback = .all
        webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.underPageBackgroundColor = NSColor(calibratedWhite: 0.067, alpha: 1)
        webView.allowsBackForwardNavigationGestures = false

        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1240, height: 820),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered, defer: false
        )
        window.title = "Rozu"
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.titlebarSeparatorStyle = .none
        window.backgroundColor = NSColor(calibratedWhite: 0.067, alpha: 1)
        window.minSize = NSSize(width: 900, height: 620)
        window.contentView = webView
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
