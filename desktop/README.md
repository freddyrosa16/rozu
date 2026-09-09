# Rozu desktop preview

A macOS AppKit window containing the bundled React interface in a WKWebView. This is an interface prototype: it does not run an agent, execute commands, access projects, authenticate, or call a model.

## Run from source

Requires macOS 13 or newer, Xcode Command Line Tools with Swift 5.9 or newer, Node.js 22.12 or newer, and pnpm 11. From the repository root:

```sh
./script/build_and_run.sh --verify
```

The script installs the locked frontend dependencies, builds the UI and native shell, stages `dist/Rozu.app`, signs it locally with an ad-hoc signature, and opens a real macOS app. The Codex Run action runs the same script. `--build-only` builds without launching; `--debug`, `--logs`, and `--telemetry` support local diagnosis.

To build the native shell against an already-built `desktop/ui/dist` without reinstalling or rebuilding the frontend, use `ROZU_USE_BUILT_UI=1 ./script/build_and_run.sh --build-only`. The same environment option works with run and packaging commands. It fails if the existing UI entry point is missing.

## Package

```sh
./script/package_macos.sh
```

An Apple Silicon build produces `dist/Rozu-0.1.0-macos-arm64.zip` and a matching `.sha256` file. The script labels the archive using the actual compiled architecture. This preview is **not Apple-notarized** and has no Developer ID signature. macOS may block a downloaded copy; a normal public distribution needs a Developer ID certificate and Apple notarization. No script changes Gatekeeper settings or removes quarantine. Build from source for local development.

## Boundaries

- The window uses standard macOS traffic lights and Edit/Window menus.
- The app loads its bundled `Contents/Resources/UI` interface into the web view. Navigation outside that directory is denied. The bundled content security policy permits local resources only and sets `connect-src 'none'`.
- App Sandbox is enabled. WebKit needs the network-client entitlement for its renderer subprocesses to start, even with bundled files; the UI's CSP still denies connections, and navigation is limited to the bundle. There are no backend calls or remote UI resources. No user-selected file access entitlement is granted. File selection and media capture are denied.
- Website data is nonpersistent. There are no JavaScript message handlers, native execution bridges, remote services, or backend dependencies.
- The repository's MIT license applies to the desktop source.

Future Python or other backend services, model integrations, and databases are separate work. This wrapper does not implement them or choose their architecture; the current UI keeps its preview state in memory.

The frontend lives in `desktop/ui`; the Swift shell lives in `desktop/Sources/Rozu/main.swift`. The frontend's production build must use relative asset URLs so it loads from the bundled file URL.

For frontend development in a browser, run `pnpm install --frozen-lockfile` and `pnpm dev` inside `desktop/ui`. Only Vite development mode permits its local hot-reload connection; packaged builds retain `connect-src 'none'`.
