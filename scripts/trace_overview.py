#!/usr/bin/env python3
"""Render a multi-track timeline (SVG) from a Chrome / Perfetto trace JSON.

Draws one lane per thread (spans as nested slices, warnings and errors as
markers) and a few counter tracks, for a time window. Used to make the README
images from the rtlog `app_trace` example; the same JSON opens in
https://ui.perfetto.dev for interactive viewing.

usage: trace_overview.py TRACE.json OUT.svg [--from-ms A --to-ms B]
                         [--counters name,name] [--title TEXT] [--width PX]
"""
import argparse
import colorsys
import html
import json
import math
import zlib

FONT = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"
BG, PANEL, GRID, TEXT, MUTED = "#0f1117", "#161a23", "#262b36", "#e6e9ef", "#8b93a7"
WARN, ERROR = "#f5a524", "#f04d4d"
COUNTER_COLORS = ["#4cc9f0", "#a78bfa", "#34d399", "#f472b6", "#fbbf24"]


def color_for(name):
    h = (zlib.crc32(name.encode()) % 360) / 360.0
    r, g, b = colorsys.hls_to_rgb(h, 0.62, 0.55)
    return "#%02x%02x%02x" % (int(r * 255), int(g * 255), int(b * 255))


def nice_step(span):
    raw = span / 8
    mag = 10 ** math.floor(math.log10(raw))
    for m in (1, 2, 5, 10):
        if raw <= m * mag:
            return m * mag
    return 10 * mag


def fmt_us(us):
    if us >= 1000:
        v = us / 1000
        return ("%.0f ms" if v >= 10 else "%.1f ms") % v
    return "%.0f µs" % us


def load(path):
    ev = json.load(open(path))["traceEvents"]
    names, threads, counters = {}, {}, {}
    for e in ev:
        ph = e.get("ph")
        if ph == "M" and e.get("name") == "thread_name":
            names[e["tid"]] = e["args"]["name"]
        elif ph in ("B", "E", "i"):
            threads.setdefault(e["tid"], []).append(e)
        elif ph == "C":
            (key, val), = e["args"].items()
            counters.setdefault(e["name"], []).append((e["ts"], val))
    lanes = []
    for tid in sorted(threads):
        stack, slices, marks = [], [], []
        for e in sorted(threads[tid], key=lambda x: (x["ts"], 0 if x["ph"] == "E" else 1)):
            if e["ph"] == "B":
                stack.append((e["ts"], e["name"]))
            elif e["ph"] == "E" and stack:
                start, name = stack.pop()
                slices.append((start, e["ts"], name, len(stack)))
            elif e["ph"] == "i":
                marks.append((e["ts"], e.get("cat", ""), e["name"]))
        lanes.append((names.get(tid, "ctx%d" % tid), slices, marks))
    lanes.sort(key=lambda lane: lane[0])
    end = max([s[1] for _, sl, _ in lanes for s in sl] + [m[0] for _, _, mk in lanes for m in mk] + [0])
    return lanes, counters, end


