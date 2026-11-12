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
import { deflateSync } from 'node:zlib';

const src = process.argv[2];
if (!src) {
  console.error('用法: node smoke.mjs <产物目录>');
  process.exit(2);
}
const dir = mkdtempSync(join(tmpdir(), 'wind-scene-smoke-'));

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

/** 纯色 RGBA PNG（node 的 zlib 现编，不用素材）。 */
function png(w, h, rgba) {
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc = (buf) => {
    let c = 0xffffffff;
    for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (type, data) => {
    const out = Buffer.alloc(12 + data.length);
    out.writeUInt32BE(data.length, 0);
    out.write(type, 4, 'latin1');
    data.copy(out, 8);
    out.writeUInt32BE(crc(out.subarray(4, 8 + data.length)), 8 + data.length);
    return out;
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr.set([8, 6, 0, 0, 0], 8);
  const row = Buffer.concat([Buffer.from([0]), Buffer.from(Array(w).fill(rgba).flat())]);
  const raw = Buffer.concat(Array(h).fill(row));
  return new Uint8Array(
    Buffer.concat([
      Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
      chunk('IHDR', ihdr),
      chunk('IDAT', deflateSync(raw)),
      chunk('IEND', Buffer.alloc(0)),
    ]),
  );
}

/** 失败抛错（不直接 exit），让 finally 清掉临时目录。 */
function check(ok, msg) {
  if (!ok) throw new Error(`冒烟失败：${msg}`);
}

function throws(fn, needle = '') {
  try {
    fn();
  } catch (e) {
    return String(e?.message ?? e).includes(needle);
  }
  return false;
}

try {
  cpSync(src, dir, { recursive: true });
  writeFileSync(join(dir, 'package.json'), '{"type":"module"}\n');
  const wasmBytes = readFileSync(join(dir, 'wind_scene_bg.wasm'));
  const m = await import(pathToFileURL(join(dir, 'wind_scene.js')).href);
  m.initSync({ module: wasmBytes });

  // —— 裸 API ——
  const p = new m.Previewer();
  const raw = p.render('{}');
  const f = { width: raw.width, height: raw.height };
  const rgba = raw.rgba;
  const nodes = JSON.parse(raw.nodes);
  raw.free();
  check(
    nodes[0]?.key === 'window' && nodes.some((n) => n.key === 'item'),
    `节点矩形 ${nodes.map((n) => n.key).join(',')}`,
  );
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
  check(Array.isArray(before.nodes) && before.nodes[0].key === 'window', '包装解出节点矩形');
  check(typeof before.clamped === 'boolean', '包装带出 clamped');
  check(before.skin === undefined && Array.isArray(before.warnings), '无皮肤：skin 缺省、warnings 为数组');

  // —— 皮肤回执与帧信息 ——
  const frame = png(32, 32, [200, 200, 200, 255]);
  const skinToml =
    '[meta]\nname = "s"\n[skin]\ndensity = 1\n' +
    '[skin.horizontal.frame]\nimage = "assets/frame.png"\nstretch_x = [[8, 24]]\n' +
    'stretch_y = [{ from = 8, to = 24, fill = "zigzag" }]\n' +
    '[[skin.horizontal.decor]]\nimage = "assets/frame.png"\nanchor = "frame.top_right"\npivot = "center"\n';
  const tw = await a.setTheme(skinToml, { 'assets/frame.png': frame });
  check(tw.length === 1 && tw[0].startsWith('skin: 未知拉伸带填法'), `setTheme 告警 ${JSON.stringify(tw)}`);
  const sf = await a.render({ scale: 2 });
  const sk = sf.skin;
  check(sk?.variant === 'horizontal' && sk.k === 2 && sk.renderScale === 2, `skin 段 ${JSON.stringify(sk)?.slice(0, 120)}`);
  check(sk.axisX.map((s) => s.kind).join() === 'fixed,round,fixed', `轴映射 ${JSON.stringify(sk.axisX)}`);
  check(sk.decor.length === 1 && sk.decor[0].rect.y < sk.frame.y, '外伸装饰');
  check(sf.warnings.length === 0, `帧告警 ${JSON.stringify(sf.warnings)}`);
  const { FrameTools } = await import(pathToFileURL(join(dir, 'preview.js')).href);
  const tools = await FrameTools.create({ wasm: wasmBytes });
  const info = await tools.inspectImage(frame);
  check(info.format === 'png' && info.width === 32 && !info.animated && info.frames.length === 1, `inspectImage ${JSON.stringify(info)}`);
  const thumbs = await tools.decodeFrames(frame, { max_edge: 8 });
  check(thumbs.length === 1 && thumbs[0].w === 8 && thumbs[0].rgba.length === 8 * 8 * 4, 'decodeFrames 缩略');
  check(thumbs[0].rgba[0] === 200 && thumbs[0].rgba[3] === 255, '缩略帧是直通 RGBA');
  check(throws(() => m.inspectImage(new Uint8Array([1, 2, 3]))), '认不出的图片报错');
  check(throws(() => m.decodeFrames(frame, JSON.stringify({ max_edge: 9999 }))), '缩略边长超限报错');
  check(typeof a.inspectImage === 'undefined', '帧信息不挂在预览器上');
  let normalErr = '';
  try {
    await tools.inspectImage(new Uint8Array([1, 2, 3]));
  } catch (e) {
    normalErr = String(e?.message ?? e);
  }
  check(normalErr.includes('认不出图片格式') && tools.generation === 1, '普通错误不重建帧工具');
  // 帧工具崩了：换自己的新实例重试，预览器不受牵连。
  let first = true;
  const ft = await FrameTools.create({
    wasm: wasmBytes,
    onInstance: (api) => {
      if (first) {
        first = false;
        api.inspectImage = () => {
          throw new WebAssembly.RuntimeError('unreachable');
        };
      }
    },
  });
  const gA = a.generation;
  let crash = '';
  try {
    await ft.inspectImage(frame);
  } catch (e) {
    crash = e instanceof WebAssembly.RuntimeError ? 'fatal' : String(e);
  }
  check(crash === 'fatal' && ft.generation === 2, `崩了原样抛、换新实例：${crash} / ${ft.generation}`);
  const again = await ft.inspectImage(frame);
  check(again.width === 32 && ft.generation === 2, `之后的调用用新实例：${ft.generation}`);
  check(a.generation === gA, '帧工具重建不动预览器');
  await a.setTheme('base = "jade"\n[meta]\nname = "j"\n', {});
  // 同页第二个预览器：独立的胶水模块与线性内存，互不牵连。
  const b = await AutoPreviewer.create({ wasm: wasmBytes });
  check(b.raw.constructor !== a.raw.constructor, '两个预览器各用一份模块');
  b.free();
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

  // 并发调用共用同一次重建。
  a.raw.render = () => {
    throw new WebAssembly.RuntimeError('unreachable');
  };
  const g0 = a.generation;
  await Promise.all([a.render({}), a.render({}), a.signal('typing')]);
  check(a.generation === g0 + 1, `并发调用应只重建一次，实为 ${a.generation - g0}`);

  // 每个实例都崩（持续 panic 的输入）：重建预算用完后永久失败，不再无限重建。
  const { PreviewerFailedError, REBUILD_MAX } = await import(pathToFileURL(join(dir, 'preview.js')).href);
  const doom = (inst) => {
    inst.render = () => {
      throw new WebAssembly.RuntimeError('unreachable');
    };
  };
  const z = await AutoPreviewer.create({ wasm: wasmBytes, onInstance: doom });
  const outcomes = [];
  for (let i = 0; i < 5; i++) {
    try {
      await z.render({});
      outcomes.push('ok');
    } catch (e) {
      outcomes.push(e instanceof PreviewerFailedError ? 'failed' : 'fatal');
    }
  }
  check(z.generation === 1 + REBUILD_MAX, `应只重建 ${REBUILD_MAX} 次，实为 ${z.generation - 1}`);
  check(z.failed && outcomes.at(-1) === 'failed' && !outcomes.includes('ok'), `结局 ${outcomes}`);

  console.log(
    `冒烟通过：横排 ${f.width}x${f.height}、竖排×2 ${v.width}x${v.height}，单帧 ${ms.toFixed(1)} ms，自动重建 OK`,
  );
} catch (e) {
  console.error(e?.stack ?? e);
  process.exitCode = 1;
} finally {
  rmSync(dir, { recursive: true, force: true });
}
