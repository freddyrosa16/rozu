# Rozu


<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/freddyrosa16/rozu/main/assets/brand/dried-rose/logo-light.svg">
  <img src="https://raw.githubusercontent.com/freddyrosa16/rozu/main/assets/brand/dried-rose/logo-graphite.svg" alt="Rozu dried rose and custom wordmark" width="280" height="78">
</picture>


An open-source AI agent project. Working name: **Rozu**.


[Visit the landing page](https://freddyrosa16.github.io/rozu/)


The repository includes the landing page, a Python backend, and a Rust desktop scaffold. The app is still a work in progress.

## Motivation

Rozu is a project for learning how to build an open-source AI agent. The goal is to build it step by step, starting with the website and the backend.


## Website


A lightweight, single-screen introduction to Rozu. The original graphite, ordered-dither animation is inspired by the visual texture on the right of Capy's signup page. No Capy artwork, branding, or source code is included.


The page uses plain HTML, CSS, and JavaScript, with no build step, dependencies, analytics, or external font requests. The animation runs automatically. Visitors who prefer reduced motion see a still background; drawing also stops while the page is hidden.


## Quick Start

To run the landing page locally, you need Git and Python 3. There are no website dependencies to install or build steps to run.

```sh
git clone https://github.com/freddyrosa16/rozu.git
cd rozu
python3 -m http.server 8080
```

## Usage

Open `http://localhost:8080` in your browser to view the landing page. The background animation starts automatically. Press `Ctrl+C` in the terminal when you want to stop the server.

GitHub Pages publishes the repository root on `main`. These steps run the website only; they do not start the backend or desktop app.


- `index.html`: text, links, and accessible structure
- `styles.css`: layout, typography, colors, and responsive styles
- `waves.js`: decorative canvas and motion preferences
- `assets/brand/dried-rose/`: SVG rose and lettering used by the website and this README


The website does not run an agent or connect to a backend.


## Contributing

Contributions are welcome. For website changes:

1. Fork this repository and clone your fork.
2. Create a branch for your change: `git switch -c your-change`.
3. Follow the Quick Start instructions from the repository root to run the website locally.
4. Make your changes and check the page in your browser, including at a smaller screen size.
5. With Node.js installed, run the website animation checks from the repository root:

```sh
node --test tests/waves.test.cjs
```

6. Commit your changes, push your branch to your fork, and open a pull request. Explain what you changed and how you checked it.

For backend or desktop changes, open an issue first to discuss the idea while those parts are still being built.

## License


[MIT](LICENSE)


The background uses a full-field WebGL shader at the display refresh rate, with a 30fps Canvas 2D fallback when WebGL is unavailable. Fluid waves travel through every part of the canvas; there are no fixed left-side or text-area masks. The gray pixels stay subdued so the text remains readable.

