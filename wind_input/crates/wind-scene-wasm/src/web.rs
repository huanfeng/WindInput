//! wasm-bindgen 导出层：JS 画布（`js/text.js`）接到 [`Canvas`]，[`Preview`] 包成 JS 类。
//!
//! JS 侧抛的异常经 `catch` 变成 `Err`，只让当前帧失败。Rust 侧的 panic 在 wasm 上不可恢复
//! （`panic = "abort"` 即陷入 `unreachable`，线程局部缓存可能停在借出状态）：入口处的
//! 「调用中」标记在陷入后留着不清，此后本模块里的调用都报「已损坏」，由 `js/preview.js`
//! 的包装重新实例化模块（自动重建）。
//!
//! 注意：陷入那个**同一实例**再被调用时，先撞上的是 wasm-bindgen 自己的借用检查（它的
//! `&mut self` 借用标记也留着），报的是「recursive use of an object detected …」，轮不到这里的
//! 「已损坏」；同模块里新建的实例或别的实例才会报「已损坏」。`preview.js` 两种都认。

use std::cell::Cell;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::canvas::{Canvas, Glyphs};
use crate::preview::{Preview, RenderOpts};

#[wasm_bindgen(module = "/js/text.js")]
extern "C" {
    #[wasm_bindgen(js_name = measure, catch)]
    fn js_measure(text: &str, font: &str) -> Result<js_sys::Float64Array, JsValue>;

    #[wasm_bindgen(js_name = draw, catch)]
    fn js_draw(text: &str, font: &str, x0: i32, x1: i32, ink: u8) -> Result<JsValue, JsValue>;
}

/// 画字结果单边上限（与 text.js 的 `MAX_SIDE` 一致）。
const MAX_GLYPH_SIDE: f64 = 16384.0;

/// 损坏时的报错（`js/preview.js` 按它识别、重建模块）。
const BROKEN: &str = "预览模块已损坏（上次调用中途中止），需重新实例化";

thread_local! {
    /// 正在一次导出调用里。正常返回时清掉；陷入（panic）后留着，说明模块状态不可信。
    static IN_CALL: Cell<bool> = const { Cell::new(false) };
}

/// 一次导出调用的进出标记。
struct CallGuard;

impl CallGuard {
    fn enter() -> Result<Self, JsError> {
        if IN_CALL.with(|c| c.replace(true)) {
            return Err(JsError::new(BROKEN));
        }
        Ok(Self)
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        IN_CALL.with(|c| c.set(false));
    }
}

fn js_err(e: JsValue) -> String {
    e.as_string()
        .or_else(|| {
            js_sys::Reflect::get(&e, &JsValue::from_str("message"))
                .ok()?
                .as_string()
        })
        .unwrap_or_else(|| "JS 异常".to_string())
}

struct JsCanvas;

fn field(obj: &JsValue, name: &str) -> Result<JsValue, String> {
    js_sys::Reflect::get(obj, &JsValue::from_str(name)).map_err(js_err)
}

/// 画字结果的整数字段：有限、在 `[lo, MAX_GLYPH_SIDE]` 内。
fn int_field(obj: &JsValue, name: &str, lo: f64) -> Result<f64, String> {
    let v = field(obj, name)?
        .as_f64()
        .ok_or_else(|| format!("画字结果缺 {name}"))?;
    if !(v.is_finite() && (lo..=MAX_GLYPH_SIDE).contains(&v)) {
        return Err(format!("画字结果 {name} = {v} 越界"));
    }
    Ok(v)
}

impl Canvas for JsCanvas {
    fn measure(&self, text: &str, font: &str) -> Result<(f32, f32), String> {
        let m = js_measure(text, font).map_err(js_err)?;
        if m.length() < 2 {
            return Err("测量结果不足两项".into());
        }
        Ok((m.get_index(0) as f32, m.get_index(1) as f32))
    }

    fn draw(&self, text: &str, font: &str, x0: i32, x1: i32, ink: u8) -> Result<Glyphs, String> {
        let o = js_draw(text, font, x0, x1, ink).map_err(js_err)?;
        let pixels = field(&o, "pixels")?
            .dyn_into::<js_sys::Uint8ClampedArray>()
            .map_err(|_| "画字结果的 pixels 不是 Uint8ClampedArray".to_string())?;
        Ok(Glyphs {
            x: int_field(&o, "x", -MAX_GLYPH_SIDE * 1e4)? as i32,
            y: int_field(&o, "y", -MAX_GLYPH_SIDE)? as i32,
            w: int_field(&o, "w", 0.0)? as u32,
            h: int_field(&o, "h", 0.0)? as u32,
            rgba: pixels.to_vec(),
        })
    }
}

