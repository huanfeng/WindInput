use super::{Canvas, Glyphs};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// 一次画字调用：(文字, 字体串, 横坐标段)。
pub(crate) type DrawCall = (String, String, (i32, i32));

pub(crate) const EMOJI_RGB: [u8; 3] = [255, 160, 0];

#[derive(Clone, Default)]
pub(crate) struct MockCanvas {
    pub(crate) measures: Rc<RefCell<Vec<String>>>,
    pub(crate) draws: Rc<RefCell<Vec<DrawCall>>>,
    pub(crate) fail: Rc<Cell<bool>>,
}

/// CSS 字体串里的字号。
fn size_of(font: &str) -> f32 {
    font.split_whitespace()
        .find_map(|t| t.strip_suffix("px")?.parse().ok())
        .unwrap_or(16.0)
}

impl Canvas for MockCanvas {
    fn measure(&self, text: &str, font: &str) -> Result<(f32, f32), String> {
        if self.fail.get() {
            return Err("mock measure 抛异常".into());
        }
        self.measures.borrow_mut().push(font.to_string());
        let s = size_of(font);
        Ok((text.chars().count() as f32 * s * 0.6, s * 1.2))
    }

    fn draw(&self, text: &str, font: &str, x0: i32, x1: i32, ink: u8) -> Result<Glyphs, String> {
        if self.fail.get() {
            return Err("mock draw 抛异常".into());
        }
        self.draws
            .borrow_mut()
            .push((text.to_string(), font.to_string(), (x0, x1)));
        let s = size_of(font);
        let cell = s * 0.6;
        let full = (text.chars().count() as f32 * cell).ceil() as i32;
        let (a, b) = (x0.max(0), x1.min(full));
        let (w, h) = ((b - a).max(0) as u32, (s * 1.2).ceil() as u32);
        let chars: Vec<char> = text.chars().collect();
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..h {
            for x in a..a + w as i32 {
                let i = (x as f32 / cell) as usize;
                match chars.get(i).copied() {
                    // 彩色字形：左半格橙、右半格纯白，都不随 ink 变。
                    Some('😀') if (x as f32) < (i as f32 + 0.5) * cell => {
                        rgba.extend_from_slice(&[EMOJI_RGB[0], EMOJI_RGB[1], EMOJI_RGB[2], 255])
                    }
                    Some('😀') => rgba.extend_from_slice(&[255, 255, 255, 255]),
                    _ => rgba.extend_from_slice(&[ink, ink, ink, 255]),
                }
            }
        }
        Ok(Glyphs {
            x: a,
            y: 0,
            w,
            h,
            rgba,
        })
    }
}
