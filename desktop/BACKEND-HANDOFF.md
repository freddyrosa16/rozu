# Backend handoff

This release is a UI preview. Freddy is building the backend, AI integrations, and database separately, using Python or another language of his choice.

## What is here now

- `ui/src/main.jsx`: React screens and temporary interface state.
- `ui/src/styles.css`: colors, spacing, layouts, and responsive rules.
- `ui/src/waves.js`: decorative dither animation and lifecycle cleanup.
- `Sources/Rozu/main.swift`: a Mac window that displays bundled frontend assets.

There are no API clients, model requests, database connections, authentication flows, filesystem operations, terminal sessions, task execution, or background jobs in the frontend. Forms and navigation are previews. Send, attach, connect, create, and save operations are disabled. Drafts and appearance choices last only for the current app session.

## When you are ready to build the backend

Keep the Python service (or any other service) separate from the React UI. A future frontend API adapter can communicate using HTTP and a stream such as server-sent events or WebSocket. The UI does not require the service to use JavaScript, Swift, or any specific database.

Suggested order for learning, not implemented features:

1. Build a small service with a health endpoint and a task endpoint.
2. Connect the composer through one frontend API adapter.
3. Stream actual task events into a conversation screen.
4. Add persistence and authentication on the backend.
5. Connect the backend to a model provider.
6. Add project and tool access only when you explicitly implement those capabilities.

Model credentials and database credentials belong in the backend. The current frontend deliberately has no credential inputs or credential storage.

The preview's content security policy sets `connect-src 'none'`, and the Mac shell only permits navigation within its bundled UI. When backend integration starts, review these boundaries and allow only the intended service endpoint. Do not broadly enable remote content or expose a general-purpose native command bridge.

The one-line website download points to the packaged Mac UI preview on GitHub Releases. The website itself remains a static GitHub Pages site; it is not an API server.

The existing `windowChrome` message is only a presentation detail: two booleans tell the Mac shell whether its window-drag strip should be visible. It does not run commands, read files, connect providers, or supply a backend transport.
