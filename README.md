# Rozu

An open-source AI agent project. Working name: **Rozu**.

[Visit the landing page](https://freddyrosa16.github.io/rozu/)

## Website

A lightweight, single-screen introduction to Rozu. The original blue/graphite, ordered-dither animation is inspired by the visual texture on the right of Capy's signup page. No Capy artwork, branding, or source code is included.

The page uses plain HTML, CSS, and JavaScript, with no build step, dependencies, analytics, or external font requests. The footer palette selector switches between blue and graphite. Motion can be paused and starts paused for visitors who prefer reduced motion. It also pauses when the page is hidden.

Run locally:

```sh
python3 -m http.server 8080
```

Open `http://localhost:8080`. GitHub Pages publishes the repository root on `main`.

- `index.html`: text, links, and accessible structure
- `styles.css`: layout, typography, colors, and responsive styles
- `waves.js`: decorative canvas and motion controls

This is the project's landing page. Agent functionality and the backend are still to be built. API keys must stay on the future backend.

## License

[MIT](LICENSE)
