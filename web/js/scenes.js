/* =========================================================================
   scenes.js: the small canvas set pieces illustrating the develop rail.

   Motion policy: nothing here loops forever. A scene draws once when it
   first scrolls into view and then rests as a static, legible diagram.
   The two scenes with a single value to communicate (white balance,
   straighten) play a short one-shot settle so the value visibly "arrives"
   the first time you see them. The tone curve keeps redrawing only in
   direct response to a pointer, because that is a real control, not a
   decoration. Nothing here runs on a timer once it has said its piece.
   ========================================================================= */
(function () {
  'use strict';

  var REDUCED = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  var INK      = '#eef1f4';
  var INK_3    = '#8a939c';
  var INK_4    = '#5d666e';
  var LINE     = 'rgba(255,255,255,.10)';
  var ACCENT   = '#5583a8';
  var ACCENT_HI= '#74a3c8';
  var WARM     = '#e0913c';
  var MAG      = '#c4577f';
  var COOL     = '#47b4cf';

  /* ---------- shared loop -------------------------------------------------
     A scene draws every frame while it is visible, unless it is marked
     `once`, in which case the loop skips it after its first draw and only
     wakes it again if something sets scene.forceRedraw (an interaction, or
     a resize). A scene can also flip its own `once` flag mid-animation,
     which is how the one-shot settles below stop themselves rather than
     running forever.
     ----------------------------------------------------------------------- */
  var scenes = [];
  var running = false;

  function fit(canvas) {
    var dpr = Math.min(window.devicePixelRatio || 1, 2);
    var w = Math.max(1, Math.round(canvas.clientWidth * dpr));
    var h = Math.max(1, Math.round(canvas.clientHeight * dpr));
    if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
    var ctx = canvas.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    return { ctx: ctx, w: canvas.clientWidth, h: canvas.clientHeight, dpr: dpr };
  }

  function register(canvas, draw, opts) {
    if (!canvas) return null;
    var scene = {
      canvas: canvas, draw: draw, visible: false, t: 0,
      once: !!(opts && opts.once), drawnOnce: false
    };
    scenes.push(scene);

    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) {
        scene.visible = e.isIntersecting;
        if (e.isIntersecting) start();
      });
    }, { rootMargin: '120px' });
    io.observe(canvas);

    var ro = new ResizeObserver(function () { scene.drawnOnce = false; scene.forceRedraw = true; start(); });
    ro.observe(canvas);
    return scene;
  }

  var last = 0;
  function frame(now) {
    var dt = last ? Math.min(Math.max((now - last) / 1000, 0), 0.05) : 0;
    last = now;
    var anyVisible = false;
    for (var i = 0; i < scenes.length; i++) {
      var s = scenes[i];
      if (!s.visible) continue;
      anyVisible = true;
      if (s.once && s.drawnOnce && !s.forceRedraw) continue;
      s.t += dt;
      var g = fit(s.canvas);
      g.ctx.clearRect(0, 0, g.w, g.h);
      s.draw(g.ctx, g.w, g.h, s.t, s);
      s.drawnOnce = true;
      s.forceRedraw = false;
    }
    if (anyVisible || !running) requestAnimationFrame(frame);
    else running = false;
  }
  function start() {
    if (running) return;
    running = true; last = 0;
    requestAnimationFrame(frame);
  }

  /* ---------- helpers ---------------------------------------------------- */
  function roundRect(ctx, x, y, w, h, r) {
    ctx.beginPath();
    ctx.moveTo(x + r, y);
    ctx.arcTo(x + w, y, x + w, y + h, r);
    ctx.arcTo(x + w, y + h, x, y + h, r);
    ctx.arcTo(x, y + h, x, y, r);
    ctx.arcTo(x, y, x + w, y, r);
    ctx.closePath();
  }
  function ease(t) { return t < .5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2; }
  function lerp(a, b, t) { return a + (b - a) * t; }
  function clamp01(v) { return v < 0 ? 0 : v > 1 ? 1 : v; }


  /* =======================================================================
     2. TONE CURVE, interactive
     No idle motion. The curve sits still until you touch it; hovering a
     handle or dragging it is the only thing that asks for a redraw.
     ======================================================================= */
  (function curveScene() {
    var canvas = document.getElementById('curveDemo');
    if (!canvas) return;

    var pts = [{ x: 0, y: 0 }, { x: .26, y: .19 }, { x: .62, y: .71 }, { x: 1, y: 1 }];
    var drag = -1, hover = -1;

    function toCanvas(p, pad, w, h) {
      return { x: pad + p.x * (w - pad * 2), y: h - pad - p.y * (h - pad * 2) };
    }
    function evalSpline(x) {
      for (var i = 0; i < pts.length - 1; i++) {
        if (x >= pts[i].x && x <= pts[i + 1].x) {
          var p0 = pts[Math.max(0, i - 1)], p1 = pts[i], p2 = pts[i + 1], p3 = pts[Math.min(pts.length - 1, i + 2)];
          var span = (p2.x - p1.x) || 1e-6;
          var t = (x - p1.x) / span, t2 = t * t, t3 = t2 * t;
          var y = 0.5 * ((2 * p1.y) + (-p0.y + p2.y) * t +
                  (2 * p0.y - 5 * p1.y + 4 * p2.y - p3.y) * t2 +
                  (-p0.y + 3 * p1.y - 3 * p2.y + p3.y) * t3);
          return Math.min(1, Math.max(0, y));
        }
      }
      return x;
    }

    var scene = register(canvas, function (ctx, w, h) {
      var pad = 26;

      // grid
      ctx.strokeStyle = 'rgba(255,255,255,.06)';
      ctx.lineWidth = 1;
      for (var g = 0; g <= 4; g++) {
        var gx = pad + (w - pad * 2) * g / 4;
        var gy = pad + (h - pad * 2) * g / 4;
        ctx.beginPath(); ctx.moveTo(gx, pad); ctx.lineTo(gx, h - pad); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(pad, gy); ctx.lineTo(w - pad, gy); ctx.stroke();
      }
      // identity
      ctx.strokeStyle = 'rgba(255,255,255,.13)';
      ctx.setLineDash([3, 4]);
      ctx.beginPath(); ctx.moveTo(pad, h - pad); ctx.lineTo(w - pad, pad); ctx.stroke();
      ctx.setLineDash([]);

      // curve
      var grad = ctx.createLinearGradient(pad, 0, w - pad, 0);
      grad.addColorStop(0, ACCENT);
      grad.addColorStop(1, ACCENT_HI);
      ctx.strokeStyle = grad;
      ctx.lineWidth = 2;
      ctx.beginPath();
      for (var i = 0; i <= 120; i++) {
        var x = i / 120;
        var p = toCanvas({ x: x, y: evalSpline(x) }, pad, w, h);
        if (i === 0) ctx.moveTo(p.x, p.y); else ctx.lineTo(p.x, p.y);
      }
      ctx.stroke();

      // fill under
      ctx.lineTo(w - pad, h - pad); ctx.lineTo(pad, h - pad); ctx.closePath();
      ctx.fillStyle = 'rgba(85,131,168,.10)';
      ctx.fill();

      // handles
      pts.forEach(function (p, i) {
        var c = toCanvas(p, pad, w, h);
        var active = (i === drag || i === hover);
        ctx.beginPath();
        ctx.arc(c.x, c.y, active ? 6.5 : 5, 0, Math.PI * 2);
        ctx.fillStyle = active ? '#ffffff' : '#cfd8e0';
        ctx.shadowColor = 'rgba(85,131,168,.75)';
        ctx.shadowBlur = active ? 16 : 7;
        ctx.fill();
        ctx.shadowBlur = 0;
      });
    }, { once: true });

    function local(ev) {
      var r = canvas.getBoundingClientRect();
      var cx = (ev.touches ? ev.touches[0].clientX : ev.clientX) - r.left;
      var cy = (ev.touches ? ev.touches[0].clientY : ev.clientY) - r.top;
      var pad = 26;
      return {
        x: Math.min(1, Math.max(0, (cx - pad) / (r.width - pad * 2))),
        y: Math.min(1, Math.max(0, 1 - (cy - pad) / (r.height - pad * 2))),
        px: cx, py: cy, r: r
      };
    }
    function nearest(pt) {
      var best = -1, bd = 1e9, pad = 26;
      pts.forEach(function (p, i) {
        var c = { x: pad + p.x * (pt.r.width - pad * 2), y: pt.r.height - pad - p.y * (pt.r.height - pad * 2) };
        var d = Math.hypot(c.x - pt.px, c.y - pt.py);
        if (d < bd) { bd = d; best = i; }
      });
      return bd < 22 ? best : -1;
    }

    canvas.addEventListener('pointerdown', function (e) {
      var pt = local(e);
      drag = nearest(pt);
      if (drag > 0 && drag < pts.length - 1) canvas.setPointerCapture(e.pointerId);
      if (scene) scene.forceRedraw = true;
    });
    canvas.addEventListener('pointermove', function (e) {
      var pt = local(e);
      var nextHover = nearest(pt);
      if (nextHover !== hover) { hover = nextHover; if (scene) scene.forceRedraw = true; }
      canvas.style.cursor = hover >= 0 ? 'grab' : 'default';
      if (drag > 0 && drag < pts.length - 1) {
        pts[drag].y = pt.y;
        pts[drag].x = Math.min(pts[drag + 1].x - .06, Math.max(pts[drag - 1].x + .06, pt.x));
        canvas.style.cursor = 'grabbing';
        if (scene) scene.forceRedraw = true;
      }
    });
    ['pointerup', 'pointercancel', 'pointerleave'].forEach(function (n) {
      canvas.addEventListener(n, function () {
        if (drag !== -1 || hover !== -1) { drag = -1; hover = -1; if (scene) scene.forceRedraw = true; }
        canvas.style.cursor = 'default';
      });
    });
  })();

  /* =======================================================================
     3. HSL BANDS
     A static reference grade: eight bands, each pushed a different amount,
     drawn once. It is a diagram of the control, not a demo of motion.
     ======================================================================= */
  (function hslScene() {
    var canvas = document.getElementById('hslDemo');
    if (!canvas) return;
    var BANDS = ['Red', 'Orange', 'Yellow', 'Green', 'Aqua', 'Blue', 'Purple', 'Magenta'];
    var HUES  = [4, 30, 52, 128, 184, 218, 268, 318];
    // fixed, varied positions (0.5 = centre / no change), a plausible grade
    var AMT   = [0.64, 0.74, 0.42, 0.70, 0.30, 0.80, 0.52, 0.36];

    register(canvas, function (ctx, w, h) {
      var pad = 18;
      var n = BANDS.length;
      var rowH = (h - pad * 2) / n;
      for (var i = 0; i < n; i++) {
        var y = pad + i * rowH;
        var amt = AMT[i];

        ctx.textAlign = 'left';
        ctx.font = '400 9.5px "JetBrains Mono", monospace';
        ctx.fillStyle = INK_4;
        ctx.fillText(BANDS[i].toUpperCase(), pad, y + rowH * 0.62);

        var tx = pad + 62, tw = w - pad - tx;
        ctx.fillStyle = 'rgba(255,255,255,.07)';
        roundRect(ctx, tx, y + rowH / 2 - 2, tw, 4, 2); ctx.fill();

        var mid = tx + tw / 2;
        var half = (amt - 0.5) * tw;
        ctx.fillStyle = 'hsl(' + HUES[i] + ', 62%, 58%)';
        roundRect(ctx, Math.min(mid, mid + half), y + rowH / 2 - 2, Math.abs(half), 4, 2);
        ctx.fill();

        var kx = mid + half;
        ctx.beginPath();
        ctx.arc(kx, y + rowH / 2, 4.6, 0, Math.PI * 2);
        ctx.fillStyle = '#dfe6ec';
        ctx.shadowColor = 'hsla(' + HUES[i] + ', 70%, 55%, .8)';
        ctx.shadowBlur = 12; ctx.fill(); ctx.shadowBlur = 0;
      }
    }, { once: true });
  })();

  /* =======================================================================
     4. WHITE BALANCE RAMP
     A single value gauge. It settles into position once, the first time it
     is seen, because "the solver arrives at a value" is a real event worth
     a brief motion; after that it holds still.
     ======================================================================= */
  (function wbScene() {
    var canvas = document.getElementById('wbDemo');
    if (!canvas) return;
    var TARGET = 0.372;   // settles at 5600 K
    var DUR = 0.6;

    register(canvas, function (ctx, w, h, t, s) {
      var pad = 24;
      var barY = h * 0.52, barH = 16;
      var barX = pad, barW = w - pad * 2;

      var localT = REDUCED ? 1 : clamp01(t / DUR);
      var phase = lerp(0.5, TARGET, ease(localT));

      var g = ctx.createLinearGradient(barX, 0, barX + barW, 0);
      g.addColorStop(0.00, '#6ea8d8');
      g.addColorStop(0.35, '#b9cfe0');
      g.addColorStop(0.50, '#e8e6e0');
      g.addColorStop(0.68, '#efd3a4');
      g.addColorStop(1.00, '#e39a4a');
      ctx.fillStyle = g;
      roundRect(ctx, barX, barY - barH / 2, barW, barH, barH / 2);
      ctx.fill();
      ctx.strokeStyle = 'rgba(0,0,0,.35)'; ctx.lineWidth = 1; ctx.stroke();

      var kx = barX + phase * barW;
      var kelvin = Math.round(2400 + phase * 8600);

      ctx.beginPath();
      ctx.arc(kx, barY, 9, 0, Math.PI * 2);
      ctx.fillStyle = '#f2f5f8';
      ctx.shadowColor = 'rgba(85,131,168,.9)'; ctx.shadowBlur = 18;
      ctx.fill(); ctx.shadowBlur = 0;
      ctx.strokeStyle = 'rgba(0,0,0,.4)'; ctx.stroke();

      ctx.textAlign = 'center';
      ctx.font = '500 15px "JetBrains Mono", monospace';
      ctx.fillStyle = INK;
      ctx.fillText(kelvin + ' K', w / 2, barY - 38);
      ctx.font = '400 9.5px "JetBrains Mono", monospace';
      ctx.fillStyle = INK_4;
      ctx.fillText('SOLVED AGAINST THE CAMERA MATRIX', w / 2, barY - 22);

      ctx.textAlign = 'left';
      ctx.fillText('2400', barX, barY + 30);
      ctx.textAlign = 'right';
      ctx.fillText('11000', barX + barW, barY + 30);

      if (localT >= 1) s.once = true;
    }, { once: false });
  })();

  /* =======================================================================
     5. CROP AND STRAIGHTEN
     Also a single value gauge: the frame settles level once, the image
     tilts to its final angle once, then both hold still.
     ======================================================================= */
  (function cropScene() {
    var canvas = document.getElementById('cropDemo');
    if (!canvas) return;
    var TARGET_ANG = -0.038; // radians, final straighten angle
    var DUR = 0.65;

    register(canvas, function (ctx, w, h, t, s) {
      var cx = w / 2, cy = h / 2;
      var iw = Math.min(w - 56, (h - 56) * 1.5), ih = iw / 1.5;

      var localT = REDUCED ? 1 : clamp01(t / DUR);
      var ang = lerp(0, TARGET_ANG, ease(localT));

      ctx.save();
      ctx.translate(cx, cy);
      ctx.rotate(ang);

      var g = ctx.createLinearGradient(0, -ih / 2, 0, ih / 2);
      g.addColorStop(0, '#28394a');
      g.addColorStop(.52, '#4d6274');
      g.addColorStop(.55, '#6a5340');
      g.addColorStop(1, '#2b2119');
      ctx.fillStyle = g;
      ctx.fillRect(-iw / 2, -ih / 2, iw, ih);
      ctx.strokeStyle = 'rgba(255,255,255,.12)';
      ctx.lineWidth = 1;
      ctx.strokeRect(-iw / 2, -ih / 2, iw, ih);
      ctx.restore();

      // crop frame, level, with thirds
      var fw = iw * 0.78, fh = ih * 0.78;
      ctx.save();
      ctx.translate(cx, cy);
      ctx.strokeStyle = 'rgba(255,255,255,.85)';
      ctx.lineWidth = 1.4;
      ctx.strokeRect(-fw / 2, -fh / 2, fw, fh);

      ctx.strokeStyle = 'rgba(255,255,255,.24)';
      ctx.lineWidth = 1;
      for (var i = 1; i < 3; i++) {
        ctx.beginPath();
        ctx.moveTo(-fw / 2 + fw * i / 3, -fh / 2); ctx.lineTo(-fw / 2 + fw * i / 3, fh / 2); ctx.stroke();
        ctx.beginPath();
        ctx.moveTo(-fw / 2, -fh / 2 + fh * i / 3); ctx.lineTo(fw / 2, -fh / 2 + fh * i / 3); ctx.stroke();
      }
      var hs = [[-1,-1],[0,-1],[1,-1],[-1,0],[1,0],[-1,1],[0,1],[1,1]];
      hs.forEach(function (p) {
        var x = p[0] * fw / 2, y = p[1] * fh / 2;
        ctx.fillStyle = '#ffffff';
        ctx.shadowColor = 'rgba(0,0,0,.7)'; ctx.shadowBlur = 5;
        ctx.fillRect(x - 3, y - 3, 6, 6);
        ctx.shadowBlur = 0;
      });
      ctx.restore();

      ctx.textAlign = 'center';
      ctx.font = '400 10px "JetBrains Mono", monospace';
      ctx.fillStyle = INK_3;
      ctx.fillText((ang * 180 / Math.PI).toFixed(1) + '°  ·  3:2', cx, h - 14);

      if (localT >= 1) s.once = true;
    }, { once: false });
  })();

  /* =======================================================================
     6. HISTOGRAM WITH CLIPPING
     A fixed, legible distribution, drawn once. A histogram that drifts on
     its own is not measuring anything, it is just moving.
     ======================================================================= */
  (function histScene() {
    var canvas = document.getElementById('histDemo');
    if (!canvas) return;

    function lobe(x, c, s, a) { return a * Math.exp(-Math.pow((x - c) / s, 2)); }

    register(canvas, function (ctx, w, h) {
      var pad = 20;
      var gw = w - pad * 2, gh = h - pad * 2 - 12;

      ctx.fillStyle = '#07090b';
      roundRect(ctx, pad, pad, gw, gh, 5); ctx.fill();
      ctx.strokeStyle = LINE; ctx.lineWidth = 1; ctx.stroke();

      var chans = [
        { c: '#d05a5a', off: -0.04 },
        { c: '#5fbf7a', off: 0.0 },
        { c: '#5b8ed0', off: 0.05 }
      ];

      ctx.save();
      roundRect(ctx, pad, pad, gw, gh, 5); ctx.clip();
      ctx.globalCompositeOperation = 'lighter';
      chans.forEach(function (ch) {
        ctx.beginPath();
        ctx.moveTo(pad, pad + gh);
        for (var i = 0; i <= 128; i++) {
          var x = i / 128;
          var v = lobe(x, 0.24 + ch.off, 0.13, 0.55) +
                  lobe(x, 0.55 + ch.off, 0.17, 0.85) +
                  lobe(x, 0.86 + ch.off, 0.07, 0.30);
          ctx.lineTo(pad + x * gw, pad + gh - Math.min(1, v) * gh * 0.92);
        }
        ctx.lineTo(pad + gw, pad + gh);
        ctx.closePath();
        ctx.fillStyle = ch.c;
        ctx.globalAlpha = 0.45;
        ctx.fill();
      });
      ctx.restore();
      ctx.globalCompositeOperation = 'source-over';
      ctx.globalAlpha = 1;

      // clipping indicators, fixed and legible rather than pulsing
      ctx.fillStyle = 'rgba(91,142,208,.55)';
      ctx.beginPath(); ctx.moveTo(pad + 2, pad + 2); ctx.lineTo(pad + 12, pad + 2); ctx.lineTo(pad + 2, pad + 12); ctx.fill();
      ctx.fillStyle = 'rgba(208,90,90,.7)';
      ctx.beginPath(); ctx.moveTo(pad + gw - 2, pad + 2); ctx.lineTo(pad + gw - 12, pad + 2); ctx.lineTo(pad + gw - 2, pad + 12); ctx.fill();

      ctx.textAlign = 'left';
      ctx.font = '400 9.5px "JetBrains Mono", monospace';
      ctx.fillStyle = INK_4;
      ctx.fillText('SHADOW CLIP', pad, h - 6);
      ctx.textAlign = 'right';
      ctx.fillText('HIGHLIGHT CLIP', pad + gw, h - 6);
    }, { once: true });
  })();

  /* =======================================================================
     7. EXPORT
     Previously a progress bar that ran 0 to 100 percent forever, on a
     layout that did not fit the card and collided with itself. Export is a
     discrete action with a finished state, not an ambient process, so this
     is now a single static settings summary with the bar shown complete.
     Row spacing is computed from the card's real height rather than a
     fixed pixel guess, which is what caused the collision.
     ======================================================================= */
  (function exportScene() {
    var canvas = document.getElementById('exportDemo');
    if (!canvas) return;
    var ROWS = [
      ['FORMAT', 'JPEG  q92'],
      ['COLOUR', 'sRGB'],
      ['LONG EDGE', '4096 px']
    ];

    register(canvas, function (ctx, w, h) {
      var pad = Math.max(14, Math.min(22, w * 0.06));
      var footH = Math.max(34, h * 0.26);
      var listTop = pad, listBottom = h - footH;
      var listH = Math.max(24, listBottom - listTop);
      var rowH = listH / ROWS.length;
      var labelX = pad, valueX = pad + Math.min(84, w * 0.34);

      ctx.textAlign = 'left';
      ROWS.forEach(function (row, i) {
        var cy = listTop + rowH * i + rowH * 0.64;
        ctx.font = '400 9.5px "JetBrains Mono", monospace';
        ctx.fillStyle = INK_4;
        ctx.fillText(row[0], labelX, cy);
        ctx.font = '500 13px "Inter Tight", Inter, system-ui, sans-serif';
        ctx.fillStyle = INK;
        ctx.fillText(row[1], valueX, cy);
      });

      // footer: a completed export, not a ticking one
      var barH = 4;
      var barY = h - pad - barH;
      var capY = barY - 8;
      var bx = pad, bw = w - pad * 2;

      ctx.font = '400 9.5px "JetBrains Mono", monospace';
      ctx.textAlign = 'left';
      ctx.fillStyle = INK_4;
      ctx.fillText('WRITTEN', bx, capY);
      ctx.textAlign = 'right';
      ctx.fillStyle = '#6fbf8a';
      ctx.fillText('DONE', bx + bw, capY);

      ctx.fillStyle = 'rgba(255,255,255,.08)';
      roundRect(ctx, bx, barY, bw, barH, barH / 2); ctx.fill();
      var g = ctx.createLinearGradient(bx, 0, bx + bw, 0);
      g.addColorStop(0, ACCENT); g.addColorStop(1, '#6fbf8a');
      ctx.fillStyle = g;
      roundRect(ctx, bx, barY, bw, barH, barH / 2); ctx.fill();
    }, { once: true });
  })();

  window.LightboxScenes = { register: register, start: start, roundRect: roundRect, ease: ease,
                            colours: { INK: INK, INK_3: INK_3, INK_4: INK_4, ACCENT: ACCENT,
                                       ACCENT_HI: ACCENT_HI, WARM: WARM, MAG: MAG, COOL: COOL } };
})();
