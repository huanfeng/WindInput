// 主题编辑器预览的推荐入口：包住 wasm-bindgen 产物（wind_scene.js），出错自动重建。
//
// Rust 侧 panic 在 wasm 上不可恢复（实例状态不可信），此后该实例的所有调用都会报错。这里遇到
// 这类致命错误（WebAssembly.RuntimeError、「已损坏」、wasm-bindgen 的 recursive use）就重新
// 实例化一份模块、新建 Previewer、重放最近一次 setTheme，再把这次调用重试一遍。普通错误
// （主题解析失败、参数超限、画字失败）原样抛出，实例照常可用。
//
// 重建有预算：REBUILD_WINDOW_MS 内至多 REBUILD_MAX 次。每次重建都会留下一份新的模块实例与
// 线性内存（ES 模块不会被卸载），遇到每次都 panic 的输入时不能每帧重建一次把标签页撑爆。超了
// 预算即永久失败：此后每次调用都抛 PreviewerFailedError，编辑器据此退回 Canvas2D 旧实现。
//
// ⚠ 部署：本文件按变量路径动态导入 wind_scene.js（`new URL(变量, import.meta.url)` + `@vite-ignore`），
// 打包器不分析、不改写这条导入。整个产物目录（preview.js、wind_scene.js、wind_scene_bg.wasm、
// snippets/）必须作为静态文件原样发布到同一目录下，不能交给打包器拆分或改名。
//
// 多实例：每个 AutoPreviewer 首次加载起就导入独立 URL 的胶水（`?instance=N`），各持一份模块与
// 线性内存。版本：以 `preview.js?v=<VERSION>` 导入本文件时，胶水与 wasm 的 URL 都带上同一个
// `v`，换版本即换 URL，静态托管可以给 immutable 缓存。（胶水对 snippets/text.js 的静态导入不带 v，
// text.js 的接口变了要连带改名或换目录。）
//
// 用法:
//   import { AutoPreviewer, PreviewerFailedError } from './preview.js';
//   const p = await AutoPreviewer.create();             // 或 create({ wasm: bytes | WebAssembly.Module })
//   const themeWarnings = await p.setTheme(toml, { 'assets/bg.png': u8 }, 'my-theme');
//   const { width, height, rgba, nextMs, nodes, skin, warnings } = await p.render({ t_ms: now, animate: true });
//   // nodes: [{ key, x, y, w, h }]，设备像素（同 rgba 坐标，÷ scale 得逻辑像素）
//   // clamped: 窗口宽过了预览屏（2048 逻辑像素），已按实机截断
//   // skin: 皮肤几何（矩形同 nodes 坐标）；没启用皮肤为 undefined
//   // warnings / themeWarnings: 本次调用期间引擎的 `skin:` 告警原文（string[]）
//   const info = await p.inspectImage(u8);             // { format, width, height, animated, frames: [{ ms }], truncated }
//   const thumbs = await p.decodeFrames(u8, { max_edge: 64, from: 0, to: 16 }); // [{ index, w, h, ms, rgba }]
//   // 帧超过 4096 设备像素时抛「帧过大：…」，调用方降 scale 重画
//   ctx.putImageData(new ImageData(rgba, width, height), 0, 0);
//   await p.signal('typing');
// render 返回的是普通对象，wasm 侧的 Frame 已在这里 free。

const GLUE = './wind_scene.js';
/** 产物版本：本文件 URL 上的 `?v=`（编辑器按 VERSION 加），透传给胶水与 wasm 的 URL，缓存不混版本。 */
const VERSION = new URL(import.meta.url).searchParams.get('v');
/**
 * 全模块的胶水实例计数。每个 AutoPreviewer 的每次（含首次）加载都导入一个独立 URL 的胶水，
 * 各得一份模块与线性内存：同页两个预览器互不牵连，一个崩了不会让另一个（或之后新建的）
 * 拿到已损坏的那份。
 */
let instances = 0;

/** 带版本与实例号的 URL。 */
function versioned(path, instance) {
  const u = new URL(path, import.meta.url);
  if (VERSION) u.searchParams.set('v', VERSION);
  if (instance !== undefined) u.searchParams.set('instance', String(instance));
  return u.href;
}
/** 重建预算：这段时间内… */
export const REBUILD_WINDOW_MS = 10_000;
/** …至多重建这么多次。 */
export const REBUILD_MAX = 2;

/** 重建预算用完后的永久失败：编辑器应退回旧实现。`cause` 是最后一次致命错误。 */
export class PreviewerFailedError extends Error {
  constructor(cause) {
    super(`wasm 预览反复崩溃，已停用：${String(cause?.message ?? cause)}`, { cause });
    this.name = 'PreviewerFailedError';
  }
}

function isFatal(e) {
  if (typeof WebAssembly !== 'undefined' && e instanceof WebAssembly.RuntimeError) return true;
  const msg = String(e?.message ?? e);
  return msg.includes('已损坏') || msg.includes('recursive use');
}

export class AutoPreviewer {
  #module = null; // 编译好的 WebAssembly.Module，重建时复用
  #glue = null;
  #p = null;
  #theme = null; // 最近一次成功的 setTheme 参数
  #rebuilds = []; // 最近各次重建的时刻
  #rebuilding = null; // 进行中的重建；期间到来的调用等它
  #failed = null; // 永久失败的 PreviewerFailedError
  #onInstance = null; // 测试 / 调试用：每次新建 Previewer 后调用
  /** 已实例化的次数（1 = 从没重建过）。 */
  generation = 0;

