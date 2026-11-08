// 本机冒烟：在 node 里加载 build-scene-wasm.sh 的产物，跑一遍 Previewer 与 AutoPreviewer 的 API。
// node 没有 OffscreenCanvas，这里垫一个假的（宽 = 字数 × 字号 × 0.6，画字出实心矩形），
// 真正的 text.js、preview.js 与 wasm-bindgen 胶水照常走——测的是 JS ↔ wasm 这一段接得上。
//
// 产物先复制到临时目录、旁边放 {"type":"module"}，不受上层 package.json 的模块类型影响。
//
// 用法: node smoke.mjs <产物目录>

import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const src = process.argv[2];
if (!src) {
  console.error('用法: node smoke.mjs <产物目录>');
  process.exit(2);
}
const dir = mkdtempSync(join(tmpdir(), 'wind-scene-smoke-'));
cpSync(src, dir, { recursive: true });
writeFileSync(join(dir, 'package.json'), '{"type":"module"}\n');

const sizeOf = (font) => Number(/([\d.]+)px/.exec(font)?.[1] ?? 16);
/** 置位时 measureText 抛异常（模拟浏览器侧出错）。 */
let failText = false;

class FakeContext {
  constructor(canvas) {
    this.canvas = canvas;
    this.font = '10px sans-serif';
    this.fillStyle = '#000';
    this.textBaseline = 'alphabetic';
    this.rects = [];
  }
  measureText(text) {
    if (failText) throw new Error('假画布：measureText 出错');
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
    this.rects.push({ x, y: y - m.fontBoundingBoxAscent, w: m.width, h: sizeOf(this.font) * 1.2 });
  }
  getImageData(x0, y0, w, h) {
    const data = new Uint8ClampedArray(w * h * 4);
    for (const r of this.rects) {
      for (let y = Math.max(0, Math.round(r.y)); y < Math.min(h, Math.round(r.y + r.h)); y++) {
        for (let x = Math.max(0, Math.round(r.x)); x < Math.min(w, Math.round(r.x + r.w)); x++) {
          data.set([255, 255, 255, 255], (y * w + x) * 4);
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

function throws(fn, needle) {
  try {
    fn();
  } catch (e) {
    return String(e?.message ?? e).includes(needle);
  }
  return false;
}

try {
  const wasmBytes = readFileSync(join(dir, 'wind_scene_bg.wasm'));
  const m = await import(pathToFileURL(join(dir, 'wind_scene.js')).href);
  m.initSync({ module: wasmBytes });

  // —— 裸 API ——
  const p = new m.Previewer();
  const raw = p.render('{}');
  const f = { width: raw.width, height: raw.height };
  const rgba = raw.rgba;
  raw.free();
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
  dark.free();

  const vf = p.render(JSON.stringify({ vertical: true, scale: 2 }));
  const v = { width: vf.width, height: vf.height };
  vf.free();
  check(v.height > v.width, `竖排尺寸 ${v.width}x${v.height}`);

  p.setTheme('base = "msime"\n[meta]\nname = "t"\n', {});
  p.render('{}').free();
  check(throws(() => p.setTheme('[meta\nname = 1', {}), 'parse theme'), '解析错误原样抛出');
  check(throws(() => p.setTheme('base = "nope"\n', {}), '只认内置主题'), '非内置 base 说清楚');
  check(typeof p.signal('typing') === 'boolean', 'signal 返回布尔');
  check(throws(() => p.signal('nonsense'), '未知事件'), '未知事件报错');
  // 参数上限：报错、不 panic，之后照常可用。
  for (const bad of [{ t_ms: 1e300 }, { font_size: 100000 }, { font_size: 200, scale: 8 }]) {
    try {
      p.render(JSON.stringify(bad)).free();
    } catch (e) {
      check(!(e instanceof WebAssembly.RuntimeError), `参数 ${JSON.stringify(bad)} 不该陷入`);
    }
  }
  // JS 侧抛异常：这一帧失败，实例不报废。
  failText = true;
  check(throws(() => p.render(JSON.stringify({ font_family: 'X' })), '假画布'), 'JS 异常变成报错');
  failText = false;
  p.render('{}').free();

  // —— AutoPreviewer：致命错误后自动重建并重放主题 ——
  const { AutoPreviewer } = await import(pathToFileURL(join(dir, 'preview.js')).href);
  const a = await AutoPreviewer.create({ wasm: wasmBytes });
  await a.setTheme('base = "jade"\n[meta]\nname = "j"\n', {});
  const before = await a.render({});
  check(before.rgba.length === before.width * before.height * 4, '包装出普通对象');
  a.raw.render = () => {
    throw new WebAssembly.RuntimeError('unreachable');
  };
  const after = await a.render({});
  check(a.generation === 2, `应重建一次，实为 ${a.generation}`);
  check(
    after.width === before.width && after.rgba.every((x, i) => x === before.rgba[i]),
    '重建后重放了主题，同一帧',
  );
  let normal = '';
  try {
    await a.setTheme('[meta', {});
  } catch (e) {
    normal = String(e?.message ?? e);
  }
  check(normal.includes('parse theme') && a.generation === 2, '普通错误不触发重建');

  console.log(
    `冒烟通过：横排 ${f.width}x${f.height}、竖排×2 ${v.width}x${v.height}，单帧 ${ms.toFixed(1)} ms，自动重建 OK`,
  );
} finally {
  rmSync(dir, { recursive: true, force: true });
}
