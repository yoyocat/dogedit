(function () {
  "use strict";

  var root = document.documentElement;
  var STORE_KEY = "dogedit-page-theme";

  function readStored() {
    try { return localStorage.getItem(STORE_KEY); } catch (e) { return null; }
  }
  function store(value) {
    try { localStorage.setItem(STORE_KEY, value); } catch (e) { /* private mode */ }
  }

  function preferred() {
    var stored = readStored();
    if (stored === "dark" || stored === "light") return stored;
    return window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches
      ? "light"
      : "dark";
  }

  function applyPageTheme(theme) {
    root.setAttribute("data-theme", theme);
    var meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.setAttribute("content", theme === "light" ? "#F6F9FC" : "#0A0E14");
    var toggle = document.getElementById("theme-toggle");
    if (toggle) toggle.setAttribute("aria-label", theme === "light" ? "切换到深色外观" : "切换到浅色外观");
  }

  function hexToRgb(hex) {
    var h = String(hex || "").replace("#", "").trim();
    if (h.length === 3) h = h[0] + h[0] + h[1] + h[1] + h[2] + h[2];
    var n = parseInt(h, 16);
    if (isNaN(n) || h.length !== 6) return { r: 0, g: 0, b: 0 };
    return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
  }

  function toHex(rgb) {
    function p(v) { var s = Math.max(0, Math.min(255, Math.round(v))).toString(16); return s.length < 2 ? "0" + s : s; }
    return "#" + p(rgb.r) + p(rgb.g) + p(rgb.b);
  }

  function mix(a, b, t) {
    var x = hexToRgb(a), y = hexToRgb(b);
    return toHex({ r: x.r + (y.r - x.r) * t, g: x.g + (y.g - x.g) * t, b: x.b + (y.b - x.b) * t });
  }

  function luminance(hex) {
    var c = hexToRgb(hex);
    function lin(v) { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); }
    return 0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b);
  }

  function themeByName(name) {
    var list = window.DG_THEMES || [];
    for (var i = 0; i < list.length; i++) if (list[i].name === name) return list[i];
    return list[0] || null;
  }

  function paintWindow(el, theme) {
    if (!el || !theme) return;
    var vars = {
      "--t-bg": theme.bg,
      "--t-fg": theme.fg,
      "--t-comment": theme.comment,
      "--t-string": theme.string,
      "--t-number": theme.number,
      "--t-func": theme.function,
      "--t-key": theme.word,
      "--t-type": theme.function,
      "--t-no": theme.no,
      "--t-caret": theme.caret,
      "--t-sel": theme.sel,
      "--t-line": theme.line,
      "--t-tab": mix(theme.bg, theme.fg, 0.07),
      "--t-status": mix(theme.bg, theme.fg, 0.1),
      "--t-border": mix(theme.bg, theme.fg, 0.16),
      "--t-muted": mix(theme.bg, theme.fg, 0.62)
    };
    Object.keys(vars).forEach(function (key) {
      el.style.setProperty(key, vars[key]);
    });
    el.setAttribute("data-theme", theme.name);
  }

  /* ---------- page shell ---------- */

  applyPageTheme(preferred());

  var toggle = document.getElementById("theme-toggle");
  if (toggle) {
    toggle.addEventListener("click", function () {
      var next = root.getAttribute("data-theme") === "light" ? "dark" : "light";
      applyPageTheme(next);
      store(next);
    });
  }

  if (window.matchMedia) {
    var query = window.matchMedia("(prefers-color-scheme: light)");
    var onChange = function (e) {
      if (!readStored()) applyPageTheme(e.matches ? "light" : "dark");
    };
    if (query.addEventListener) query.addEventListener("change", onChange);
    else if (query.addListener) query.addListener(onChange);
  }

  var head = document.getElementById("site-head");
  function onScroll() {
    if (!head) return;
    head.classList.toggle("is-scrolled", window.scrollY > 6);
  }
  onScroll();
  window.addEventListener("scroll", onScroll, { passive: true });

  var menuBtn = document.getElementById("menu-btn");
  var navLinks = document.getElementById("nav-links");
  function closeMenu() {
    if (!navLinks || !menuBtn) return;
    navLinks.classList.remove("is-open");
    menuBtn.setAttribute("aria-expanded", "false");
  }
  if (menuBtn && navLinks) {
    menuBtn.addEventListener("click", function (e) {
      e.stopPropagation();
      var open = navLinks.classList.toggle("is-open");
      menuBtn.setAttribute("aria-expanded", open ? "true" : "false");
    });
    navLinks.addEventListener("click", function (e) {
      if (e.target.closest("a")) closeMenu();
    });
    document.addEventListener("click", function (e) {
      if (!navLinks.contains(e.target) && !menuBtn.contains(e.target)) closeMenu();
    });
    document.addEventListener("keydown", function (e) {
      if (e.key === "Escape") closeMenu();
    });
  }

  /* ---------- toast ---------- */

  var toast = document.createElement("div");
  toast.className = "toast";
  toast.setAttribute("role", "status");
  toast.setAttribute("aria-live", "polite");
  document.body.appendChild(toast);
  var toastTimer = 0;

  function say(text) {
    toast.textContent = text;
    toast.classList.add("is-on");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(function () { toast.classList.remove("is-on"); }, 1600);
  }

  function fallbackCopy(text) {
    var area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    var ok = false;
    try { ok = document.execCommand("copy"); } catch (e) { ok = false; }
    document.body.removeChild(area);
    return ok;
  }

  function copyText(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(
        function () { say("已复制：" + text); },
        function () { say(fallbackCopy(text) ? "已复制：" + text : "复制失败，请手动选择"); }
      );
    } else {
      say(fallbackCopy(text) ? "已复制：" + text : "复制失败，请手动选择");
    }
  }

  var copyBtn = document.getElementById("copy-qq");
  if (copyBtn) {
    copyBtn.addEventListener("click", function () {
      copyText(copyBtn.getAttribute("data-copy") || "1130640365");
    });
  }

  /* ---------- editor windows ---------- */

  document.querySelectorAll(".code-window[data-theme]").forEach(function (el) {
    var theme = themeByName(el.getAttribute("data-theme"));
    if (theme) paintWindow(el, theme);
  });

  /* ---------- theme switcher ---------- */

  var chipsBox = document.getElementById("theme-chips");
  var windowEl = document.getElementById("theme-window");
  var nameEl = document.getElementById("theme-name");
  var kindEl = document.getElementById("theme-kind");
  var statusEl = document.getElementById("theme-status-name");
  var windowTitle = document.getElementById("theme-window-title");
  var swatchEls = document.querySelectorAll("[data-swatch]");

  function selectTheme(theme) {
    if (!theme) return;
    if (windowEl) paintWindow(windowEl, theme);
    var light = luminance(theme.bg) > 0.5;
    if (nameEl) nameEl.textContent = theme.name;
    if (kindEl) kindEl.textContent = light ? "浅色主题" : "深色主题";
    if (statusEl) statusEl.textContent = theme.name;
    if (windowTitle) windowTitle.textContent = "welcome.rs — DogEdit · " + theme.name;
    swatchEls.forEach(function (el) {
      var key = el.getAttribute("data-swatch");
      var value = theme[key] || theme.fg;
      el.textContent = String(value).toUpperCase();
      el.style.background = value;
      el.style.color = luminance(value) > 0.5 ? "#101820" : "#F2F6FA";
    });
    if (chipsBox) {
      chipsBox.querySelectorAll(".chip").forEach(function (chip) {
        chip.setAttribute("aria-pressed", chip.getAttribute("data-theme") === theme.name ? "true" : "false");
      });
    }
  }

  if (chipsBox && window.DG_THEMES) {
    window.DG_THEMES.forEach(function (theme) {
      var chip = document.createElement("button");
      chip.type = "button";
      chip.className = "chip";
      chip.setAttribute("data-theme", theme.name);
      chip.setAttribute("aria-pressed", "false");
      chip.title = theme.name;
      var dot = document.createElement("span");
      dot.className = "swatch";
      dot.style.setProperty("--sw", theme.bg);
      dot.style.setProperty("--sw2", theme.fg);
      var label = document.createElement("span");
      label.textContent = theme.name;
      chip.appendChild(dot);
      chip.appendChild(label);
      chip.addEventListener("click", function () { selectTheme(theme); });
      chipsBox.appendChild(chip);
    });

    var initial = themeByName("DogEdit_Dark") || window.DG_THEMES[0];
    selectTheme(initial);
  }
})();
