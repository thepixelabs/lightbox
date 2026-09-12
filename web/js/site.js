/* =========================================================================
   site.js: navigation, scroll choreography, the hero develop sequence,
   the preset picker, the develop panel and copy buttons.
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
     COPY BUTTONS
     --------------------------------------------------------------------- */
  (function copy() {
    var n = 0;
    document.querySelectorAll('.copy').forEach(function (btn) {
      n += 1;
      btn.type = 'button';

      // Five of these on the page, and every one of them is named "Copy" and
      // nothing else, so a screen reader reads the same name five times with
      // no way to tell which block it would copy. Each code card already
      // carries its own label in the head beside the button, so the button
      // borrows it: the accessible name becomes "Copy, From source" without
      // a single new string being written.
      var head = btn.parentElement;
      var title = head && head.querySelector('.mono');
      if (title) {
        if (!btn.id) btn.id = 'copy-btn-' + n;
        if (!title.id) title.id = 'copy-for-' + n;
        btn.setAttribute('aria-labelledby', btn.id + ' ' + title.id);
      }
      // The label swaps to "Copied" on success. That is the only feedback the
      // button gives, so it has to be announced rather than just drawn.
      btn.setAttribute('aria-live', 'polite');

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
