//! wasm-bindgen 导出层：JS 画布（`js/text.js`）接到 [`Canvas`]，[`Preview`] 包成 JS 类。

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::canvas::{Canvas, Glyphs};
use crate::preview::{Preview, RenderOpts};

#[wasm_bindgen(module = "/js/text.js")]
extern "C" {
    #[wasm_bindgen(js_name = measure)]
    fn js_measure(text: &str, font: &str) -> js_sys::Float64Array;

    #[wasm_bindgen(js_name = draw)]
    fn js_draw(text: &str, font: &str, r: u8, g: u8, b: u8, a: u8) -> JsValue;
}

struct JsCanvas;

fn field(obj: &JsValue, name: &str) -> Option<JsValue> {
    js_sys::Reflect::get(obj, &JsValue::from_str(name)).ok()
}

impl Canvas for JsCanvas {
    fn measure(&self, text: &str, font: &str) -> (f32, f32) {
        let m = js_measure(text, font);
        (m.get_index(0) as f32, m.get_index(1) as f32)
    }

    fn draw(&self, text: &str, font: &str, [r, g, b, a]: [u8; 4]) -> Option<Glyphs> {
        let o = js_draw(text, font, r, g, b, a);
        let num = |k: &str| field(&o, k).and_then(|v| v.as_f64());
        let pixels = field(&o, "pixels")?
            .dyn_into::<js_sys::Uint8ClampedArray>()
            .ok()?;
        Some(Glyphs {
            x: num("x")? as i32,
            y: num("y")? as i32,
            w: num("w")? as u32,
            h: num("h")? as u32,
            rgba: pixels.to_vec(),
        })
    }
}

/// 一帧：`new ImageData(frame.rgba, frame.width, frame.height)` 即可上画布。
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

/// 候选窗预览器。
#[wasm_bindgen]
pub struct Previewer {
    inner: Preview<JsCanvas>,
}

#[wasm_bindgen]
impl Previewer {
    /// 预览内置 `default` 主题。
    #[wasm_bindgen(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Previewer {
        Previewer {
            inner: Preview::new(JsCanvas),
        }
    }

    /// 换主题：`toml` 是 theme.toml 文本，`assets` 是 `{ "assets/a.png": Uint8Array | ArrayBuffer }`
    /// （可省）。解析失败抛 Error，message 即解析错误原文；当前主题不变。
    #[wasm_bindgen(js_name = setTheme)]
    pub fn set_theme(&mut self, toml: &str, assets: JsValue) -> Result<(), JsError> {
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
            .set_theme(toml, list)
            .map_err(|e| JsError::new(&e))
    }

    /// 画一帧。`opts` 是 JSON 串（字段见 `RenderOpts`，全可省；`"{}"` 即默认示例）。
    pub fn render(&mut self, opts: &str) -> Result<Frame, JsError> {
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
        self.inner.signal(name, t_ms).map_err(|e| JsError::new(&e))
    }
}
