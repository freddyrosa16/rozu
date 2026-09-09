# Rozu

An open-source AI agent project. Working name: **Rozu**.

[Visit the landing page](https://freddyrosa16.github.io/rozu/)

## Desktop preview

[Download for Mac](https://github.com/freddyrosa16/rozu/releases/download/v0.1.0-preview/Rozu-0.1.0-macos-arm64.zip) · macOS 13+ · Apple silicon

A frontend-only desktop interface with Rozu's gray dithered background, a Codex-style workspace, and layout details from the supplied Capy desktop reference. It includes a composer, sidebar navigation, empty task/review panels, and settings. It makes no AI calls, executes no commands, and connects to no database. Backend-dependent actions are disabled.

The React frontend is separate from the Mac window shell. Freddy can build the backend in Python or another language. See [backend handoff](desktop/BACKEND-HANDOFF.md) and [desktop build instructions](desktop/README.md).

Run the Mac app from source with `./script/build_and_run.sh`. This preview is ad-hoc signed, not Apple-notarized; see the [release notes](https://github.com/freddyrosa16/rozu/releases/tag/v0.1.0-preview) for installation details.

## Website

A lightweight, single-screen introduction to Rozu. The original graphite, ordered-dither animation is inspired by the visual texture on the right of Capy's signup page. No Capy artwork, branding, or source code is included.

The page uses plain HTML, CSS, and JavaScript, with no build step, dependencies, analytics, or external font requests. The animation runs automatically. Visitors who prefer reduced motion see a still background; drawing also stops while the page is hidden.

Run locally:

```sh
python3 -m http.server 8080
```

Open `http://localhost:8080`. GitHub Pages publishes the repository root on `main`.

- `index.html`: text, links, and accessible structure
- `styles.css`: layout, typography, colors, and responsive styles
- `waves.js`: decorative canvas and motion preferences

The landing page and desktop interface are separate surfaces. Agent functionality and the backend are still to be built. API keys must stay on the future backend.

## License

[MIT](LICENSE)

The background uses a full-field WebGL shader at the display refresh rate, with a 30fps Canvas 2D fallback when WebGL is unavailable. Fluid waves travel through every part of the canvas; there are no fixed left-side or text-area masks. The gray pixels stay subdued so the text remains readable.
