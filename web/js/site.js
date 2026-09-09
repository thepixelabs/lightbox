/* =========================================================================
   site.js: navigation, scroll choreography, the hero develop sequence,
   counters, the preset picker, the crash-drill terminal and copy buttons.
   ========================================================================= */
(function () {
  'use strict';

  var REDUCED = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  var clamp = function (v, a, b) { return v < a ? a : v > b ? b : v; };
  var lerp = function (a, b, t) { return a + (b - a) * t; };
  var smooth = function (t) { return t * t * (3 - 2 * t); };

  /* ---------------------------------------------------------------------
     Sticky navigation
     --------------------------------------------------------------------- */
  (function nav() {
    var el = document.getElementById('nav');
    if (!el) return;
    var on = false;
    function update() {
      var should = window.scrollY > 24;
      if (should !== on) { on = should; el.classList.toggle('is-stuck', should); }
    }
    update();
    addEventListener('scroll', update, { passive: true });
  })();

  /* ---------------------------------------------------------------------
     Reveal on entry, with a stagger inside each group
     --------------------------------------------------------------------- */
  (function reveals() {
    var items = [].slice.call(document.querySelectorAll('.reveal'));
    if (!items.length) return;
    if (REDUCED || !('IntersectionObserver' in window)) {
      items.forEach(function (n) { n.classList.add('is-in'); });
      return;
    }
    var groups = new Map();
    items.forEach(function (n) {
      var p = n.parentElement;
      if (!groups.has(p)) groups.set(p, []);
      groups.get(p).push(n);
    });
    groups.forEach(function (list) {
      list.forEach(function (n, i) { n.style.setProperty('--d', Math.min(i, 6) * 70 + 'ms'); });
    });

    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) {
        if (!e.isIntersecting) return;
        e.target.classList.add('is-in');
        io.unobserve(e.target);
      });
    }, { rootMargin: '0px 0px -12% 0px', threshold: 0.06 });

    // Anything already scrolled past on load (a deep link, or the browser
    // restoring a position) never intersects, so it would stay invisible
    // forever. Show those immediately and only observe what is still below.
    items.forEach(function (n) {
      if (n.getBoundingClientRect().bottom < 0) n.classList.add('is-in');
      else io.observe(n);
    });
  })();

  /* ---------------------------------------------------------------------
     Counters in the statistics strip
     --------------------------------------------------------------------- */
  (function counters() {
    var nodes = [].slice.call(document.querySelectorAll('[data-count]'));
    if (!nodes.length) return;
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) {
        if (!e.isIntersecting) return;
        var el = e.target;
        io.unobserve(el);
        var target = parseFloat(el.getAttribute('data-count'));
        var suffix = el.getAttribute('data-suffix') || '';
        if (REDUCED) { el.textContent = target + suffix; return; }
        var t0 = performance.now(), dur = 1150;
        (function tick(now) {
          var p = clamp((now - t0) / dur, 0, 1);
          var eased = 1 - Math.pow(1 - p, 3);
          el.textContent = Math.round(target * eased) + (p === 1 ? suffix : '');
          if (p < 1) requestAnimationFrame(tick);
        })(t0);
      });
    }, { threshold: 0.5 });
    nodes.forEach(function (n) { io.observe(n); });
  })();

  /* ---------------------------------------------------------------------
     HERO: a develop session driven by scroll position.

     Each parameter has a window of the scroll range in which it moves, so
     the panel reads like somebody working through an image in order rather
     than every slider sliding at once.
     --------------------------------------------------------------------- */
  (function hero() {
    var track  = document.getElementById('heroTrack');
    var canvas = document.getElementById('heroCanvas');
    var panel  = document.getElementById('heroPanel');
    var hostEl = document.getElementById('heroSliders');
    var histEl = document.getElementById('heroHist');
    var cue    = document.getElementById('scrollCue');
    if (!track || !canvas || !window.LightboxDevelop) return;

    // from (as decoded, flat) -> to (developed), each over a scroll window
    var STAGES = [
      { key: 'temp',      label: 'Temp',       from: -0.16, to:  0.13, a: 0.02, b: 0.24, fmt: kelvin,  unit: '' },
      { key: 'tint',      label: 'Tint',       from:  0.00, to: -0.05, a: 0.04, b: 0.24, fmt: signed1, unit: '' },
      { key: 'exposure',  label: 'Exposure',   from: -0.30, to:  0.30, a: 0.18, b: 0.44, fmt: ev,      unit: 'EV' },
      { key: 'contrast',  label: 'Contrast',   from: -0.52, to:  0.30, a: 0.20, b: 0.48, fmt: pct,     unit: '' },
      { key: 'high',      label: 'Highlights', from:  0.00, to: -0.38, a: 0.40, b: 0.66, fmt: pct,     unit: '' },
      { key: 'shadow',    label: 'Shadows',    from:  0.00, to:  0.34, a: 0.42, b: 0.68, fmt: pct,     unit: '' },
      { key: 'vibrance',  label: 'Vibrance',   from: -0.10, to:  0.36, a: 0.58, b: 0.84, fmt: pct,     unit: '' }
    ];
    // parameters that move but are not shown as rows
    var HIDDEN = [
      { key: 'sat',      from: -0.34, to:  0.04, a: 0.58, b: 0.86 },
      { key: 'white',    from: -0.10, to:  0.12, a: 0.44, b: 0.70 },
      { key: 'black',    from:  0.10, to: -0.08, a: 0.44, b: 0.70 },
      { key: 'fade',     from:  0.42, to:  0.00, a: 0.10, b: 0.52 },
      { key: 'vignette', from:  0.00, to:  0.26, a: 0.72, b: 0.96 },
      { key: 'grain',    from:  0.00, to:  0.13, a: 0.76, b: 1.00 }
    ];

    function kelvin(v) { return Math.round(5000 + v * 3400) + ' K'; }
    function ev(v)     { return (v >= 0 ? '+' : '') + v.toFixed(2); }
    function pct(v)    { return (v >= 0 ? '+' : '') + Math.round(v * 100); }
    function signed1(v){ return (v >= 0 ? '+' : '') + Math.round(v * 100); }

    // build the panel rows once
    var rows = STAGES.map(function (s) {
      var row = document.createElement('div');
      row.className = 'prow';
      row.innerHTML =
        '<div class="prow-top"><span>' + s.label + '</span>' +
        '<span class="prow-val">0</span></div>' +
        '<div class="ptrack"><i class="pfill"></i><i class="pknob"></i></div>';
      hostEl.appendChild(row);
      return {
        spec: s,
        val: row.querySelector('.prow-val'),
        fill: row.querySelector('.pfill'),
        knob: row.querySelector('.pknob')
      };
    });

    var dev = new window.LightboxDevelop.Developer(canvas, 'assets/img/hero-base.webp', { fit: 'cover' });
    if (!dev.gl) return;
    dev.onReady(function () { canvas.classList.add('is-live'); });

    var hctx = histEl ? histEl.getContext('2d') : null;
    var histAcc = null;
    var lastHist = 0;

    function drawHistogram(now) {
      if (!hctx || now - lastHist < 90) return;
      lastHist = now;
      var h = dev.histogram(96);
      if (!h) return;
      if (!histAcc) histAcc = { r: h.r.slice(), g: h.g.slice(), b: h.b.slice() };
      // ease toward the new distribution so it moves like a real readout
      ['r', 'g', 'b'].forEach(function (c) {
        for (var i = 0; i < h[c].length; i++) histAcc[c][i] = lerp(histAcc[c][i], h[c][i], 0.35);
      });

      var W = histEl.width, H = histEl.height;
      hctx.clearRect(0, 0, W, H);
      var peak = 1;
      ['r', 'g', 'b'].forEach(function (c) {
        for (var i = 0; i < histAcc[c].length; i++) peak = Math.max(peak, histAcc[c][i]);
      });
      hctx.globalCompositeOperation = 'lighter';
      [['r', '#d05a5a'], ['g', '#5fbf7a'], ['b', '#5b8ed0']].forEach(function (pair) {
        var data = histAcc[pair[0]];
        hctx.beginPath();
        hctx.moveTo(0, H);
        for (var i = 0; i < data.length; i++) {
          var x = i / (data.length - 1) * W;
          var y = H - Math.pow(data[i] / peak, 0.62) * H * 0.94;
          hctx.lineTo(x, y);
        }
        hctx.lineTo(W, H);
        hctx.closePath();
        hctx.fillStyle = pair[1];
        hctx.globalAlpha = 0.5;
        hctx.fill();
      });
      hctx.globalCompositeOperation = 'source-over';
      hctx.globalAlpha = 1;
    }

    function progress() {
      var r = track.getBoundingClientRect();
      var total = track.offsetHeight - window.innerHeight;
      if (total <= 0) return 1;
      return clamp(-r.top / total, 0, 1);
    }

    function stageValue(s, p) {
      var t = clamp((p - s.a) / (s.b - s.a), 0, 1);
      return lerp(s.from, s.to, smooth(t));
    }

    var pending = false, lastP = -1;

    // The histogram eases toward each new distribution (see drawHistogram's
    // lerp) rather than snapping, so it reads like a live readout. That ease
    // needs a few extra frames to actually finish converging after the last
    // scroll tick. This chase is bounded: it runs a fixed number of frames
    // after each apply() and then stops on its own, rather than polling
    // forever while the hero is merely in view.
    var settleFrames = 0;
    function settleStep(now) {
      drawHistogram(now);
      if (settleFrames > 0) { settleFrames--; requestAnimationFrame(settleStep); }
    }
    function settleHistogram() {
      if (settleFrames <= 0) requestAnimationFrame(settleStep);
      settleFrames = 20; // ~330ms at 60fps: enough for the 0.35 lerp to converge
    }

    function apply() {
      pending = false;
      var p = REDUCED ? 1 : progress();

      if (Math.abs(p - lastP) > 0.0004 || lastP < 0) {
        lastP = p;
        var patch = {};
        STAGES.forEach(function (s) { patch[s.key] = stageValue(s, p); });
        HIDDEN.forEach(function (s) { patch[s.key] = stageValue(s, p); });
        // split tone arrives late: cool shadows, warm highlights
        var st = clamp((p - 0.66) / 0.3, 0, 1);
        patch.shadowTint = [-0.05 * st, 0.0, 0.09 * st];
        patch.highTint   = [0.09 * st, 0.03 * st, -0.05 * st];
        dev.set(patch);
        dev.render();

        rows.forEach(function (r) {
          var v = patch[r.spec.key];
          r.val.textContent = r.spec.fmt(v) + (r.spec.unit ? ' ' + r.spec.unit : '');
          var norm = clamp((v - r.spec.from) / ((r.spec.to - r.spec.from) || 1), 0, 1);
          // draw bipolar params from the centre, the way the app does
          var pos = clamp(0.5 + v * 0.9, 0.04, 0.96);
          var mid = 0.5;
          r.fill.style.left = Math.min(mid, pos) * 100 + '%';
          r.fill.style.width = Math.abs(pos - mid) * 100 + '%';
          r.knob.style.left = pos * 100 + '%';
          void norm;
        });

        if (panel) panel.classList.toggle('is-in', p > 0.04);
        if (cue) cue.style.opacity = String(clamp(1 - p * 7, 0, 1));
      }
      settleHistogram();
    }

    function onScroll() {
      if (pending) return;
      pending = true;
      requestAnimationFrame(apply);
    }

    dev.onReady(function () { apply(performance.now()); });
    addEventListener('scroll', onScroll, { passive: true });
    addEventListener('resize', function () { dev.resize(); lastP = -1; onScroll(); }, { passive: true });
  })();

  /* ---------------------------------------------------------------------
     ENTRY: fire the drop animation once, when the section arrives
     --------------------------------------------------------------------- */
  (function entry() {
    var demo = document.querySelector('.entry-demo');
    if (!demo) return;
    if (REDUCED) { demo.classList.add('is-dropped'); return; }
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) {
        if (!e.isIntersecting) return;
        io.disconnect();
        demo.classList.add('is-armed');
        setTimeout(function () { demo.classList.add('is-dropped'); }, 900);
      });
    }, { threshold: 0.35 });
    io.observe(demo);
  })();

  /* ---------------------------------------------------------------------
     LOOKS: a real develop rail, not an image swap.

     A single WebGL canvas is graded live from ten sliders, using the same
     LightboxDevelop.Developer class as the hero. A preset chip is a set of
     slider values applied at once, not a picture; after applying one you
     can keep dragging, which is the entire point of the section's own
     copy. A wipe handle compares the live render against the flat file.

     If WebGL is unavailable, the sliders would do nothing, so the panel
     degrades to the old behaviour: chips swap in the pre-rendered frame
     for that look, and a note says why.
     --------------------------------------------------------------------- */
  (function developPanel() {
    var stage    = document.getElementById('ddStage');
    var canvas   = document.getElementById('ddCanvas');
    var beforeImg= document.getElementById('ddBeforeImg');
    var wipeIn   = document.getElementById('ddWipeInput');
    var wipeLine = document.getElementById('ddWipeLine');
    var wipeGrip = document.getElementById('ddWipeGrip');
    var badgeName= document.getElementById('ddBadgeName');
    var badgeGrp = document.getElementById('ddBadgeGroup');
    var noGl     = document.getElementById('ddNoGl');
    var resetBtn = document.getElementById('ddReset');
    var rail     = document.getElementById('ddRail');
    var groupEls = { light: document.getElementById('ddGroupLight'), colour: document.getElementById('ddGroupColour') };
    var chips    = [].slice.call(document.querySelectorAll('#ddPresets .look-chip'));
    if (!stage || !canvas || !window.LightboxDevelop || !chips.length) return;

    function pct(v)     { return (v >= 0 ? '+' : '') + Math.round(v * 100); }
    function ev(v)      { return (v >= 0 ? '+' : '') + v.toFixed(2) + ' EV'; }
    function signed1(v) { return (v >= 0 ? '+' : '') + Math.round(v * 100); }
    function kelvin(v)  { return Math.round(5500 + v * (2500 / 0.3)) + ' K'; }

    var PARAMS = [
      { key: 'exposure',   shaderKey: 'exposure', label: 'Exposure',    group: 'light',  min: -1.2,  max: 1.2,  step: 0.01,  fmt: ev },
      { key: 'contrast',   shaderKey: 'contrast', label: 'Contrast',    group: 'light',  min: -0.5,  max: 0.5,  step: 0.005, fmt: pct },
      { key: 'highlights', shaderKey: 'high',     label: 'Highlights',  group: 'light',  min: -1,    max: 1,    step: 0.01,  fmt: pct },
      { key: 'shadows',    shaderKey: 'shadow',   label: 'Shadows',     group: 'light',  min: -1,    max: 1,    step: 0.01,  fmt: pct },
      { key: 'whites',     shaderKey: 'white',    label: 'Whites',      group: 'light',  min: -1,    max: 1,    step: 0.01,  fmt: pct },
      { key: 'blacks',     shaderKey: 'black',    label: 'Blacks',      group: 'light',  min: -1,    max: 1,    step: 0.01,  fmt: pct },
      { key: 'temp',       shaderKey: 'temp',     label: 'Temperature', group: 'colour', min: -0.3,  max: 0.3,  step: 0.005, fmt: kelvin },
      { key: 'tint',       shaderKey: 'tint',     label: 'Tint',        group: 'colour', min: -0.15, max: 0.15, step: 0.002, fmt: signed1 },
      { key: 'vibrance',   shaderKey: 'vibrance', label: 'Vibrance',    group: 'colour', min: -1,    max: 1,    step: 0.01,  fmt: pct },
      { key: 'saturation', shaderKey: 'sat',      label: 'Saturation',  group: 'colour', min: -1,    max: 1,    step: 0.01,  fmt: pct }
    ];

    // Nine recipes: real slider values, the same way the application's own
    // presets are real parameter sets rather than baked images. Any key
    // missing from `v` is treated as zero.
    var LOOKS = {
      'original':        { name: 'As shot',        group: 'No grade applied', v: {} },
      'glasshouse':      { name: 'Glasshouse',      group: 'Tonal',      v: { temp: -0.05, exposure: 0.15, contrast: -0.12, highlights: -0.25, shadows: 0.20, whites: 0.10, blacks: 0.05, vibrance: 0.05, saturation: -0.05 } },
      'super8':          { name: 'Super 8',         group: 'Retro',      v: { temp: 0.12, tint: -0.03, exposure: 0.05, contrast: -0.18, highlights: -0.10, shadows: 0.28, whites: -0.05, blacks: 0.12, vibrance: 0.10, saturation: -0.15 } },
      'sunday-chrome':   { name: 'Sunday Chrome',   group: 'Retro',      v: { temp: 0.08, tint: 0.01, exposure: 0.05, contrast: 0.22, highlights: -0.15, shadows: 0.05, whites: 0.08, blacks: -0.05, vibrance: 0.25, saturation: 0.15 } },
      'noir-grain':      { name: 'Noir Grain',      group: 'Mono',       v: { exposure: -0.05, contrast: 0.35, highlights: -0.30, shadows: -0.10, whites: 0.05, blacks: -0.15, saturation: -1 } },
      'neon-rain':       { name: 'Neon Rain',       group: 'Neon',       v: { temp: -0.15, tint: 0.06, exposure: -0.08, contrast: 0.18, highlights: -0.20, shadows: -0.15, whites: -0.05, blacks: -0.08, vibrance: 0.35, saturation: 0.30 } },
      'moss-slate':      { name: 'Moss and Slate',  group: 'Verdant',    v: { temp: -0.06, tint: 0.03, contrast: 0.08, highlights: -0.10, shadows: 0.10, blacks: 0.02, vibrance: -0.10, saturation: -0.15 } },
      'concrete-brutal': { name: 'Concrete Brutal', group: 'Edge',       v: { temp: -0.03, exposure: -0.05, contrast: 0.32, highlights: -0.05, shadows: -0.20, whites: 0.05, blacks: -0.10, vibrance: -0.20, saturation: -0.35 } },
      'fog-bank':        { name: 'Fog Bank',        group: 'Atmosphere', v: { temp: -0.04, exposure: 0.10, contrast: -0.30, highlights: -0.05, shadows: 0.30, whites: -0.10, blacks: 0.15, vibrance: -0.15, saturation: -0.20 } }
    };

    // ---- build the rows ---------------------------------------------------
    var rows = PARAMS.map(function (spec) {
      var id = 'dd-' + spec.key;
      var row = document.createElement('div');
      row.className = 'drow';
      row.innerHTML =
        '<div class="drow-top"><label for="' + id + '">' + spec.label + '</label>' +
        '<output class="drow-val" id="' + id + '-val" for="' + id + '">0</output></div>' +
        '<div class="dtrack-wrap">' +
          '<div class="dtrack" aria-hidden="true"><i class="dfill"></i><i class="dknob"></i></div>' +
          '<input type="range" class="dslider" id="' + id + '" ' +
            'min="' + spec.min + '" max="' + spec.max + '" step="' + spec.step + '" value="0">' +
        '</div>';
      (groupEls[spec.group] || rail).appendChild(row);
      return {
        spec: spec,
        input: row.querySelector('.dslider'),
        val: row.querySelector('.drow-val'),
        fill: row.querySelector('.dfill'),
        knob: row.querySelector('.dknob')
      };
    });
    function paintRow(r, value) {
      var span = r.spec.max - r.spec.min;
      var pos = span ? clamp((value - r.spec.min) / span, 0.02, 0.98) : 0.5;
      var mid = 0.5;
      r.fill.style.left = Math.min(mid, pos) * 100 + '%';
      r.fill.style.width = Math.abs(pos - mid) * 100 + '%';
      r.knob.style.left = pos * 100 + '%';
      var text = r.spec.fmt(value);
      r.val.textContent = text;
      r.input.setAttribute('aria-valuetext', text);
    }

    function setRowValue(r, value, silent) {
      value = clamp(value, r.spec.min, r.spec.max);
      r.input.value = String(value);
      paintRow(r, value);
      if (!silent) markCustom();
    }

    // ---- WebGL develop surface ---------------------------------------------
    // Built lazily, the first time the section nears the viewport, for the
    // same reason the old preset picker warmed its images lazily: nobody
    // scrolling the hero should pay for a second decoded photograph and a
    // second GL context before they have asked for either.
    var dev = null, live = false;

    var renderPending = false;
    function scheduleRender() {
      if (!live || renderPending) return;
      renderPending = true;
      requestAnimationFrame(function () { renderPending = false; dev.render(); });
    }

    function applyShader(patch) {
      if (!live) return;
      dev.set(patch);
      scheduleRender();
    }

    function currentShaderPatch() {
      var patch = {};
      rows.forEach(function (r) { patch[r.spec.shaderKey] = parseFloat(r.input.value); });
      return patch;
    }

    // ---- preset / custom state ---------------------------------------------
    function markCustom() {
      chips.forEach(function (c) { c.classList.remove('is-on'); c.setAttribute('aria-selected', 'false'); });
      badgeName.textContent = 'Custom';
      badgeGrp.textContent = 'edited by hand';
    }

    function selectChip(chip) {
      chips.forEach(function (c) {
        var on = c === chip;
        c.classList.toggle('is-on', on);
        c.setAttribute('aria-selected', on ? 'true' : 'false');
      });
      badgeName.textContent = chip.getAttribute('data-name');
      badgeGrp.textContent = chip.getAttribute('data-group');
    }

    var animId = 0;
    function animateTo(targets, chip) {
      var from = {};
      rows.forEach(function (r) { from[r.spec.key] = parseFloat(r.input.value); });
      var myId = ++animId;
      var dur = REDUCED ? 0 : 0.38;
      var t0 = performance.now();
      (function step(now) {
        if (myId !== animId) return; // superseded by a newer click or a manual drag
        var t = dur ? clamp((now - t0) / 1000 / dur, 0, 1) : 1;
        var e = smooth(t);
        rows.forEach(function (r) {
          var target = targets[r.spec.key] || 0;
          setRowValue(r, lerp(from[r.spec.key], target, e), true);
        });
        applyShader(currentShaderPatch());
        if (t < 1) requestAnimationFrame(step);
        else if (chip) selectChip(chip);
      })(t0);
    }

    function applyLook(slug, chip) {
      var look = LOOKS[slug];
      if (!look) return;
      if (live) {
        animateTo(look.v, chip);
      } else if (beforeImg) {
        beforeImg.src = 'assets/img/presets/' + slug + '.webp';
        selectChip(chip);
      }
    }

    // ---- wire the sliders ---------------------------------------------------
    rows.forEach(function (r) {
      paintRow(r, 0);
      r.input.addEventListener('input', function () {
        animId++; // a manual drag cancels any in-flight preset tween
        paintRow(r, parseFloat(r.input.value));
        applyShader(currentShaderPatch());
        markCustom();
      });
    });

    // ---- reset ----------------------------------------------------------
    if (resetBtn) {
      resetBtn.addEventListener('click', function () {
        applyLook('original', chips[0]);
      });
    }

    // ---- presets ----------------------------------------------------------
    chips.forEach(function (chip) {
      chip.addEventListener('click', function () {
        applyLook(chip.getAttribute('data-look'), chip);
      });
    });

    // ---- before / after wipe ------------------------------------------------
    if (wipeIn) {
      var updateWipe = function () {
        if (!live) return;
        var v = clamp(parseFloat(wipeIn.value), 0, 100);
        beforeImg.style.clipPath = 'inset(0 ' + (100 - v) + '% 0 0)';
        wipeLine.style.left = v + '%';
        wipeGrip.style.left = v + '%';
        wipeIn.setAttribute('aria-valuetext', Math.round(v) + ' percent before');
      };
      wipeIn.addEventListener('input', updateWipe);
    }

    // ---- init: build the GL surface once the section is nearly in view ----
    function init() {
      dev = new window.LightboxDevelop.Developer(canvas, 'assets/img/presets/original.webp', { fit: 'cover' });
      live = !!dev.gl;

      if (live) {
        dev.onReady(function () {
          canvas.classList.add('is-live');
          applyShader(currentShaderPatch());
        });
        addEventListener('resize', function () { dev.resize(); scheduleRender(); }, { passive: true });
        if (wipeIn) { wipeIn.value = '50'; updateWipe(); }
      } else {
        stage.classList.add('no-webgl');
        if (rail) rail.hidden = true;
        if (noGl) noGl.hidden = false;
        if (wipeIn) wipeIn.hidden = true;
        if (wipeLine) wipeLine.hidden = true;
        if (wipeGrip) wipeGrip.hidden = true;
        if (beforeImg) { beforeImg.style.clipPath = 'none'; beforeImg.src = 'assets/img/presets/original.webp'; }
      }
    }

    new IntersectionObserver(function (entries, obs) {
      entries.forEach(function (e) {
        if (!e.isIntersecting) return;
        obs.disconnect();
        init();
      });
    }, { rootMargin: '500px' }).observe(stage);
  })();


  /* ---------------------------------------------------------------------
     COPY BUTTONS
     --------------------------------------------------------------------- */
  (function copy() {
    document.querySelectorAll('.copy').forEach(function (btn) {
      btn.addEventListener('click', function () {
        var src = document.getElementById(btn.getAttribute('data-copy'));
        if (!src) return;
        var text = src.innerText.replace(/\n+$/, '');
        var done = function () {
          var was = btn.textContent;
          btn.textContent = 'Copied';
          btn.classList.add('is-done');
          setTimeout(function () { btn.textContent = was; btn.classList.remove('is-done'); }, 1600);
        };
        if (navigator.clipboard) {
          navigator.clipboard.writeText(text).then(done, function () {});
        } else {
          var ta = document.createElement('textarea');
          ta.value = text;
          document.body.appendChild(ta);
          ta.select();
          try { document.execCommand('copy'); done(); } catch (e) {}
          document.body.removeChild(ta);
        }
      });
    });
  })();

})();
