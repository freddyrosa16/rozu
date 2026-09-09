# Rozu

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/freddyrosa16/rozu/main/assets/brand/dried-rose/logo-light.svg">
  <img src="https://raw.githubusercontent.com/freddyrosa16/rozu/main/assets/brand/dried-rose/logo-graphite.svg" alt="Rozu dried rose and custom wordmark" width="280" height="78">
</picture>

An open-source AI agent project. Working name: **Rozu**.

[Visit the landing page](https://freddyrosa16.github.io/rozu/)

This repository currently contains the GitHub Pages website only. The previous desktop preview has been removed so app development can start fresh.

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
- `assets/brand/dried-rose/`: SVG rose and lettering used by the website and this README

The website does not run an agent or connect to a backend.

Run the website animation checks with Node.js:

```sh
node --test tests/waves.test.cjs
```

## License

[MIT](LICENSE)

The background uses a full-field WebGL shader at the display refresh rate, with a 30fps Canvas 2D fallback when WebGL is unavailable. Fluid waves travel through every part of the canvas; there are no fixed left-side or text-area masks. The gray pixels stay subdued so the text remains readable.
