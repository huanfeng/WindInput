// 主题编辑器预览的推荐入口：包住 wasm-bindgen 产物（wind_scene.js），出错自动重建。
//
// Rust 侧 panic 在 wasm 上不可恢复（实例状态不可信），此后该实例的所有调用都会报错。这里遇到
// 这类致命错误（WebAssembly.RuntimeError、「已损坏」、wasm-bindgen 的 recursive use）就重新
// 实例化一份模块、新建 Previewer、重放最近一次 setTheme，再把这次调用重试一遍。普通错误
// （主题解析失败、参数超限、画字失败）原样抛出，实例照常可用。
//
// 用法:
//   import { AutoPreviewer } from './preview.js';
//   const p = await AutoPreviewer.create();             // 或 create({ wasm: bytes | WebAssembly.Module })
//   await p.setTheme(toml, { 'assets/bg.png': u8 }, 'my-theme');
//   const { width, height, rgba, nextMs } = await p.render({ t_ms: now, animate: true });
//   ctx.putImageData(new ImageData(rgba, width, height), 0, 0);
//   await p.signal('typing');
// render 返回的是普通对象，wasm 侧的 Frame 已在这里 free。

const GLUE = './wind_scene.js';

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
  /** 已实例化的次数（1 = 从没重建过）。 */
  generation = 0;

  /** @param {{ wasm?: WebAssembly.Module | BufferSource | URL | string }} [opts] */
  static async create(opts = {}) {
    const self = new AutoPreviewer();
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

  async #load() {
    try {
      this.#p?.free();
    } catch {
      // 损坏的实例连 free 都可能抛，整份模块随之丢弃。
    }
    this.#p = null;
    // 第一次按原路径导入（打包器认得）；重建时加查询串，拿一份全新的模块实例。
    const url = this.generation === 0 ? GLUE : `${GLUE}?instance=${this.generation + 1}`;
    this.#glue = await import(/* @vite-ignore */ new URL(url, import.meta.url).href);
    await this.#glue.default({ module_or_path: this.#module });
    this.generation += 1;
    this.#p = new this.#glue.Previewer();
    if (this.#theme) this.#p.setTheme(...this.#theme);
  }

  async #call(fn) {
    try {
      return fn(this.#p);
    } catch (e) {
      if (!isFatal(e)) throw e;
      await this.#load();
      return fn(this.#p);
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
        return { width: f.width, height: f.height, rgba: f.rgba, nextMs: f.nextMs };
      } finally {
        f.free();
      }
    });
  }

  async signal(name, tMs = undefined) {
    return this.#call((p) => p.signal(name, tMs));
  }

  free() {
    this.#p?.free?.();
    this.#p = null;
  }
}
