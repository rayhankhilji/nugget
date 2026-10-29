// Benchmark dashboard for the Nugget book: draws one small line chart per headline metric from
// history.json (written by scripts/bench-dashboard.py). Plain DOM and SVG, no dependencies.
(function () {
  "use strict";

  var SVG = "http://www.w3.org/2000/svg";
  var W = 340, H = 150, PAD = { top: 12, right: 14, bottom: 26, left: 52 };
  var DIRECTION = {
    higher_is_better: "higher is better",
    lower_is_better: "lower is better",
    info: "for information"
  };

  function el(name, attrs, text) {
    var node = document.createElement(name);
    for (var k in attrs || {}) node.setAttribute(k, attrs[k]);
    if (text != null) node.textContent = text;
    return node;
  }

  function svg(name, attrs, text) {
    var node = document.createElementNS(SVG, name);
    for (var k in attrs || {}) node.setAttribute(k, attrs[k]);
    if (text != null) node.textContent = text;
    return node;
  }

  function fmt(v) {
    if (v === 0) return "0";
    var a = Math.abs(v);
    if (a >= 1e6 || a < 1e-3) return v.toExponential(2);
    if (a >= 100) return String(Math.round(v * 10) / 10);
    return String(Number(v.toPrecision(3)));
  }

  function withUnit(v, unit) {
    return unit ? fmt(v) + " " + unit : fmt(v);
  }

  function day(date) {
    return Date.UTC(+date.slice(0, 4), +date.slice(5, 7) - 1, +date.slice(8, 10));
  }

  function shortDate(date) {
    return date.slice(5); // MM-DD; the full date is in each point's tooltip
  }

  // Up to `n` evenly spaced, rounded tick values covering [lo, hi].
  function ticks(lo, hi, n) {
    var step = Math.pow(10, Math.floor(Math.log10((hi - lo) / n)));
    var err = (hi - lo) / n / step;
    step *= err >= 5 ? 10 : err >= 2 ? 5 : err >= 1.5 ? 2 : 1;
    var out = [];
    for (var t = Math.ceil(lo / step) * step; t <= hi + step * 1e-9; t += step) {
      out.push(Math.abs(t) < step * 1e-9 ? 0 : t);
    }
    return out;
  }

  function chart(series, repo) {
    var pts = series.points;
    var root = svg("svg", {
      viewBox: "0 0 " + W + " " + H,
      class: "bench-chart",
      role: "img",
      "aria-label": series.label + " over time"
    });
    if (pts.length === 0) {
      root.appendChild(svg("text", { x: W / 2, y: H / 2, class: "bench-empty", "text-anchor": "middle" },
        "No results yet"));
      return root;
    }

    var values = pts.map(function (p) { return p.value; });
    if (series.target != null) values.push(series.target);
    var lo = Math.min.apply(null, values), hi = Math.max.apply(null, values);
    if (lo === hi) { var pad = Math.abs(lo) * 0.1 || 1; lo -= pad; hi += pad; }
    var span = hi - lo; lo -= span * 0.08; hi += span * 0.08;
    if (Math.min.apply(null, values) >= 0 && lo < 0) lo = 0;

    var t0 = day(pts[0].date), t1 = day(pts[pts.length - 1].date);
    var iw = W - PAD.left - PAD.right, ih = H - PAD.top - PAD.bottom;
    function x(date) { return t1 === t0 ? PAD.left + iw / 2 : PAD.left + (day(date) - t0) / (t1 - t0) * iw; }
    function y(v) { return PAD.top + (hi - v) / (hi - lo) * ih; }

    // Grid and y labels.
    ticks(lo, hi, 4).forEach(function (t) {
      root.appendChild(svg("line", { x1: PAD.left, x2: W - PAD.right, y1: y(t), y2: y(t), class: "bench-grid" }));
      root.appendChild(svg("text", { x: PAD.left - 6, y: y(t) + 4, "text-anchor": "end", class: "bench-axis" }, fmt(t)));
    });
    // x labels: first and last date (one label when they are the same day).
    root.appendChild(svg("text", { x: x(pts[0].date), y: H - 8, "text-anchor": pts.length > 1 ? "start" : "middle",
      class: "bench-axis" }, shortDate(pts[0].date)));
    if (pts.length > 1) {
      root.appendChild(svg("text", { x: x(pts[pts.length - 1].date), y: H - 8, "text-anchor": "end",
        class: "bench-axis" }, shortDate(pts[pts.length - 1].date)));
    }

    if (series.target != null) {
      root.appendChild(svg("line", { x1: PAD.left, x2: W - PAD.right, y1: y(series.target), y2: y(series.target),
        class: "bench-target" }));
    }

    if (pts.length > 1) {
      var d = pts.map(function (p, i) { return (i ? "L" : "M") + x(p.date).toFixed(1) + "," + y(p.value).toFixed(1); });
      root.appendChild(svg("path", { d: d.join(""), class: "bench-line" }));
    }

    pts.forEach(function (p) {
      var tip = p.date + ": " + withUnit(p.value, series.unit) +
        (p.target_met === true ? " (target met)" : p.target_met === false ? " (target missed)" : "") +
        "\ncommit " + (p.commit ? p.commit.slice(0, 10) : "unknown");
      var holder = root;
      if (p.commit) {
        holder = svg("a", { href: repo + "/commit/" + p.commit, target: "_blank", rel: "noopener" });
        root.appendChild(holder);
      }
      var g = svg("g", { class: "bench-point" });
      g.appendChild(svg("title", null, tip));
      g.appendChild(svg("circle", { cx: x(p.date), cy: y(p.value), r: 10, class: "bench-hit" }));
      g.appendChild(svg("circle", { cx: x(p.date), cy: y(p.value), r: 4, class: "bench-dot" }));
      holder.appendChild(g);
    });
    return root;
  }

  function table(series) {
    var t = el("table");
    var head = el("tr");
    ["Date", "Value", "Commit"].forEach(function (h) { head.appendChild(el("th", null, h)); });
    t.appendChild(el("thead")).appendChild(head);
    var body = t.appendChild(el("tbody"));
    series.points.slice().reverse().forEach(function (p) {
      var row = body.appendChild(el("tr"));
      row.appendChild(el("td", null, p.date));
      row.appendChild(el("td", null, withUnit(p.value, series.unit)));
      row.appendChild(el("td")).appendChild(el("code", null, p.commit ? p.commit.slice(0, 10) : "n/a"));
    });
    return t;
  }

  function card(series, repo) {
    var box = el("figure", { class: "bench-card" });
    var cap = box.appendChild(el("figcaption"));
    cap.appendChild(el("span", { class: "bench-label" }, series.label));
    var last = series.points[series.points.length - 1];
    cap.appendChild(el("span", { class: "bench-latest" }, last ? withUnit(last.value, series.unit) : "n/a"));
    var hint = [el("code", null, series.metric)];
    if (DIRECTION[series.direction]) hint.push(DIRECTION[series.direction]);
    if (series.target != null) {
      var op = series.direction === "higher_is_better" ? "≥ " : series.direction === "lower_is_better" ? "≤ " : "";
      hint.push("target (dashed) " + op + withUnit(series.target, series.unit));
    }
    if (last) hint.push("latest " + last.date);
    var meta = box.appendChild(el("div", { class: "bench-meta" }));
    hint.forEach(function (h, i) {
      if (i) meta.appendChild(document.createTextNode(" · "));
      meta.appendChild(typeof h === "string" ? document.createTextNode(h) : h);
    });
    box.appendChild(chart(series, repo));
    if (series.points.length) {
      var det = box.appendChild(el("details"));
      det.appendChild(el("summary", null, "Values"));
      det.appendChild(table(series));
    }
    return box;
  }

  function render(root, data) {
    root.textContent = "";
    var repo = data.repository || "https://github.com/rayhankhilji/nugget";
    if (!data.generated_from) {
      root.appendChild(el("p", { class: "bench-note" },
        "No nightly results have been published yet; the charts fill in as runs land."));
    }
    var groups = [];
    data.series.forEach(function (s) {
      var g = groups[groups.length - 1];
      if (!g || g.run !== s.run) groups.push(g = { run: s.run, title: s.run_title, series: [] });
      g.series.push(s);
    });
    groups.forEach(function (g) {
      var sec = root.appendChild(el("section", { class: "bench-run" }));
      var h = sec.appendChild(el("h2", { id: "run-" + g.run }, g.title + " "));
      h.appendChild(el("code", null, g.run));
      var grid = sec.appendChild(el("div", { class: "bench-grid-cards" }));
      g.series.forEach(function (s) { grid.appendChild(card(s, repo)); });
    });
  }

  function start() {
    var root = document.getElementById("bench-dashboard");
    if (!root) return;
    root.appendChild(el("p", { class: "bench-note" }, "Loading results…"));
    fetch("history.json", { cache: "no-cache" })
      .then(function (r) {
        if (!r.ok) throw new Error("HTTP " + r.status);
        return r.json();
      })
      .then(function (data) { render(root, data); })
      .catch(function (e) {
        root.textContent = "";
        var p = root.appendChild(el("p", { class: "bench-note" }, "Could not load history.json (" + e.message + "). "));
        p.appendChild(el("a", { href: "history.json" }, "Raw data"));
      });
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
  else start();
})();