/// 一帧：`new ImageData(frame.rgba, frame.width, frame.height)` 即可上画布。
/// 它占着 wasm 内存，用完调 `frame.free()`（`js/preview.js` 的包装已代办）。
#[wasm_bindgen]
pub struct Frame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    next_ms: Option<f64>,
}

#[wasm_bindgen]
impl Frame {
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 直通 alpha 的 RGBA（每次取都复制一份）。
    #[wasm_bindgen(getter)]
    pub fn rgba(&self) -> js_sys::Uint8ClampedArray {
        js_sys::Uint8ClampedArray::from(&self.rgba[..])
    }

    /// 下一次换帧时刻（毫秒，同 `t_ms` 的时间轴）；undefined = 画面静止。
    #[wasm_bindgen(getter, js_name = nextMs)]
    pub fn next_ms(&self) -> Option<f64> {
        self.next_ms
    }
}

/// 候选窗预览器。同页可建多个，各持主题、资源与缓存。
#[wasm_bindgen]
pub struct Previewer {
    inner: Preview<JsCanvas>,
}

#[wasm_bindgen]
impl Previewer {
    /// 预览内置 `default` 主题。模块已损坏时抛错。
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<Previewer, JsError> {
        let _g = CallGuard::enter()?;
        Ok(Previewer {
            inner: Preview::new(JsCanvas),
        })
    }

    /// 换主题：`toml` 是 theme.toml 文本；`assets` 是 `{ "assets/a.png": Uint8Array | ArrayBuffer }`
    /// （可省）；`id` 是主题自己的 id（可省，`base` 写成它视同没写）。解析失败抛 Error，message
    /// 即解析错误原文；当前主题不变。资源内容没变时不重新解码。
    #[wasm_bindgen(js_name = setTheme)]
    pub fn set_theme(
        &mut self,
        toml: &str,
        assets: JsValue,
        id: Option<String>,
    ) -> Result<(), JsError> {
        let _g = CallGuard::enter()?;
        let mut list = Vec::new();
        if !assets.is_undefined() && !assets.is_null() {
            let obj: js_sys::Object = assets
                .dyn_into()
                .map_err(|_| JsError::new("assets 应是对象：{ 路径: Uint8Array }"))?;
            for entry in js_sys::Object::entries(&obj).iter() {
                let pair: js_sys::Array = entry.unchecked_into();
                let path = pair.get(0).as_string().unwrap_or_default();
                let v = pair.get(1);
                let bytes = if let Some(b) = v.dyn_ref::<js_sys::Uint8Array>() {
                    b.to_vec()
                } else if let Some(b) = v.dyn_ref::<js_sys::ArrayBuffer>() {
                    js_sys::Uint8Array::new(b).to_vec()
                } else {
                    return Err(JsError::new(&format!(
                        "资源 {path:?} 应是 Uint8Array 或 ArrayBuffer"
                    )));
                };
                list.push((path, bytes));
            }
        }
        self.inner
            .set_theme(toml, id.as_deref().unwrap_or(""), list)
            .map_err(|e| JsError::new(&e))
    }

    /// 画一帧。`opts` 是 JSON 串（字段见 `RenderOpts`，全可省；`"{}"` 即默认示例）。出错只让
    /// 这一帧失败。
    pub fn render(&mut self, opts: &str) -> Result<Frame, JsError> {
        let _g = CallGuard::enter()?;
        let o: RenderOpts = serde_json::from_str(if opts.trim().is_empty() { "{}" } else { opts })
            .map_err(|e| JsError::new(&format!("opts 解析失败：{e}")))?;
        let f = self.inner.render(&o).map_err(|e| JsError::new(&e))?;
        Ok(Frame {
            width: f.width,
            height: f.height,
            rgba: f.rgba,
            next_ms: f.next_ms,
        })
    }

    /// 喂装饰事件（`typing` / `commit` / `click:0` / `show` …），`tMs` 缺省取上一帧时刻。
    /// 返回画面是否可能变了。
    pub fn signal(&mut self, name: &str, t_ms: Option<f64>) -> Result<bool, JsError> {
        let _g = CallGuard::enter()?;
        self.inner.signal(name, t_ms).map_err(|e| JsError::new(&e))
    }
}
