// 主题编辑器 wasm 预览的文字后端：wind-scene-wasm 的 CanvasText 经 wasm-bindgen 调这里。
// 测量与画字都用 OffscreenCanvas（主线程与 Worker 都可用），用的是用户机器上的系统字体。
//
// font 是 CSS `font` 简写（Rust 侧由 TextStyle 拼出）；颜色是直通 alpha 的 RGBA（0–255）。

let ctx = null;

/** 至少 w×h 的 2D 上下文。改画布尺寸会重置上下文状态（font、fillStyle），调用方之后再设。 */
function context(w, h) {
  if (ctx === null) {
    ctx = new OffscreenCanvas(Math.max(w, 1), Math.max(h, 1)).getContext('2d', {
      willReadFrequently: true,
    });
  }
  const c = ctx.canvas;
  if (c.width < w || c.height < h) {
    c.width = Math.max(c.width, w);
    c.height = Math.max(c.height, h);
  }
  return ctx;
}

/** [宽, 行高]；行高 = fontBoundingBoxAscent + fontBoundingBoxDescent。 */
export function measure(text, font) {
  const g = context(1, 1);
  g.font = font;
  const m = g.measureText(text);
  return new Float64Array([m.width, m.fontBoundingBoxAscent + m.fontBoundingBoxDescent]);
}

/**
 * 以文字盒左上角为原点画整串文字，四周留边容纳伸出的字形。
 * 返回 { x, y, w, h, pixels }：位图左上角相对原点的偏移（≤ 0）、尺寸、getImageData 的像素。
 */
export function draw(text, font, r, g, b, a) {
  let c = context(1, 1);
  c.font = font;
  const m = c.measureText(text);
  const asc = m.fontBoundingBoxAscent;
  const lineH = asc + m.fontBoundingBoxDescent;
  const left = Math.ceil(Math.max(0, m.actualBoundingBoxLeft)) + 1;
  const top = Math.ceil(Math.max(0, m.actualBoundingBoxAscent - asc)) + 1;
  const right = Math.ceil(Math.max(m.width, m.actualBoundingBoxRight)) + 1;
  const bottom = Math.ceil(Math.max(lineH, asc + m.actualBoundingBoxDescent)) + 1;
  const w = left + right;
  const h = top + bottom;
  c = context(w, h);
  c.clearRect(0, 0, w, h);
  c.font = font;
  c.textBaseline = 'alphabetic';
  c.fillStyle = `rgba(${r}, ${g}, ${b}, ${a / 255})`;
  c.fillText(text, left, top + asc);
  return { x: -left, y: -top, w, h, pixels: c.getImageData(0, 0, w, h).data };
}
