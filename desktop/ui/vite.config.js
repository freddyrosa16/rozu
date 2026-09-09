import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  base: './',
  plugins: [react(), {
    name: 'bundled-file-script',
    transformIndexHtml: {
      order: 'post',
      handler(html, context) {
        if (context.server) {
          // Vite's local development refresh only; never included in the Mac bundle.
          return html.replace("script-src 'self'", "script-src 'self' 'unsafe-inline'")
            .replace("connect-src 'none'", "connect-src 'self' ws://127.0.0.1:*");
        }
        // A classic self-contained script also works with WKWebView file URLs.
        return context.bundle ? html.replace('type="module" crossorigin', 'defer').replace(/ crossorigin/g, '') : html;
      },
    },
  }],
  build: {
    target: 'safari16',
    modulePreload: false,
    rollupOptions: { output: { format: 'iife', inlineDynamicImports: true } },
  },
});
