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
// 用法:
//   import { AutoPreviewer, PreviewerFailedError } from './preview.js';
//   const p = await AutoPreviewer.create();             // 或 create({ wasm: bytes | WebAssembly.Module })
//   await p.setTheme(toml, { 'assets/bg.png': u8 }, 'my-theme');
//   const { width, height, rgba, nextMs, nodes } = await p.render({ t_ms: now, animate: true });
//   // nodes: [{ key, x, y, w, h }]，设备像素（同 rgba 坐标，÷ scale 得逻辑像素）
//   ctx.putImageData(new ImageData(rgba, width, height), 0, 0);
//   await p.signal('typing');
// render 返回的是普通对象，wasm 侧的 Frame 已在这里 free。

const GLUE = './wind_scene.js';
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
    let wasm = opts.wasm ?? new URL('./wind_scene_bg.wasm', import.meta.url);
    if (!(wasm instanceof WebAssembly.Module)) {
      if (typeof wasm === 'string' || wasm instanceof URL) {
        const res = await fetch(wasm);
        wasm = await WebAssembly.compileStreaming(res);
      } else {
        wasm = await WebAssembly.compile(wasm);
      }
    }
    self.#module = wasm;
    await self.#load();
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
    // 重建时加查询串：同一 URL 的 ES 模块只实例化一次，换 URL 才拿得到一份全新的模块。
    const url = this.generation === 0 ? GLUE : `${GLUE}?instance=${this.generation + 1}`;
    this.#glue = await import(/* @vite-ignore */ new URL(url, import.meta.url).href);
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

  async setTheme(toml, assets = {}, id = undefined) {
    await this.#call((p) => p.setTheme(toml, assets, id));
    this.#theme = [toml, assets, id];
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
        };
      } finally {
        f.free();
      }
    });
  }

  async signal(name, tMs = undefined) {
    return this.#call((p) => p.signal(name, tMs));
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