  /**
   * @param {{ wasm?: WebAssembly.Module | BufferSource | URL | string,
   *           onInstance?: (previewer: object) => void }} [opts]
   *   onInstance：测试 / 调试用，每次（含重建）新建 Previewer 后、重放主题前调用。
   */
  static async create(opts = {}) {
    const self = new AutoPreviewer();
    self.#onInstance = opts.onInstance ?? null;
    let wasm = opts.wasm ?? versioned('./wind_scene_bg.wasm');
    if (!(wasm instanceof WebAssembly.Module)) {
      if (typeof wasm === 'string' || wasm instanceof URL) {
        const res = await fetch(wasm);
        wasm = await WebAssembly.compileStreaming(res);
      } else {
        wasm = await WebAssembly.compile(wasm);
      }
    }
    self.#module = wasm;
    try {
      await self.#load();
    } catch (e) {
      // 首次加载就陷入（如构造 Previewer 时 panic）：同样走受预算约束的重建。
      if (!isFatal(e)) throw e;
      await self.#rebuild(e);
    }
    return self;
  }

  /** 已永久失败（重建预算用完）。 */
  get failed() {
    return this.#failed !== null;
  }

  async #load() {
    try {
      this.#p?.free();
    } catch {
      // 损坏的实例连 free 都可能抛，整份模块随之丢弃。
    }
    this.#p = null;
    // 每次都换实例号：同一 URL 的 ES 模块只实例化一次，换 URL 才拿得到一份全新的模块。
    instances += 1;
    this.#glue = await import(/* @vite-ignore */ versioned(GLUE, instances));
    await this.#glue.default({ module_or_path: this.#module });
    this.generation += 1;
    this.#p = new this.#glue.Previewer();
    this.#onInstance?.(this.#p);
    if (this.#theme) this.#p.setTheme(...this.#theme);
  }

  /** 致命错误后重建一次（串行化：并发的调用共用同一次重建）；预算用完则永久失败。 */
  #rebuild(cause) {
    if (this.#rebuilding) return this.#rebuilding;
    const now = Date.now();
    this.#rebuilds = this.#rebuilds.filter((t) => now - t < REBUILD_WINDOW_MS);
    if (this.#rebuilds.length >= REBUILD_MAX) {
      this.#failed = new PreviewerFailedError(cause);
      this.free();
      return Promise.reject(this.#failed);
    }
    this.#rebuilds.push(now);
    this.#rebuilding = this.#load()
      .catch((e) => {
        this.#failed = new PreviewerFailedError(e);
        throw this.#failed;
      })
      .finally(() => {
        this.#rebuilding = null;
      });
    return this.#rebuilding;
  }

  async #call(fn) {
    if (this.#rebuilding) await this.#rebuilding;
    if (this.#failed) throw this.#failed;
    try {
      return fn(this.#p);
    } catch (e) {
      if (!isFatal(e)) throw e;
      await this.#rebuild(e);
      try {
        return fn(this.#p);
      } catch (e2) {
        if (!isFatal(e2)) throw e2;
        // 重试仍崩：再记一次重建（多半就此用完预算），本次调用以致命错误告终。
        await this.#rebuild(e2);
        throw e2;
      }
    }
  }

  /** 当前的 wasm Previewer（测试 / 调试用）。 */
  get raw() {
    return this.#p;
  }

  /** @returns {Promise<string[]>} 解析期间引擎的 `skin:` 告警 */
  async setTheme(toml, assets = {}, id = undefined) {
    const warnings = await this.#call((p) => p.setTheme(toml, assets, id));
    this.#theme = [toml, assets, id];
    return warnings;
  }

  /** @param {object | string} opts RenderOpts（对象或 JSON 串） */
  async render(opts = {}) {
    const json = typeof opts === 'string' ? opts : JSON.stringify(opts);
    return this.#call((p) => {
      const f = p.render(json);
      try {
        return {
          width: f.width,
          height: f.height,
          rgba: f.rgba,
          nextMs: f.nextMs,
          nodes: JSON.parse(f.nodes),
          clamped: f.clamped,
          skin: f.skin === undefined ? undefined : JSON.parse(f.skin),
          warnings: f.warnings,
        };
      } finally {
        f.free();
      }
    });
  }

  async signal(name, tMs = undefined) {
    return this.#call((p) => p.signal(name, tMs));
  }

  /** 图片帧信息（引擎口径，无状态）。@param {Uint8Array} bytes */
  async inspectImage(bytes) {
    return this.#call(() => JSON.parse(this.#glue.inspectImage(bytes)));
  }

  /**
   * 缩略帧（无状态）。
   * @param {Uint8Array} bytes
   * @param {{ max_edge?: number, from?: number, to?: number }} [opts] `[from, to)` 原始帧号
   */
  async decodeFrames(bytes, opts = {}) {
    return this.#call(() => this.#glue.decodeFrames(bytes, JSON.stringify(opts)));
  }

  free() {
    try {
      this.#p?.free();
    } catch {
      // 同 #load。
    }
    this.#p = null;
  }
}
