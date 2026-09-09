// Shared visual effect, adapted for React lifecycle cleanup.
export function startWaves(canvas) {
  'use strict';
  const media = matchMedia('(prefers-reduced-motion: reduce)');
  const bayer = [0,48,12,60,3,51,15,63,32,16,44,28,35,19,47,31,8,56,4,52,11,59,7,55,40,24,36,20,43,27,39,23,2,50,14,62,1,49,13,61,34,18,46,30,33,17,45,29,10,58,6,54,9,57,5,53,42,26,38,22,41,25,37,21];

  function gpuRenderer(gl) {
    const vertexSource = `
      attribute vec2 position;
      void main() { gl_Position = vec4(position, 0.0, 1.0); }
    `;
    const fragmentSource = `
      #ifdef GL_FRAGMENT_PRECISION_HIGH
        precision highp float;
      #else
        precision mediump float;
      #endif
      uniform vec2 resolution;
      uniform float time;
      uniform sampler2D thresholds;
      float hash(vec2 p) {
        return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
      }
      float noise(vec2 p) {
        vec2 cell = floor(p), f = fract(p);
        f = f * f * (3.0 - 2.0 * f);
        return mix(mix(hash(cell), hash(cell + vec2(1.0, 0.0)), f.x),
                   mix(hash(cell + vec2(0.0, 1.0)), hash(cell + vec2(1.0)), f.x), f.y);
      }
      float fbm(vec2 p) {
        float value = 0.0, weight = 0.5;
        mat2 rotation = mat2(0.8, -0.6, 0.6, 0.8);
        for (int i = 0; i < 4; i++) {
          value += weight * noise(p);
          p = rotation * p * 2.03 + vec2(7.3, 2.1);
          weight *= 0.5;
        }
        return value;
      }
      void main() {
        vec2 pixel = floor(gl_FragCoord.xy);
        vec2 uv = gl_FragCoord.xy / resolution;
        vec2 p = (uv - 0.5) * vec2(resolution.x / resolution.y, 1.0) * 3.2;
        float t = time * 0.22;
        vec2 warp = vec2(fbm(p + vec2(t * 0.18, -t * 0.12)),
                         fbm(p + vec2(4.7 - t * 0.13, 1.9 + t * 0.16)));
        vec2 flow = p + 1.8 * (warp - 0.5);
        float field = 0.50
          + 0.31 * sin(flow.x * 1.65 + flow.y * 1.2 + t)
          + 0.19 * cos(flow.y * 2.15 - flow.x * 0.8 - t * 0.7);
        float density = smoothstep(0.20, 0.90, field) * 0.70;
        float threshold = texture2D(thresholds, (mod(pixel, 8.0) + 0.5) / 8.0).r;
        // A narrow transition smooths each pixel's appearance without blurring its edges.
        float ink = smoothstep(threshold - 0.018, threshold + 0.018, density);
        float light = 0.051 + ink * (0.12 + 0.07 * density);
        gl_FragColor = vec4(vec3(light), 1.0);
      }
    `;
    function shader(type, source) {
      const item = gl.createShader(type);
      gl.shaderSource(item, source);
      gl.compileShader(item);
      if (!gl.getShaderParameter(item, gl.COMPILE_STATUS)) throw new Error('Wave shader compilation failed');
      return item;
    }
    const vertex = shader(gl.VERTEX_SHADER, vertexSource);
    const fragment = shader(gl.FRAGMENT_SHADER, fragmentSource);
    const program = gl.createProgram();
    gl.attachShader(program, vertex);
    gl.attachShader(program, fragment);
    gl.linkProgram(program);
    gl.deleteShader(vertex);
    gl.deleteShader(fragment);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) throw new Error('Wave shader linking failed');
    gl.useProgram(program);
    const buffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
    const position = gl.getAttribLocation(program, 'position');
    gl.enableVertexAttribArray(position);
    gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);
    const texture = gl.createTexture();
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, texture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    const values = new Uint8Array(bayer.map(value => Math.round((value + 0.5) / 64 * 255)));
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.LUMINANCE, 8, 8, 0, gl.LUMINANCE, gl.UNSIGNED_BYTE, values);
    gl.uniform1i(gl.getUniformLocation(program, 'thresholds'), 0);
    const size = gl.getUniformLocation(program, 'resolution');
    const clock = gl.getUniformLocation(program, 'time');
    canvas.dataset.renderer = 'webgl';
    return {
      minFrameMs: 0,
      resize(width, height) {
        // One backing pixel per ~2 CSS pixels keeps the fine dither grid and GPU load predictable.
        const scale = Math.max(2, width / 1200, height / 900);
        canvas.width = Math.max(1, Math.round(width / scale));
        canvas.height = Math.max(1, Math.round(height / scale));
        gl.viewport(0, 0, canvas.width, canvas.height);
        gl.uniform2f(size, canvas.width, canvas.height);
      },
      draw(seconds) { gl.uniform1f(clock, seconds); gl.drawArrays(gl.TRIANGLES, 0, 3); }
    };
  }

  function cpuRenderer() {
    const ctx = canvas.getContext('2d', {alpha: false});
    if (!ctx) return null;
    let pixels;
    canvas.dataset.renderer = '2d';
    return {
      minFrameMs: 1000 / 30,
      resize(width, height) {
        const scale = Math.max(3, width / 320, height / 450);
        canvas.width = Math.max(1, Math.round(width / scale));
        canvas.height = Math.max(1, Math.round(height / scale));
        pixels = ctx.createImageData(canvas.width, canvas.height);
      },
      draw(seconds) {
        const {width, height} = canvas;
        const t = seconds * 0.22;
        for (let y = 0; y < height; y++) {
          for (let x = 0; x < width; x++) {
            const px = (x / width - 0.5) * width / height * 3.2;
            const py = (y / height - 0.5) * 3.2;
            const bend = Math.sin(px * 1.7 + py * 1.2 + t * 0.3) * 0.7;
            const field = 0.5 + 0.31 * Math.sin(px * 1.65 + py * 1.2 + bend + t)
              + 0.19 * Math.cos(py * 2.15 - px * 0.8 - t * 0.7);
            const f = Math.max(0, Math.min(1, (field - 0.2) / 0.7));
            const density = f * f * (3 - 2 * f) * 0.7;
            const threshold = (bayer[(y % 8) * 8 + x % 8] + 0.5) / 64;
            const ink = Math.max(0, Math.min(1, (density - threshold + 0.018) / 0.036));
            const light = Math.round(13 + ink * (31 + 18 * density));
            const offset = (y * width + x) * 4;
            pixels.data[offset] = pixels.data[offset + 1] = pixels.data[offset + 2] = light;
            pixels.data[offset + 3] = 255;
          }
        }
        ctx.putImageData(pixels, 0, 0);
      }
    };
  }

  let gl;
  let renderer;
  function initialize() {
    try {
      gl = canvas.getContext('webgl', {alpha:false, antialias:false, depth:false, stencil:false, powerPreference:'low-power'});
      renderer = gl ? gpuRenderer(gl) : cpuRenderer();
    } catch {
      // The page and navigation remain usable if a graphics driver cannot initialize.
      renderer = null;
    }
  }
  let paused = media.matches;
  let visible = true;
  let contextLost = false;
  let elapsed = 0;
  let last = null;
  let lastDraw = -Infinity;
  let frame = 0;
  function tick(now) {
    frame = 0;
    if (paused || document.hidden || !visible || contextLost || !renderer) {last = null; return;}
    if (last !== null) elapsed += Math.min(now - last, 100);
    last = now;
    if (elapsed - lastDraw >= renderer.minFrameMs) {
      renderer.draw(elapsed / 1000);
      lastDraw = elapsed;
    }
    frame = requestAnimationFrame(tick);
  }
  function sync() {
    cancelAnimationFrame(frame);
    frame = 0;
    last = null;
    lastDraw = -Infinity;
    if (!paused && !document.hidden && visible && !contextLost && renderer) frame = requestAnimationFrame(tick);
  }
  function resize() {
    if (!renderer || contextLost) return;
    const bounds = canvas.getBoundingClientRect();
    renderer.resize(bounds.width, bounds.height);
    renderer.draw(elapsed / 1000);
  }
  const preferenceChanged = () => {paused = media.matches; sync();};
  media.addEventListener('change', preferenceChanged);
  document.addEventListener('visibilitychange', sync);
  canvas.addEventListener('webglcontextlost', event => {event.preventDefault(); contextLost = true; sync();});
  canvas.addEventListener('webglcontextrestored', () => {contextLost = false; initialize(); resize(); sync();});
  const resizeObserver = new ResizeObserver(resize);
  resizeObserver.observe(canvas);
  const intersectionObserver = new IntersectionObserver(([entry]) => {visible = entry.isIntersecting; sync();});
  intersectionObserver.observe(canvas);
  initialize();
  resize();
  sync();
  return () => {
    paused = true;
    cancelAnimationFrame(frame);
    resizeObserver.disconnect();
    intersectionObserver.disconnect();
    media.removeEventListener('change', preferenceChanged);
    document.removeEventListener('visibilitychange', sync);
    gl?.getExtension('WEBGL_lose_context')?.loseContext();
  };
}
