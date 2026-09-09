/* Original ordered-dither wave artwork. No dependencies or network requests. */
(() => {
  'use strict';
  const canvas = document.getElementById('waves');
  const ctx = canvas.getContext('2d', { alpha: false });
  if (!ctx) return;
  const button = document.getElementById('motion-toggle');
  const label = document.getElementById('motion-label');
  const media = matchMedia('(prefers-reduced-motion: reduce)');
  const bayer = [0,48,12,60,3,51,15,63,32,16,44,28,35,19,47,31,8,56,4,52,11,59,7,55,40,24,36,20,43,27,39,23,2,50,14,62,1,49,13,61,34,18,46,30,33,17,45,29,10,58,6,54,9,57,5,53,42,26,38,22,41,25,37,21];
  let paused = media.matches;
  let visible = true;
  let elapsed = 0;
  let last = 0;
  let frame = 0;
  let pixels;
  let columns;
  let rows;
  let mobile = false;
  let waveColor = [100, 145, 196];
  let baseColor = [11, 13, 16];
  function updatePalette() {
    const style = getComputedStyle(canvas);
    waveColor = style.getPropertyValue('--wave-rgb').split(',').map(Number);
    baseColor = style.getPropertyValue('--base-rgb').split(',').map(Number);
    if (pixels) draw();
  }
  document.querySelectorAll('input[name="palette"]').forEach(input => {
    input.addEventListener('change', () => {
      document.body.dataset.theme = input.value;
      document.querySelector('meta[name="theme-color"]').content = input.value === 'blue' ? '#0b0d10' : '#0d0d0d';
      updatePalette();
    });
  });
  function draw() {
    const data = pixels.data;
    const time = elapsed * 0.000018;
    for (let y = 0; y < rows; y++) {
      const ny = y / rows;
      const center = 0.77 + 0.26 * Math.sin(ny * 6.1 - 1.1 + time);
      const width = 0.08 + 0.035 * Math.sin(ny * 8 + time * 0.6);
      for (let x = 0; x < columns; x++) {
        const nx = x / columns;
        const bend = center + 0.018 * Math.sin(nx * 19 + ny * 23 + time);
        const distance = (nx - bend) / width;
        const broad = (nx - center) / 0.24;
        const ripple = (Math.sin(nx * 37 - ny * 29 + time * 0.7) + 1) * 0.5;
        let intensity = Math.exp(-distance * distance) * (0.26 + 0.15 * ripple) + Math.exp(-broad * broad) * 0.17;
        const clearText = mobile ? 0.30 + 0.70 * Math.max(0, (ny - 0.42) / 0.58) : Math.min(1, Math.max(0, (nx - 0.28) / 0.5));
        const quietEdges = Math.max(0, Math.min(1, ny / 0.14, (0.94 - ny) / 0.14));
        const quietHero = (ny > 0.23 && ny < 0.73) ? (mobile ? 0.35 : Math.min(1, Math.max(0, (nx - 0.53) / 0.19))) : 1;
        intensity *= clearText * quietEdges * quietHero;
        const threshold = (bayer[(y % 8) * 8 + x % 8] + 0.5) / 64;
        const on = intensity > threshold;
        const brightness = 0.46 + 0.54 * Math.min(1, intensity * 2.4);
        const i = (y * columns + x) * 4;
        data[i] = on ? Math.round(waveColor[0] * brightness) : baseColor[0];
        data[i + 1] = on ? Math.round(waveColor[1] * brightness) : baseColor[1];
        data[i + 2] = on ? Math.round(waveColor[2] * brightness) : baseColor[2];
        data[i + 3] = 255;
      }
    }
    ctx.putImageData(pixels, 0, 0);
  }
  function tick(now) {
    frame = 0;
    if (paused || document.hidden || !visible) { last = 0; return; }
    if (!last) last = now;
    if (now - last >= 1000 / 15) {
      elapsed += Math.min(now - last, 100);
      last = now;
      draw();
    }
    frame = requestAnimationFrame(tick);
  }
  function sync() {
    cancelAnimationFrame(frame);
    frame = 0;
    last = 0;
    button.classList.toggle('is-paused', paused);
    label.textContent = paused ? 'Play animation' : 'Pause animation';
    button.setAttribute('aria-label', label.textContent);
    if (!paused && !document.hidden && visible) frame = requestAnimationFrame(tick);
  }
  function resize() {
    const bounds = canvas.getBoundingClientRect();
    mobile = bounds.width < 600;
    columns = Math.max(1, Math.min(640, Math.round(bounds.width / 3)));
    rows = Math.max(1, Math.round(bounds.height / (bounds.width / columns)));
    canvas.width = columns;
    canvas.height = rows;
    pixels = ctx.createImageData(columns, rows);
    draw();
  }
  button.hidden = false;
  button.addEventListener('click', () => { paused = !paused; sync(); });
  media.addEventListener('change', () => { paused = media.matches; sync(); });
  document.addEventListener('visibilitychange', sync);
  new ResizeObserver(resize).observe(canvas);
  new IntersectionObserver(([entry]) => { visible = entry.isIntersecting; sync(); }).observe(canvas);
  updatePalette();
  resize();
  sync();
})();
