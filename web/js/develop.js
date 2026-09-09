/* =========================================================================
   develop.js: a small raw-develop pipeline running in WebGL.
   This is an honest approximation of the operations Lightbox performs, done
   in the browser so the page can grade a real photograph as you scroll. The
   application itself runs the same kinds of operations on a GPU compute
   graph in floating point, at full resolution, with far more care.
   ========================================================================= */
(function () {
  'use strict';

  var VERT = [
    'attribute vec2 a_pos;',
    'varying vec2 v_uv;',
    'void main(){',
    '  v_uv = vec2((a_pos.x + 1.0) * 0.5, 1.0 - (a_pos.y + 1.0) * 0.5);',
    '  gl_Position = vec4(a_pos, 0.0, 1.0);',
    '}'
  ].join('\n');

  var FRAG = [
    'precision highp float;',
    'varying vec2 v_uv;',
    'uniform sampler2D u_tex;',
    'uniform vec2  u_scale;',    // cover-fit correction
    'uniform vec2  u_offset;',
    'uniform float u_temp;',     // -1 cool .. +1 warm
    'uniform float u_tint;',
    'uniform float u_exposure;', // stops
    'uniform float u_contrast;',
    'uniform float u_high;',
    'uniform float u_shadow;',
    'uniform float u_white;',
    'uniform float u_black;',
    'uniform float u_vibrance;',
    'uniform float u_sat;',
    'uniform float u_fade;',
    'uniform float u_vignette;',
    'uniform float u_grain;',
    'uniform vec3  u_shadowTint;',
    'uniform vec3  u_highTint;',
    'uniform float u_seed;',

    'const vec3 LUMA = vec3(0.2126, 0.7152, 0.0722);',

    'vec3 toLinear(vec3 c){ return pow(max(c, 0.0), vec3(2.2)); }',
    'vec3 toSrgb(vec3 c){ return pow(max(c, 0.0), vec3(1.0/2.2)); }',

    'float hash(vec2 p){',
    '  return fract(sin(dot(p, vec2(127.1, 311.7)) + u_seed) * 43758.5453);',
    '}',

    'void main(){',
    '  vec2 uv = v_uv * u_scale + u_offset;',
    '  vec3 c = texture2D(u_tex, clamp(uv, 0.0, 1.0)).rgb;',
    '  c = toLinear(c);',

    // --- white balance: channel gains around a neutral pivot -------------
    '  float t = u_temp;',
    '  c.r *= 1.0 + t * 0.34;',
    '  c.b *= 1.0 - t * 0.30;',
    '  c.g *= 1.0 + u_tint * 0.18;',
    '  c.r *= 1.0 - u_tint * 0.07;',

    // --- exposure, in linear light ---------------------------------------
    '  c *= pow(2.0, u_exposure);',

    // --- highlight / shadow recovery, masked by luminance -----------------
    '  float y = dot(c, LUMA);',
    '  float hiMask = smoothstep(0.36, 1.05, y);',
    '  float loMask = 1.0 - smoothstep(0.0, 0.42, y);',
    '  c *= 1.0 + u_high * hiMask * 0.85;',
    '  c += u_shadow * loMask * 0.11;',

    // --- whites / blacks --------------------------------------------------
    '  c *= 1.0 + u_white * 0.24;',
    '  c += u_black * 0.055;',

    // --- contrast around a middle-grey pivot ------------------------------
    '  const float PIVOT = 0.18;',
    '  c = max(c, 0.0);',
    '  c = PIVOT * pow(c / PIVOT + 0.0001, vec3(1.0 + u_contrast * 0.62));',

    // --- back to display-referred ----------------------------------------
    '  c = toSrgb(c);',

    // --- saturation and vibrance -----------------------------------------
    '  float ly = dot(c, LUMA);',
    '  float chroma = max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b);',
    '  float vibAmt = u_vibrance * (1.0 - smoothstep(0.05, 0.62, chroma));',
    '  c = mix(vec3(ly), c, 1.0 + u_sat * 0.9 + vibAmt * 1.15);',

    // --- split tone -------------------------------------------------------
    '  float sMask = 1.0 - smoothstep(0.0, 0.55, ly);',
    '  float hMask = smoothstep(0.45, 1.0, ly);',
    '  c += u_shadowTint * sMask * 0.16;',
    '  c += u_highTint  * hMask * 0.13;',

    // --- matte / faded blacks --------------------------------------------
    '  c = mix(c, c * (1.0 - u_fade * 0.22) + u_fade * 0.075, 1.0);',

    // --- vignette ---------------------------------------------------------
    '  vec2 d = v_uv - 0.5;',
    '  float r = length(d * vec2(1.05, 1.0));',
    '  c *= 1.0 - u_vignette * smoothstep(0.24, 0.82, r);',

    // --- grain ------------------------------------------------------------
    '  float g = hash(gl_FragCoord.xy) - 0.5;',
    '  c += g * u_grain * 0.09;',

    '  gl_FragColor = vec4(clamp(c, 0.0, 1.0), 1.0);',
    '}'
  ].join('\n');

  var DEFAULTS = {
    temp: 0, tint: 0, exposure: 0, contrast: 0, high: 0, shadow: 0,
    white: 0, black: 0, vibrance: 0, sat: 0, fade: 0, vignette: 0,
    grain: 0, shadowTint: [0, 0, 0], highTint: [0, 0, 0]
  };

  function compile(gl, type, src) {
    var s = gl.createShader(type);
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
      console.warn('shader:', gl.getShaderInfoLog(s));
      return null;
    }
    return s;
  }

  /** A WebGL develop surface bound to one canvas and one image. */
  function Developer(canvas, imageUrl, opts) {
    this.canvas = canvas;
    this.opts = opts || {};
    this.params = Object.assign({}, DEFAULTS);
    this.ready = false;
    this.dirty = true;
    this._onReady = [];

    var gl = canvas.getContext('webgl', {
      alpha: false, antialias: false, depth: false,
      preserveDrawingBuffer: false, powerPreference: 'high-performance'
    });
    if (!gl) return;
    this.gl = gl;

    var vs = compile(gl, gl.VERTEX_SHADER, VERT);
    var fs = compile(gl, gl.FRAGMENT_SHADER, FRAG);
    if (!vs || !fs) return;

    var p = gl.createProgram();
    gl.attachShader(p, vs);
    gl.attachShader(p, fs);
    gl.linkProgram(p);
    if (!gl.getProgramParameter(p, gl.LINK_STATUS)) {
      console.warn('link:', gl.getProgramInfoLog(p));
      return;
    }
    gl.useProgram(p);
    this.program = p;

    var buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1,-1, 3,-1, -1,3]), gl.STATIC_DRAW);
    var loc = gl.getAttribLocation(p, 'a_pos');
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);

    // Small offscreen target used for histogram read-back. Reading the full
    // canvas would stall the GPU every frame; 128x80 costs almost nothing.
    this.hbW = 128; this.hbH = 80;
    var htex = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, htex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, this.hbW, this.hbH, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    this.hFbo = gl.createFramebuffer();
    gl.bindFramebuffer(gl.FRAMEBUFFER, this.hFbo);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, htex, 0);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    this.hPixels = new Uint8Array(this.hbW * this.hbH * 4);

    this.u = {};
    ['u_tex','u_scale','u_offset','u_temp','u_tint','u_exposure','u_contrast',
     'u_high','u_shadow','u_white','u_black','u_vibrance','u_sat','u_fade',
     'u_vignette','u_grain','u_shadowTint','u_highTint','u_seed'
    ].forEach(function (n) { this.u[n] = gl.getUniformLocation(p, n); }, this);

    gl.uniform1f(this.u.u_seed, Math.random() * 100);

    var self = this;
    var img = new Image();
    img.crossOrigin = 'anonymous';
    img.onload = function () {
      var tex = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, tex);
      gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGB, gl.RGB, gl.UNSIGNED_BYTE, img);
      gl.uniform1i(self.u.u_tex, 0);
      self.imgW = img.naturalWidth;
      self.imgH = img.naturalHeight;
      self.ready = true;
      self.resize();
      self.render();
      self._onReady.forEach(function (f) { f(self); });
      self._onReady.length = 0;
    };
    img.onerror = function () { console.warn('develop: image failed', imageUrl); };
    img.src = imageUrl;
  }

  Developer.prototype.onReady = function (fn) {
    if (this.ready) fn(this); else this._onReady.push(fn);
  };

  Developer.prototype.resize = function () {
    if (!this.gl) return;
    var c = this.canvas;
    var dpr = Math.min(window.devicePixelRatio || 1, this.opts.maxDpr || 2);
    var w = Math.max(1, Math.round(c.clientWidth * dpr));
    var h = Math.max(1, Math.round(c.clientHeight * dpr));
    if (c.width !== w || c.height !== h) {
      c.width = w; c.height = h;
    }
    this.gl.viewport(0, 0, c.width, c.height);
    this.dirty = true;
  };

  Developer.prototype.set = function (patch) {
    var changed = false;
    for (var k in patch) {
      if (this.params[k] !== patch[k]) { this.params[k] = patch[k]; changed = true; }
    }
    if (changed) this.dirty = true;
    return changed;
  };

  Developer.prototype.render = function () {
    if (!this.gl || !this.ready) return;
    var gl = this.gl, p = this.params, c = this.canvas;

    // cover-fit the texture into the canvas without distortion
    var canvasAR = c.width / c.height;
    var imgAR = this.imgW / this.imgH;
    var sx = 1, sy = 1;
    if (this.opts.fit === 'contain') {
      if (canvasAR > imgAR) { sx = canvasAR / imgAR; } else { sy = imgAR / canvasAR; }
    } else {
      if (canvasAR > imgAR) { sy = imgAR / canvasAR; } else { sx = canvasAR / imgAR; }
    }
    gl.uniform2f(this.u.u_scale, sx, sy);
    gl.uniform2f(this.u.u_offset, (1 - sx) / 2, (1 - sy) / 2);

    gl.uniform1f(this.u.u_temp, p.temp);
    gl.uniform1f(this.u.u_tint, p.tint);
    gl.uniform1f(this.u.u_exposure, p.exposure);
    gl.uniform1f(this.u.u_contrast, p.contrast);
    gl.uniform1f(this.u.u_high, p.high);
    gl.uniform1f(this.u.u_shadow, p.shadow);
    gl.uniform1f(this.u.u_white, p.white);
    gl.uniform1f(this.u.u_black, p.black);
    gl.uniform1f(this.u.u_vibrance, p.vibrance);
    gl.uniform1f(this.u.u_sat, p.sat);
    gl.uniform1f(this.u.u_fade, p.fade);
    gl.uniform1f(this.u.u_vignette, p.vignette);
    gl.uniform1f(this.u.u_grain, p.grain);
    gl.uniform3fv(this.u.u_shadowTint, p.shadowTint);
    gl.uniform3fv(this.u.u_highTint, p.highTint);

    gl.drawArrays(gl.TRIANGLES, 0, 3);
    this.dirty = false;
  };

  /**
   * Renders one extra pass into a 128x80 offscreen target and bins an RGB
   * histogram from it. One small read-back rather than a full-canvas stall,
   * which is roughly the reason the real application computes its histogram
   * in the render pass and reads it back without blocking the frame.
   */
  Developer.prototype.histogram = function (bins) {
    if (!this.gl || !this.ready) return null;
    var gl = this.gl;

    gl.bindFramebuffer(gl.FRAMEBUFFER, this.hFbo);
    gl.viewport(0, 0, this.hbW, this.hbH);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    gl.readPixels(0, 0, this.hbW, this.hbH, gl.RGBA, gl.UNSIGNED_BYTE, this.hPixels);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);

    var r = new Float32Array(bins), g = new Float32Array(bins), b = new Float32Array(bins);
    var px = this.hPixels, n = px.length, k = (bins - 1) / 255;
    for (var i = 0; i < n; i += 4) {
      r[(px[i] * k) | 0]++;
      g[(px[i + 1] * k) | 0]++;
      b[(px[i + 2] * k) | 0]++;
    }
    return { r: r, g: g, b: b };
  };

  window.LightboxDevelop = { Developer: Developer, DEFAULTS: DEFAULTS };
})();