def render(trace, out, t_from, t_to, counter_names, title, width):
    lanes, counters, end = load(trace)
    t_to = end if t_to is None else t_to
    left, right, top = 150, 18, 64
    plot_w = width - left - right
    x = lambda t: left + (t - t_from) / (t_to - t_from) * plot_w
    row_h, lane_pad = 15, 8
    parts, y = [], top + 26

    def text(xp, yp, s, size=12, fill=TEXT, anchor="start", weight="normal"):
        parts.append('<text x="%.1f" y="%.1f" font-size="%d" fill="%s" text-anchor="%s" font-weight="%s">%s</text>'
                     % (xp, yp, size, fill, anchor, weight, html.escape(s)))

    # Axis.
    step = nice_step(t_to - t_from)
    tick = math.ceil(t_from / step) * step
    grid = []
    while tick <= t_to:
        grid.append(tick)
        tick += step
    # Thread lanes.
    lane_boxes = []
    for name, slices, marks in lanes:
        vis = [s for s in slices if s[1] >= t_from and s[0] <= t_to]
        depth = max([s[3] for s in vis] + [0]) + 1 if vis else 1
        h = depth * row_h + lane_pad * 2 + 12
        lane_boxes.append((name, vis, [m for m in marks if t_from <= m[0] <= t_to], y, h))
        y += h + 4
    # Counter tracks.
    ctr_h = 44
    ctr_boxes = []
    for cname in counter_names:
        pts = counters.get(cname)
        if pts:
            ctr_boxes.append((cname, pts, y, ctr_h))
            y += ctr_h + 4
    height = y + 34

    parts.append('<rect width="%d" height="%d" fill="%s"/>' % (width, height, BG))
    text(18, 30, title, 17, TEXT, weight="bold")
    legend = "%s window · each lane is a thread · slices are spans · ▲ warn / error · ticks are log records" % fmt_us(t_to - t_from)
    if counter_names:
        legend += " · bottom tracks are counters from log fields"
    text(18, 50, legend, 12, MUTED)
    for g in grid:
        gx = x(g)
        parts.append('<line x1="%.1f" x2="%.1f" y1="%d" y2="%d" stroke="%s"/>' % (gx, gx, top + 10, y, GRID))
        text(gx + 3, top + 20, fmt_us(g), 11, MUTED)

    for name, vis, marks, ly, h in lane_boxes:
        parts.append('<rect x="%d" y="%d" width="%d" height="%d" fill="%s" rx="4"/>' % (8, ly, width - 16, h, PANEL))
        text(18, ly + 17, name, 12, TEXT, weight="bold")
        for s0, s1, sname, d in vis:
            x0, x1 = max(x(s0), left), min(x(s1), left + plot_w)
            w = max(x1 - x0, 0.8)
            sy = ly + lane_pad + 4 + d * row_h
            parts.append('<rect x="%.2f" y="%d" width="%.2f" height="%d" fill="%s" rx="2"/>'
                         % (x0, sy, w, row_h - 2, color_for(sname)))
            if w > len(sname) * 6.6 + 8:
                parts.append('<text x="%.1f" y="%d" font-size="10" fill="#0b0d12">%s</text>'
                             % (x0 + 4, sy + row_h - 5, html.escape(sname)))
        for ts, cat, mname in marks:
            if cat not in ("warn", "error"):
                mx = x(ts)
                parts.append('<line x1="%.1f" x2="%.1f" y1="%d" y2="%d" stroke="%s" stroke-opacity="0.55"/>'
                             % (mx, mx, ly + h - 9, ly + h - 3, MUTED))
        # Errors last, so a warning logged right after never hides one.
        for ts, cat, mname in sorted(marks, key=lambda m: m[1] == "error"):
            if cat in ("warn", "error"):
                mx, col = x(ts), ERROR if cat == "error" else WARN
                parts.append('<path d="M%.1f %d l5 9 h-10 z" fill="%s"><title>%s</title></path>'
                             % (mx, ly + 2, col, html.escape(mname)))

    for i, (cname, pts, cy, ch) in enumerate(ctr_boxes):
        col = COUNTER_COLORS[i % len(COUNTER_COLORS)]
        vis = [(t, v) for t, v in pts if t_from <= t <= t_to]
        prev = [p for p in pts if p[0] < t_from][-1:]
        vis = prev + vis
        parts.append('<rect x="%d" y="%d" width="%d" height="%d" fill="%s" rx="4"/>' % (8, cy, width - 16, ch, PANEL))
        text(18, cy + 17, cname, 11, TEXT, weight="bold")
        if not vis:
            continue
        lo, hi = min(min(v for _, v in vis), 0), max(v for _, v in vis)
        hi = hi if hi > lo else lo + 1
        yv = lambda v: cy + ch - 5 - (v - lo) / (hi - lo) * (ch - 12)
        d = "M%.1f %.1f" % (max(x(vis[0][0]), left), yv(vis[0][1]))
        for (t0, v0), (t1, v1) in zip(vis, vis[1:]):
            d += " H%.1f V%.1f" % (x(t1), yv(v1))
        d += " H%.1f" % (left + plot_w)
        area = d + " V%.1f H%.1f Z" % (yv(lo), max(x(vis[0][0]), left))
        parts.append('<path d="%s" fill="%s" fill-opacity="0.18"/>' % (area, col))
        parts.append('<path d="%s" fill="none" stroke="%s" stroke-width="1.4"/>' % (d, col))
        label = ("%.3g" % hi) if abs(hi) < 1e4 else ("%.1f MiB" % (hi / 1048576) if "bytes" in cname else "%.3g" % hi)
        text(18, cy + 33, "max " + label, 10, MUTED)

    text(18, height - 12, "rendered by scripts/trace_overview.py from the trace that `rtlog decode --perfetto` writes; "
         "open the same file in ui.perfetto.dev", 11, MUTED)
    svg = ('<svg xmlns="http://www.w3.org/2000/svg" width="%d" height="%d" viewBox="0 0 %d %d" font-family="%s">%s</svg>'
           % (width, height, width, height, FONT, "".join(parts)))
    open(out, "w").write(svg)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("trace")
    ap.add_argument("out")
    ap.add_argument("--from-ms", type=float, default=0.0)
    ap.add_argument("--to-ms", type=float, default=None)
    ap.add_argument("--counters", default="mem-monitor/live_bytes,control/err,control/out,http-0/latency_us")
    ap.add_argument("--title", default="rtlog · app_trace")
    ap.add_argument("--width", type=int, default=1600)
    a = ap.parse_args()
    render(a.trace, a.out, a.from_ms * 1000, None if a.to_ms is None else a.to_ms * 1000,
           [c for c in a.counters.split(",") if c], a.title, a.width)


if __name__ == "__main__":
    main()
