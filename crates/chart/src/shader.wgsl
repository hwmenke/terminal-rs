// Chart shaders. All geometry is generated in the vertex shader from storage buffers:
// one instance per (merged) bar or per line segment, 6 vertices per rectangle.
// Coordinates are NDC within the pane viewport set by the caller.

struct Uniforms {
    // width_px, height_px, left_hi, left_lo  (left edge index split for f32 precision)
    rect: vec4<f32>,
    // visible_bars, y_lo, y_hi, log_scale (0/1); y bounds are in transformed space
    view: vec4<f32>,
    up: vec4<f32>,
    down: vec4<f32>,
    color: vec4<f32>,
    // body_frac, wick_px, thickness_px, mode (0 = candles, 1 = OHLC bars)
    params: vec4<f32>,
    // vol_frac, vol_max, hist_width_bars, baseline
    extra: vec4<f32>,
    // reveal front, ramp (bars), line front (both relative to left_hi), pane opacity
    anim: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

fn x_ndc(idx: f32) -> f32 {
    let rel = (idx - u.rect.z) - u.rect.w;
    return rel / u.view.x * 2.0 - 1.0;
}

fn y_t(p: f32) -> f32 {
    if (u.view.w > 0.5) {
        return log(max(p, 1e-30));
    }
    return p;
}

fn y_ndc(p: f32) -> f32 {
    return (y_t(p) - u.view.y) / (u.view.z - u.view.y) * 2.0 - 1.0;
}

fn px_to_ndc_x(px: f32) -> f32 {
    return px * 2.0 / u.rect.x;
}

fn px_to_ndc_y(px: f32) -> f32 {
    return px * 2.0 / u.rect.y;
}

fn corner(vi: u32) -> vec2<f32> {
    var c = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    return c[vi % 6u];
}

// Premultiplied colour with the pane opacity applied.
fn shade(color: vec4<f32>) -> vec4<f32> {
    let a = color.a * u.anim.w;
    return vec4<f32>(color.rgb * a, a);
}

// How far the bar centred at `idx` has grown in (0..1, eased) behind the reveal front.
fn grow(idx: f32) -> f32 {
    let f = clamp((u.anim.x - (idx - u.rect.z)) / u.anim.y, 0.0, 1.0);
    let r = 1.0 - f;
    return 1.0 - r * r * r;
}

fn rect(vi: u32, x0: f32, x1: f32, y0: f32, y1: f32, color: vec4<f32>) -> VsOut {
    let c = corner(vi);
    var out: VsOut;
    out.pos = vec4<f32>(mix(x0, x1, c.x), mix(y0, y1, c.y), 0.0, 1.0);
    out.color = shade(color);
    return out;
}

fn degenerate() -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(2.0, 2.0, 0.0, 1.0);
    out.color = vec4<f32>(0.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}

// ---- bars -------------------------------------------------------------------------------

struct Bar {
    x: f32,
    w: f32,
    open: f32,
    high: f32,
    low: f32,
    close: f32,
    volume: f32,
    pad: f32,
};

@group(1) @binding(0) var<storage, read> bars: array<Bar>;

// 18 vertices per bar: three rectangles. Candles use wick + body; OHLC bars use
// stem + open tick + close tick.
@vertex
fn vs_candle(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VsOut {
    let b = bars[ii];
    let g = grow(b.x + b.w * 0.5);
    if (g <= 0.0) {
        return degenerate();
    }
    // Growing in: the bar opens out from its open price.
    let high = mix(b.open, b.high, g);
    let low = mix(b.open, b.low, g);
    let close = mix(b.open, b.close, g);
    var color = select(u.down, u.up, b.close >= b.open);
    color.a = color.a * clamp(g * 3.0, 0.0, 1.0);
    let cx = x_ndc(b.x + b.w * 0.5);
    let half_body = max((b.w * u.params.x * 0.5) / u.view.x * 2.0, px_to_ndc_x(0.5));
    let half_wick = px_to_ndc_x(u.params.y * 0.5);
    let r = vi / 6u;
    let one_px = px_to_ndc_y(1.0);
    if (u.params.w < 0.5) {
        if (r == 0u) {
            return rect(vi, cx - half_wick, cx + half_wick, y_ndc(low), max(y_ndc(high), y_ndc(low) + one_px), color);
        }
        if (r == 1u) {
            let y0 = y_ndc(min(b.open, close));
            let y1 = max(y_ndc(max(b.open, close)), y0 + one_px);
            return rect(vi, cx - half_body, cx + half_body, y0, y1, color);
        }
        return degenerate();
    }
    let tick = px_to_ndc_y(max(u.params.y, 1.0) * 0.5);
    if (r == 0u) {
        return rect(vi, cx - half_wick, cx + half_wick, y_ndc(low), max(y_ndc(high), y_ndc(low) + one_px), color);
    }
    if (r == 1u) {
        let y = y_ndc(b.open);
        return rect(vi, cx - half_body, cx, y - tick, y + tick, color);
    }
    let y = y_ndc(close);
    return rect(vi, cx, cx + half_body, y - tick, y + tick, color);
}

@vertex
fn vs_volume(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VsOut {
    let b = bars[ii];
    if (!(b.volume > 0.0)) {
        return degenerate();
    }
    let g = grow(b.x + b.w * 0.5);
    if (g <= 0.0) {
        return degenerate();
    }
    var color = select(u.down, u.up, b.close >= b.open);
    color.a = color.a * u.color.a;
    let cx = x_ndc(b.x + b.w * 0.5);
    let half = max((b.w * u.params.x * 0.5) / u.view.x * 2.0, px_to_ndc_x(0.5));
    let h = clamp(b.volume / max(u.extra.y, 1e-30), 0.0, 1.0) * u.extra.x * 2.0 * g;
    return rect(vi, cx - half, cx + half, -1.0, -1.0 + h, color);
}
