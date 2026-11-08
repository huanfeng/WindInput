// 本机冒烟：在 node 里加载 build-scene-wasm.sh 的产物，跑一遍 Previewer 的 API。
// node 没有 OffscreenCanvas，这里垫一个假的（宽 = 字数 × 字号 × 0.6，画字出实心矩形），
// 真正的 text.js 与 wasm-bindgen 胶水照常走——测的是 JS ↔ wasm 这一段接得上。
//
// 用法: node smoke.mjs <产物目录>

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const dir = process.argv[2];
if (!dir) {
  console.error('用法: node smoke.mjs <产物目录>');
  process.exit(2);
}

const sizeOf = (font) => Number(/([\d.]+)px/.exec(font)?.[1] ?? 16);

class FakeContext {
  constructor(canvas) {
    this.canvas = canvas;
    this.font = '10px sans-serif';
    this.fillStyle = '#000';
    this.textBaseline = 'alphabetic';
    this.rects = [];
  }
  measureText(text) {
    const s = sizeOf(this.font);
    const width = [...text].length * s * 0.6;
    return {
      width,
      fontBoundingBoxAscent: s * 0.9,
      fontBoundingBoxDescent: s * 0.3,
      actualBoundingBoxLeft: 0,
      actualBoundingBoxRight: width,
      actualBoundingBoxAscent: s * 0.8,
      actualBoundingBoxDescent: s * 0.2,
    };
  }
  clearRect() {
    this.rects = [];
  }
  fillText(text, x, y) {
    const m = this.measureText(text);
    const rgba = /rgba\((\d+), (\d+), (\d+), ([\d.]+)\)/.exec(this.fillStyle);
    const c = [Number(rgba[1]), Number(rgba[2]), Number(rgba[3]), Math.round(Number(rgba[4]) * 255)];
    this.rects.push({ x, y: y - m.fontBoundingBoxAscent, w: m.width, h: sizeOf(this.font) * 1.2, c });
  }
  getImageData(x0, y0, w, h) {
    const data = new Uint8ClampedArray(w * h * 4);
    for (const r of this.rects) {
      for (let y = Math.max(0, Math.round(r.y)); y < Math.min(h, Math.round(r.y + r.h)); y++) {
        for (let x = Math.max(0, Math.round(r.x)); x < Math.min(w, Math.round(r.x + r.w)); x++) {
          data.set(r.c, (y * w + x) * 4);
        }
      }
    }
    return { data };
  }
}

globalThis.OffscreenCanvas = class {
  constructor(w, h) {
    this.width = w;
    this.height = h;
  }
  getContext() {
    return (this.ctx ??= new FakeContext(this));
  }
};

function check(ok, msg) {
  if (!ok) {
    console.error(`冒烟失败：${msg}`);
    process.exit(1);
  }
}

const m = await import(pathToFileURL(join(dir, 'wind_scene.js')).href);
m.initSync({ module: readFileSync(join(dir, 'wind_scene_bg.wasm')) });

const p = new m.Previewer();
const f = p.render('{}');
const rgba = f.rgba;
check(f.width > f.height && f.height > 0, `横排尺寸 ${f.width}x${f.height}`);
check(rgba.length === f.width * f.height * 4, '像素长度');
let opaque = 0;
for (let i = 3; i < rgba.length; i += 4) opaque += rgba[i] === 255 ? 1 : 0;
check(opaque > 0, '窗口底色应画出');

const t0 = performance.now();
const dark = p.render(JSON.stringify({ dark: true }));
const ms = performance.now() - t0;
check(dark.width === f.width, '明暗同几何');
check(dark.rgba.some((v, i) => v !== rgba[i]), '明暗换色');

const v = p.render(JSON.stringify({ vertical: true, scale: 2 }));
check(v.height > v.width, `竖排尺寸 ${v.width}x${v.height}`);

p.setTheme('base = "msime"\n[meta]\nname = "t"\n', {});
p.render('{}');
let msg = '';
try {
  p.setTheme('[meta\nname = 1', {});
} catch (e) {
  msg = String(e.message ?? e);
}
check(msg.includes('parse theme'), `解析错误原样抛出：${msg}`);
check(typeof p.signal('typing') === 'boolean', 'signal 返回布尔');
msg = '';
try {
  p.signal('nonsense');
} catch (e) {
  msg = String(e.message ?? e);
}
check(msg.includes('未知事件'), `未知事件报错：${msg}`);

console.log(`冒烟通过：横排 ${f.width}x${f.height}、竖排×2 ${v.width}x${v.height}，单帧 ${ms.toFixed(1)} ms`);
