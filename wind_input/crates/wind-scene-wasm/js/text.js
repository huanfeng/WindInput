// 主题编辑器 wasm 预览的文字后端：wind-scene-wasm 的 CanvasText 经 wasm-bindgen 调这里。
// 测量与画字都用 OffscreenCanvas（主线程与 Worker 都可用），用的是用户机器上的系统字体。
// 抛出的异常在 Rust 侧变成「这一帧失败」，不会让预览器报废。
//
// font 是 CSS `font` 简写（Rust 侧由 TextStyle 拼出），text 不含换行。

/** 画字位图单边上限（与 Rust 侧 web.rs 的 MAX_GLYPH_SIDE 一致）。 */
const MAX_SIDE = 16384;
/** 画布面积超过它，用完缩回 1×1，别让一次大字常驻显存。 */
const KEEP_AREA = 1 << 22;

let ctx = null;

/** 至少 w×h 的 2D 上下文。改画布尺寸会重置上下文状态（font、fillStyle），调用方之后再设。 */
function context(w, h) {
  if (ctx === null) {
    ctx = new OffscreenCanvas(1, 1).getContext('2d', { willReadFrequently: true });
    if (!ctx) throw new Error('OffscreenCanvas 2d 上下文不可用');
  }
  const c = ctx.canvas;
  if (c.width < w || c.height < h) {
    c.width = Math.max(c.width, w);
    c.height = Math.max(c.height, h);
  }
  return ctx;
}

function metrics(text, font) {
  const g = context(1, 1);
  g.font = font;
  const m = g.measureText(text);
  const asc = m.fontBoundingBoxAscent;
  const desc = m.fontBoundingBoxDescent;
  if (!Number.isFinite(m.width) || !Number.isFinite(asc) || !Number.isFinite(desc)) {
    throw new Error('measureText 缺 fontBoundingBox（浏览器过旧）');
  }
  return { m, asc, desc };
}

/** [宽, 行高]；行高 = fontBoundingBoxAscent + fontBoundingBoxDescent。 */
export function measure(text, font) {
  const { m, asc, desc } = metrics(text, font);
  return new Float64Array([m.width, asc + desc]);
}

/**
 * 用不透明灰度 ink（255 = 白、0 = 黑）画一行文字，只栅格化文字盒局部横坐标 [x0, x1) 那段
 * （Rust 传缓冲里可见的部分）。彩色字形（emoji）不随 ink 变，Rust 侧靠黑白两遍对比把它认出来。
 * 返回 { x, y, w, h, pixels }：位图左上角在文字盒局部坐标里的位置、尺寸、getImageData 的像素。
 */
export function draw(text, font, x0, x1, ink) {
  const { m, asc, desc } = metrics(text, font);
  const lineH = asc + desc;
  // 字形可能伸出文字盒：左右按 actualBoundingBox 外扩，再各留 1 px。
  const left = Math.ceil(Math.max(0, m.actualBoundingBoxLeft || 0)) + 1;
  const right = Math.ceil(Math.max(m.width, m.actualBoundingBoxRight || 0)) + 1;
  const top = Math.ceil(Math.max(0, (m.actualBoundingBoxAscent || 0) - asc)) + 1;
  const bottom = Math.ceil(Math.max(lineH, asc + (m.actualBoundingBoxDescent || 0))) + 1;
  const a = Math.max(x0, -left);
  const b = Math.min(x1, right);
  const w = Math.ceil(b - a);
  const h = top + bottom;
  if (!Number.isFinite(w) || !Number.isFinite(h) || w > MAX_SIDE || h > MAX_SIDE) {
    throw new Error(`画字尺寸越界：${w}×${h}`);
  }
  if (w <= 0) return { x: 0, y: 0, w: 0, h: 0, pixels: new Uint8ClampedArray(0) };
  const c = context(w, h);
  c.clearRect(0, 0, w, h);
  c.font = font;
  c.textBaseline = 'alphabetic';
  c.fillStyle = `rgb(${ink}, ${ink}, ${ink})`;
  c.fillText(text, -a, top + asc);
  const pixels = c.getImageData(0, 0, w, h).data;
  if (c.canvas.width * c.canvas.height > KEEP_AREA) {
    c.canvas.width = 1;
    c.canvas.height = 1;
  }
  return { x: a, y: -top, w, h, pixels };
}
