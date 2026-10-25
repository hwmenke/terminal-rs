#!/usr/bin/env python3
"""Regenerate adaptive_trend_golden.json from the Whats-News Adaptive Trend rules.

Not imported by the Rust runtime. Conceptual port of adaptive_trend.py helpers
(_adaptive_ma, Wilder ATR, sticky regime, ratcheting stop) on a synthetic
rise-then-fall series so initialization, warm-up, a long→short flip and the
medium trailing stop can be checked without vendoring Python.
"""

from __future__ import annotations

import json
from pathlib import Path


def atr(high, low, close, n=20):
    tr = []
    for i, (h, l, c) in enumerate(zip(high, low, close)):
        prev = close[i - 1] if i else None
        span = h - l
        if prev is None:
            tr.append(span)
        else:
            tr.append(max(span, abs(h - prev), abs(l - prev)))
    out = [None] * len(tr)
    if len(tr) < n:
        return out
    out[n - 1] = sum(tr[:n]) / n
    for i in range(n, len(tr)):
        out[i] = out[i - 1] + (tr[i] - out[i - 1]) / n
    return out


def adaptive_ma(src, er_len, fast, slow, method="kama"):
    fast_sc = 2.0 / (fast + 1)
    slow_sc = 2.0 / (slow + 1)
    n = len(src)
    out = [None] * n
    first = next((i for i, v in enumerate(src) if v == v), None)
    if first is None:
        return out
    seed = first + er_len
    if seed > n:
        return out
    out[seed - 1] = src[seed - 1]
    for i in range(seed, n):
        direction = abs(src[i] - src[i - er_len])
        window = src[i - er_len : i + 1]
        volatility = sum(abs(window[j] - window[j - 1]) for j in range(1, len(window)))
        er = direction / volatility if volatility > 1e-12 else 0.0
        alpha = slow_sc + er * (fast_sc - slow_sc)
        if method == "kama":
            alpha *= alpha
        alpha = min(max(alpha, 0.0), 1.0)
        out[i] = out[i - 1] + alpha * (src[i] - out[i - 1])
    return out


def sticky(long_cond, short_cond, min_hold=10):
    state = [0] * len(long_cond)
    current = 0
    bars_since = min_hold
    for i, (lng, sh) in enumerate(zip(long_cond, short_cond)):
        bars_since += 1
        if current >= 0 and sh and bars_since >= min_hold:
            current = -1
            bars_since = 0
        elif current <= 0 and lng and bars_since >= min_hold:
            current = 1
            bars_since = 0
        state[i] = current
    return state


def ratchet(center, regime, atr_vals, multiple, kind):
    band = [None] * len(center)
    prev = 0
    for i, (c, r, a) in enumerate(zip(center, regime, atr_vals)):
        if r == 0 or c is None or a is None:
            if i and band[i - 1] is not None:
                band[i] = band[i - 1]
            prev = r
            continue
        raw = (c + multiple * a) if (r == 1) == (kind == "tp") else (c - multiple * a)
        flipped = r != prev
        if i == 0 or flipped or band[i - 1] is None:
            band[i] = raw
        elif r == 1:
            band[i] = max(band[i - 1], raw)
        else:
            band[i] = min(band[i - 1], raw)
        prev = r
    return band


def main():
    # Chop, then a trend, then a dump — a linear ramp keeps SB≈MB (ER=1, same
    # fast period) so the medium regime never fires under default confirm.
    closes = []
    for i in range(40):
        closes.append(100.0 + (0.4 if i % 2 == 0 else -0.4))
    for _ in range(80):
        closes.append(closes[-1] + 1.2)
    for _ in range(80):
        closes.append(closes[-1] - 1.6)
    high = [c + 0.5 for c in closes]
    low = [c - 0.5 for c in closes]
    hlc3 = [(h + l + c) / 3.0 for h, l, c in zip(high, low, closes)]
    sb = adaptive_ma(hlc3, 10, 2, 30)
    mb = adaptive_ma(hlc3, 20, 2, 60)
    lb = adaptive_ma(hlc3, 40, 2, 120)
    a = atr(high, low, closes, 20)
    sb_slope = [None] + [sb[i] - sb[i - 1] if sb[i] is not None and sb[i - 1] is not None else None for i in range(1, len(sb))]
    mb_slope = [None] + [mb[i] - mb[i - 1] if mb[i] is not None and mb[i - 1] is not None else None for i in range(1, len(mb))]
    med_long, med_short = [], []
    for i, c in enumerate(closes):
        confirm = 0.25 * a[i] if a[i] is not None else None
        if confirm is None or sb[i] is None or mb[i] is None or mb_slope[i] is None:
            med_long.append(False)
            med_short.append(False)
            continue
        med_long.append(sb[i] > mb[i] + confirm and mb_slope[i] > 0 and c > mb[i])
        med_short.append(sb[i] < mb[i] - confirm and mb_slope[i] < 0 and c < mb[i])
    medium = sticky(med_long, med_short, 10)
    mrt = ratchet(mb, medium, a, 2.25, "stop")
    long_at = next(i for i, s in enumerate(medium) if s == 1)
    short_at = next(i for i, s in enumerate(medium) if s == -1)
    first = lambda xs: next(i for i, v in enumerate(xs) if v is not None)
    payload = {
        "comment": (
            "Conceptual Whats-News Adaptive Trend vectors (adaptive_trend.py): "
            "KAMA baselines on hlc3, Wilder ATR(20), sticky medium regime, "
            "MRT = ratchet(MB, medium, 2.25×ATR, stop). 40-bar chop, 80-bar "
            "advance, 80-bar dump. Not vendored into the runtime."
        ),
        "closes": closes,
        "first_sb": first(sb),
        "first_mb": first(mb),
        "first_lb": first(lb),
        "first_atr": first(a),
        "long_at": long_at,
        "short_at": short_at,
        "mrt_long": mrt[long_at],
        "mrt_short": mrt[short_at],
        "sb_long": sb[long_at],
        "mb_long": mb[long_at],
    }
    dest = Path(__file__).with_name("adaptive_trend_golden.json")
    dest.write_text(json.dumps(payload, indent=2) + "\n")
    print(dest)
    print({k: payload[k] for k in payload if k != "closes"})


if __name__ == "__main__":
    main()
