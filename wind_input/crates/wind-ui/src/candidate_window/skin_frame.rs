//! 候选窗异形皮肤的窗口侧接线：动画时钟、装饰事件（打字 / 上屏 / 翻页 / 中英）与动画开关。
//! 几何求值、布局外扩、绘制、对齐点换算在 [`wind_scene::candidate`]。
//! 设计见 `docs/design/theme-skin-animation.md` §3–§5。

use super::CandidateWindow;
use super::scene::AnimClock;
use wind_theme::skin::DecorEvent;

impl CandidateWindow {
    /// 刷新本帧皮肤几何与装饰动画（见 [`CandidateScene::refresh_skin`]）：由隐藏变可见时
    /// 重读动画开关、记动画起点。
    #[cfg(test)]
    pub(super) fn refresh_skin_geom(&mut self) {
        self.refresh_skin_geom_at(std::time::Instant::now());
    }

    /// 同 [`Self::refresh_skin_geom`]，时刻由调用方给（金样注入时间用）。
    #[cfg(test)]
    pub(super) fn refresh_skin_geom_at(&mut self, now: std::time::Instant) {
        let clock = self.begin_anim_frame(now);
        self.with_scene(|scene, inp| scene.refresh_skin(inp, clock));
    }

    /// 一帧开头的窗口侧动画簿记，返回交给场景的时钟：由隐藏变可见时重读动画开关、记动画起点。
    pub(super) fn begin_anim_frame(&mut self, now: std::time::Instant) -> AnimClock {
        if !self.visible {
            self.refresh_anim_allowed();
            self.anim_start = now;
        }
        // 这一帧就是按此刻重画的，之前登记的「待重绘」作废。
        self.anim_due = false;
        self.anim_clock(now)
    }

    /// 此刻的动画时钟：时间轴起点、可见性与动画开关都在窗口侧。
    fn anim_clock(&self, now: std::time::Instant) -> AnimClock {
        AnimClock {
            t: now.saturating_duration_since(self.anim_start),
            showing: !self.visible,
            allowed: self.anim_allowed,
        }
    }

    /// 重读「播不播动画」：`ui.theme.animation` 且系统没关动画效果。变了就让状态机重来。
    pub(super) fn refresh_anim_allowed(&mut self) {
        let system = (self.motion_allowed)();
        let allowed = self.theme_animation && system;
        // 冻结原因变了记一条：远程桌面等场景系统关了动画效果，看不到动画不是 bug。
        let why = (self.theme_animation, system);
        if self.theme.skin.is_some() && self.anim_reported != Some(why) {
            self.anim_reported = Some(why);
            tracing::info!(
                "skin: 主题动画{}（ui.theme.animation={}，系统动画效果={}）",
                if allowed { "播放" } else { "冻结" },
                why.0,
                if why.1 { "开" } else { "关" }
            );
        }
        if allowed != self.anim_allowed {
            self.anim_allowed = allowed;
            self.scene.decor_anims_key = None;
        }
    }

    /// 喂一个装饰事件；`only` 只给那一块装饰（`click`）。不播动画时不切换。
    /// 显示中画面可能变了就登记一次换帧重绘。
    pub(super) fn skin_event(&mut self, ev: DecorEvent, only: Option<usize>) {
        self.skin_event_at(ev, only, std::time::Instant::now());
    }

    /// 同 [`Self::skin_event`]，时刻由调用方给（金样注入时间用）。
    pub(super) fn skin_event_at(
        &mut self,
        ev: DecorEvent,
        only: Option<usize>,
        now: std::time::Instant,
    ) {
        let clock = self.anim_clock(now);
        let changed = self.with_scene(|scene, inp| scene.decor_event(inp, ev, only, clock));
        if changed && self.visible {
            self.anim_due = true;
        }
    }

    /// 候选更新：比较相邻两次的编码串与页码，喂 `typing` / `backspace` / `page` / `empty`
    /// （`preedit` 取协调器下发的原串，编码由宿主自绘时也照样比）。
    ///
    /// 只有 `composing`（打字中）的帧才算编码：上屏后进联想时编码串变成空串或联想标识，
    /// 那不是退格也不是打字。不在打字中的帧之后再打字，按从空串起打算 `typing`。
    pub fn note_skin_input(
        &mut self,
        preedit: &str,
        composing: bool,
        page: usize,
        no_candidates: bool,
    ) {
        if self.theme.skin.is_none() {
            return;
        }
        let code = if composing { preedit } else { "" };
        let empty = composing && no_candidates;
        let prev = self
            .skin_input_prev
            .replace((code.to_string(), page, empty));
        let (old, old_page, old_empty) = match &prev {
            Some((p, n, e)) => (p.as_str(), Some(*n), *e),
            None => ("", None, false),
        };
        if !composing {
            // 联想 / 上屏后的帧：不报编码事件，只记下来作下一帧的基线。
        } else if code != old {
            let ev = if code.len() < old.len() && old.starts_with(code) {
                DecorEvent::Backspace
            } else {
                DecorEvent::Typing
            };
            self.skin_event(ev, None);
        } else if old_page.is_some_and(|n| n != page) {
            self.skin_event(DecorEvent::Page, None);
        }
        if empty && !old_empty {
            self.skin_event(DecorEvent::Empty, None);
        }
    }

    /// 工具栏状态：中英变了喂 `mode_cn` / `mode_en`（第一次只记下，不算切换）。
    pub fn note_skin_mode(&mut self, chinese: bool) {
        if self.theme.skin.is_none() {
            return;
        }
        if self
            .skin_mode_prev
            .replace(chinese)
            .is_some_and(|c| c != chinese)
        {
            let ev = if chinese {
                DecorEvent::ModeCn
            } else {
                DecorEvent::ModeEn
            };
            self.skin_event(ev, None);
        }
    }

    /// 协调器发来的装饰事件。
    pub fn skin_signal(&mut self, sig: wind_ui_types::SkinSignal) {
        match sig {
            wind_ui_types::SkinSignal::Commit => self.skin_event(DecorEvent::Commit, None),
        }
    }

    /// `ui.theme.animation`。
    pub fn set_theme_animation(&mut self, on: bool) {
        if self.theme_animation != on {
            self.theme_animation = on;
            self.refresh_anim_allowed();
            // 显示中就地换成播放 / 首帧。
            self.anim_due = self.visible;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::scene::{AvoidMask, PlaceAnchor, SlotKind};
    use super::*;
    use wind_theme::skin::{Anchor, DecorAction, DecorClip, DecorHit, Pos9};

    /// 夹具：30×20 窗体图（左上像素红、其余白；x 10..20 / y 8..12 可拉伸）+ 6×6 蓝色装饰，
    /// 装饰挂在窗体右上角外侧（底边右端贴锚点）。
    fn fixture_window(tag: &str) -> CandidateWindow {
        fixture_window_hit(tag, None)
    }

    fn fixture_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wind-skin-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixture_window_hit(tag: &str, hit: Option<&str>) -> CandidateWindow {
        let dir = fixture_dir(tag);
        let decor = dir.join("decor.png");
        image::RgbaImage::from_pixel(6, 6, image::Rgba([0, 0, 255, 255]))
            .save(&decor)
            .unwrap();
        fixture_window_decor(&dir, &decor, hit)
    }

    /// 夹具窗体 + 给定的装饰文件（6×6）。
    fn fixture_window_decor(
        dir: &std::path::Path,
        decor: &std::path::Path,
        hit: Option<&str>,
    ) -> CandidateWindow {
        use wind_theme::skin::{BandSpec, Skin, SkinDecor, SkinFrame, SkinInsets, SkinVariant};
        let frame = dir.join("frame.png");
        let mut img = image::RgbaImage::from_pixel(30, 20, image::Rgba([255, 255, 255, 255]));
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.save(&frame).unwrap();
        let skin = Skin {
            horizontal: Some(SkinVariant {
                frame: Some(SkinFrame {
                    image: frame.to_string_lossy().into_owned(),
                    stretch_x: Some(vec![BandSpec::Pair([10.0, 20.0])]),
                    stretch_y: Some(vec![BandSpec::Pair([8.0, 12.0])]),
                    ..Default::default()
                }),
                content: SkinInsets {
                    left: 4.0,
                    top: 3.0,
                    right: 5.0,
                    bottom: 2.0,
                },
                decor: vec![SkinDecor {
                    image: decor.to_string_lossy().into_owned(),
                    anchor: "frame.top_right".into(),
                    pivot: Some("bottom_right".into()),
                    z: 1,
                    hit: hit.map(Into::into),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let theme = wind_theme::Resolved {
            skin: wind_theme::skin::resolve_skin(&skin).map(Box::new),
            ..Default::default()
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w =
            CandidateWindow::new(super::super::CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_theme(theme);
        w.set_orientation(false, false, false);
        w
    }

    #[test]
    fn skin_sets_min_size_insets_and_decor_margin() {
        let mut w = fixture_window("layout");
        // 只有模式标记：内容远小于窗体最小尺寸。
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.scene.build_tree(&w.scene_input(false));
        let m = w
            .scene
            .skin_layout(&mut root, &w.text_renderer)
            .expect("皮肤生效");
        assert_eq!(m, (0, 6, 0, 0), "装饰伸出窗体上方 6px，只在上边外扩");
        let g = w.scene.skin_geom.as_ref().unwrap();
        // 最小尺寸 = 固定段之和：30-10、20-4。
        let (fx, fy, fw, fh) = g.frame_for_test();
        assert_eq!((fx, fy), (0, 6));
        assert!(fw >= 20 && fh >= 16, "窗体 {fw}x{fh} 不应小于固定段之和");
        assert_eq!(g.decor_rects_for_test(), vec![(fw as i32 - 6, 0, 6, 6)]);
        // 根容器的盒就是窗体：内边距 = 内容框内缩。
        assert_eq!(
            (g.insets.l, g.insets.t, g.insets.r, g.insets.b),
            (4.0, 3.0, 5.0, 2.0)
        );
        assert!(
            w.scene.shadow_params(&w.scene_input(false)).is_none(),
            "皮肤不叠软件阴影"
        );
    }

    #[test]
    fn skin_paints_frame_corner_and_decor() {
        let mut w = fixture_window("paint");
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.scene.build_tree(&w.scene_input(false));
        let (ml, mt, mr, mb) = w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let g = w.scene.skin_geom.as_ref().unwrap();
        let (_, _, fw, fh) = g.frame_for_test();
        let (bw, bh) = (fw + ml + mr, fh + mt + mb);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        g.paint(&mut buf, bw, bh, false, false);
        let px = |x: u32, y: u32| {
            let o = ((y * bw + x) * 4) as usize;
            [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
        };
        // BGRA：窗体左上（固定段，逐像素拷贝）是红；装饰是蓝；装饰左侧的外扩区透明。
        assert_eq!(px(ml, mt), [0, 0, 255, 255]);
        assert_eq!(px(bw - 1, 0), [255, 0, 0, 255]);
        assert_eq!(px(0, 0)[3], 0);
    }

    /// 按 `hit` 配一个挂在窗体右上外侧的装饰，返回「窗体外装饰上的一点是否不起拖」等三个判定。
    fn decor_no_drag(tag: &str, hit: Option<&str>) -> [bool; 3] {
        let mut w = fixture_window_hit(tag, hit);
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.scene.build_tree(&w.scene_input(false));
        w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let g = w.scene.skin_geom.as_ref().unwrap();
        let mut m = w.mouse.borrow_mut();
        m.skin_frame = Some(g.frame_rect());
        m.skin_decor = g.no_drag_rects();
        let (_, _, fw, _) = g.frame_for_test();
        let x = fw as f32 - 3.0;
        [
            m.on_decor_only(x, 2.0),
            m.on_decor_only(x, 10.0),
            m.on_decor_only(1.0, 2.0),
        ]
    }

    #[test]
    fn block_decor_outside_frame_does_not_start_drag() {
        let [decor, in_frame, gap] = decor_no_drag("mouse-block", Some("block"));
        assert!(decor, "窗体外的装饰");
        assert!(!in_frame, "窗体内");
        assert!(!gap, "外扩区里装饰以外的透明处");
    }

    #[test]
    fn decor_drags_window_by_default() {
        assert_eq!(decor_no_drag("mouse-drag", None), [false; 3]);
    }

    #[test]
    fn clamped_content_size_moves_frame_and_decor() {
        let mut w = fixture_window("fit");
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.scene.build_tree(&w.scene_input(false));
        w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let (_, _, fw, fh) = w.scene.skin_geom.as_ref().unwrap().frame_for_test();
        // 渲染入口把宽度钳窄 4px：窗体按钳后宽度画，右上角装饰跟着左移。
        w.scene.skin_fit_frame(fw - 4, fh, &w.text_renderer);
        let g = w.scene.skin_geom.as_ref().unwrap();
        assert_eq!(g.frame_for_test(), (0, 6, fw - 4, fh));
        assert_eq!(g.decor_rects_for_test(), vec![(fw as i32 - 10, 0, 6, 6)]);
    }

    #[test]
    fn svg_frame_falls_back_to_plain_window() {
        let mut w = fixture_window("svg");
        let mut theme = w.theme_source.clone();
        let skin = theme.skin.as_mut().unwrap();
        skin.horizontal.as_mut().unwrap().frame.image = "data:image/svg+xml;base64,PHN2Zy8+".into();
        w.set_theme(theme);
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        assert!(
            w.scene.skin_geom.is_none(),
            "SVG 窗体画不出来，不能当皮肤生效（否则窗口全透明）"
        );
    }

    #[test]
    fn avoid_pushes_opaque_rows_off_the_caret_line() {
        // 光标行 180..200（底端 200，行高 20），gap 2 → 保护带 178..202。
        // 窗体顶上方 30 行：上 20 行透明、下 10 行（-10..0）不透明。
        let mut rows = vec![false; 20];
        rows.extend([true; 10]);
        rows.extend([true; 20]); // 窗体本身 0..20
        let m = AvoidMask::from_rows(-30, &rows);
        // 下方 y=202：不透明的 -10..0 落在 192..202，压线 → 推到 212。透明的那 20 行越过光标行不算。
        assert_eq!(
            CandidateWindow::avoid_caret_line(202, false, 200, 20, Some(&m)),
            212
        );
        // 不透明行本来就在保护带以下：不动。
        let low = AvoidMask::from_rows(0, &[false, false, true]);
        assert_eq!(
            CandidateWindow::avoid_caret_line(200, false, 200, 20, Some(&low)),
            200
        );
        // 上方：窗体 0..20 在 y=160 → 160..180，压到 178 → 上推到底边 ≤ 178。
        let frame = AvoidMask::from_rows(0, &[true; 20]);
        assert_eq!(
            CandidateWindow::avoid_caret_line(160, true, 200, 20, Some(&frame)),
            158
        );
        assert_eq!(
            CandidateWindow::avoid_caret_line(5, false, 200, 20, None),
            5
        );
    }

    #[test]
    fn caret_point_with_offset_moves_window() {
        use wind_theme::skin::{Anchor, CaretPoint};
        let mut w = fixture_window("caret");
        {
            let theme = w.theme_source.clone();
            let mut theme = theme;
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.placement.caret = CaretPoint {
                anchor: Anchor::parse("content.top_left").unwrap(),
                offset: [0.0, -1.0],
            };
            v.placement.avoid_caret = None;
            w.set_theme(theme);
        }
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.scene.build_tree(&w.scene_input(false));
        w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let a = w.scene.skin_place_anchor(1.0, w.scale);
        // 内容框左上 (4, 3) 再上移 1 → (4, 2)；上方缺省窗体左下 → 距底 0。
        assert_eq!(a.below, (4, 2));
        assert_eq!(a.above, (0, 0));
        assert!(
            a.avoid_below.is_none() && a.avoid_above.is_none(),
            "关掉 avoid_caret 不该带保护带"
        );
        // 非 Windows 下 place_window 恒在下方：对齐点落在光标底端 + 2。
        let (x, y, above) = CandidateWindow::place_window(100, 200, 20, 30, 20, false, (0, 0), &a);
        assert_eq!((x, y, above), (96, 200, false));
        // 无皮肤的默认参数：与改前一致（窗体左上贴光标底端 + 2）。
        let d = PlaceAnchor::default();
        assert_eq!(
            CandidateWindow::place_window(100, 200, 20, 30, 20, false, (0, 0), &d),
            (100, 202, false)
        );
    }

    #[test]
    fn default_avoid_sees_decor_pixels_in_strip() {
        let mut w = fixture_window("avoid");
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.scene.build_tree(&w.scene_input(false));
        w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let a = w.scene.skin_place_anchor(1.0, w.scale);
        assert_eq!(a.clamp, (0, 0, 0, 0), "缺省只钳窗体");
        let m = a.avoid_below.as_ref().expect("缺省开避让");
        assert_eq!(m.top, -6, "装饰向上外扩 6px，保护带从那里算起");
        assert!(m.any(-6, 0), "缺省保护带够宽，右上角装饰落在带内");
        // 装饰伸到窗体上方 6px，下方放时压住光标行 → 整窗下推 6。
        let (_, y, _) = CandidateWindow::place_window(100, 200, 20, 30, 20, false, (0, 0), &a);
        assert_eq!(y, 208);
    }

    #[test]
    fn narrow_strip_ignores_decor_far_from_caret() {
        use wind_theme::skin::AvoidStrip;
        let mut w = fixture_window("strip");
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.placement.avoid_caret = Some(AvoidStrip {
                before: 0.0,
                after: 10.0,
            });
            w.set_theme(theme);
        }
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.scene.build_tree(&w.scene_input(false));
        w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let a = w.scene.skin_place_anchor(1.0, w.scale);
        assert!(
            !a.avoid_below.as_ref().unwrap().any(-6, 0),
            "装饰在保护带右边"
        );
        // 光标左侧对齐窗体左缘、带宽 10px：右上角的装饰压不到正在输入的字，窗口不动。
        let (_, y, _) = CandidateWindow::place_window(100, 200, 20, 30, 20, false, (0, 0), &a);
        assert_eq!(y, 202);
    }

    #[test]
    fn pass_decor_is_split_out_of_main_buffer() {
        let mut w = fixture_window("pass");
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.decor[0].hit = DecorHit::Pass;
            w.set_theme(theme);
        }
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.scene.build_tree(&w.scene_input(false));
        let (ml, mt, mr, mb) = w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let g = w.scene.skin_geom.as_ref().unwrap();
        assert!(g.has_pass());
        assert!(!g.pass_below(), "z = 1 的穿透装饰要放在主窗口上面");
        let (_, _, fw, fh) = g.frame_for_test();
        let (bw, bh) = (fw + ml + mr, fh + mt + mb);
        let blank = vec![0u8; (bw * bh * 4) as usize];
        let (mut main, mut pass) = (blank.clone(), blank);
        g.paint(&mut main, bw, bh, false, true);
        g.paint_pass(&mut pass, bw, bh);
        let at = |b: &[u8], x: u32, y: u32| b[((y * bw + x) * 4 + 3) as usize];
        // 装饰在右上角外扩区：主缓冲里是透明（点击会穿过主窗口），穿透层里有像素。
        assert_eq!(at(&main, bw - 1, 0), 0);
        assert_eq!(at(&pass, bw - 1, 0), 255);
        // 窗体只在主缓冲。
        assert_eq!(at(&main, ml, mt), 255);
        assert_eq!(at(&pass, ml, mt), 0);
    }

    fn cand(text: &str) -> wind_ui_types::CandidateItem {
        wind_ui_types::CandidateItem {
            text: text.to_string(),
            text_styled: Default::default(),
            code: String::new(),
            label: String::new(),
            tooltip: Default::default(),
            comment: Default::default(),
            comment_above: Default::default(),
            no_index: false,
        }
    }

    #[test]
    fn free_slots_place_candidates_and_footer_independently() {
        use wind_theme::skin::{RvSlot, RvSlots};
        let mut w = fixture_window("free");
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.slots = Some(RvSlots {
                preedit: None,
                candidates: RvSlot {
                    anchor: Anchor::parse("content.top_left").unwrap(),
                    pivot: Pos9::TOP_LEFT,
                    offset: [0.0, 0.0],
                },
                footer: Some(RvSlot {
                    anchor: Anchor::parse("frame.bottom_right").unwrap(),
                    pivot: Pos9 { h: 2, v: 2 },
                    offset: [-1.0, -1.0],
                }),
            });
            w.set_theme(theme);
        }
        w.update("", 0, "", vec![cand("甲"), cand("乙")], 0, -1, 1, 3);
        w.refresh_skin_geom();
        assert!(w.scene.skin_is_free());
        let mut root = w.scene.build_tree(&w.scene_input(false));
        let (ml, mt, _, _) = w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let g = w.scene.skin_geom.as_ref().unwrap();
        let kinds: Vec<SlotKind> = g.parts_for_test().iter().map(|p| p.0).collect();
        assert_eq!(kinds, vec![SlotKind::Candidates, SlotKind::Footer]);
        let (fx, fy, fw, fh) = g.frame_for_test();
        assert_eq!((fx, fy), (ml as i32, mt as i32));
        let pos = g.part_positions_for_test(fw as f32, fh as f32);
        // 候选槽贴内容框左上（内缩 4, 3）。
        assert_eq!(pos[0], (4.0, 3.0));
        // 翻页槽右下角贴窗体右下、内缩 1。
        let footer = g.parts_for_test()[1];
        assert_eq!(
            (pos[1].0 + footer.1, pos[1].1 + footer.2),
            (fw as f32 - 1.0, fh as f32 - 1.0)
        );
        // 窗体装得下两块：候选在内容框里，翻页不压出窗体。
        assert!(fw as f32 >= 4.0 + g.parts_for_test()[0].1 + 5.0);
        assert!(pos[1].0 >= 0.0 && pos[1].1 >= 0.0);
        // 命中区来自各块，候选可点。
        let mut hits = Vec::new();
        g.collect_part_hits(&mut hits);
        assert!(hits.iter().any(|(tag, _)| *tag == 0), "候选 0 的命中区");
    }

    #[test]
    fn vertical_without_skin_variant_keeps_plain_window() {
        let mut w = fixture_window("vertical");
        w.set_orientation(true, false, false);
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        assert!(w.scene.skin_geom.is_none(), "竖排没配皮肤：不硬套横排图");
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];

    /// 装饰是两帧 GIF（红 100 ms、绿 200 ms）的夹具窗；`motion` 为系统动画开关的桩。
    fn anim_window(tag: &str, motion: fn() -> bool) -> CandidateWindow {
        let dir = fixture_dir(tag);
        let gif = dir.join("decor.gif");
        let bytes = crate::image_cache::anim_fixtures::gif(6, 6, &[(RED, 100), (GREEN, 200)]);
        std::fs::write(&gif, bytes).unwrap();
        let mut w = fixture_window_decor(&dir, &gif, None);
        w.motion_allowed = motion;
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w
    }

    /// 装饰右上角的像素（BGRA）：装饰挂在窗体右上外侧，缓冲右上角就是它。
    fn decor_px(w: &CandidateWindow) -> [u8; 4] {
        let (bw, _) = w.window.size();
        let o = ((bw - 1) * 4) as usize;
        let b = w.window.buffer();
        [b[o], b[o + 1], b[o + 2], b[o + 3]]
    }

    /// 期限在 `now` 之后 `max` 之内。
    fn due_within(d: Option<std::time::Instant>, max: std::time::Duration) -> bool {
        let now = std::time::Instant::now();
        d.is_some_and(|t| t > now && t <= now + max)
    }

    #[test]
    fn visible_animated_decor_schedules_next_frame() {
        let mut w = anim_window("anim-visible", || true);
        w.show();
        assert!(w.is_visible());
        assert!(
            due_within(w.next_deadline(), std::time::Duration::from_millis(100)),
            "首帧 100 ms 后换帧"
        );
        assert_eq!(decor_px(&w), [0, 0, 255, 255], "首帧是红");
    }

    #[test]
    fn hidden_window_does_not_wake_for_animation() {
        let mut w = anim_window("anim-hidden", || true);
        w.show();
        w.hide();
        assert!(
            !due_within(w.next_deadline(), std::time::Duration::from_secs(1)),
            "隐藏后只剩释放像素缓冲那类长期限"
        );
        w.tick();
        assert!(!w.take_anim_due());
    }

    #[test]
    fn static_decor_does_not_schedule_frames() {
        let mut w = fixture_window("anim-static");
        w.motion_allowed = || true;
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.show();
        assert_eq!(w.next_deadline(), None);
    }

    #[test]
    fn reduced_motion_keeps_first_frame_and_never_wakes() {
        let mut w = anim_window("anim-reduced", || false);
        w.show();
        assert_eq!(w.next_deadline(), None);
        assert_eq!(decor_px(&w), [0, 0, 255, 255], "停在首帧");
    }

    #[test]
    fn due_frame_repaints_without_resetting_timeline_or_hover() {
        let mut w = anim_window("anim-repaint", || true);
        w.show();
        // 等到换帧期限，tick 置标记。
        let due = w.next_deadline().expect("动画登记了期限");
        std::thread::sleep(due.saturating_duration_since(std::time::Instant::now()));
        w.tick();
        assert!(w.take_anim_due());
        assert!(!w.take_anim_due(), "标记取走即清");
        let start = w.anim_start;
        w.mouse.borrow_mut().last_hover = 0;
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(w.anim_start, start, "重绘不重置时间轴");
        assert_eq!(w.mouse.borrow().last_hover, 0, "换帧重绘不动悬停去重");
        assert_eq!(decor_px(&w), [0, 255, 0, 255], "第二帧是绿");
        assert!(
            due_within(w.next_deadline(), std::time::Duration::from_millis(200)),
            "第二帧 200 ms 后回到首帧"
        );
        // 普通重绘（内容更新）照旧重置悬停去重。
        w.show();
        assert_eq!(w.mouse.borrow().last_hover, -1);
    }

    #[test]
    fn showing_again_restarts_the_timeline() {
        let mut w = anim_window("anim-restart", || true);
        w.show();
        w.anim_start -= std::time::Duration::from_millis(150);
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(decor_px(&w), [0, 255, 0, 255], "经过 150 ms 在第二帧");
        w.hide();
        w.show();
        assert_eq!(decor_px(&w), [0, 0, 255, 255], "重新显示从首帧开始");
    }

    #[test]
    fn pass_decor_layer_follows_animation_frames() {
        let mut w = anim_window("anim-pass", || true);
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.decor[0].hit = DecorHit::Pass;
            w.set_theme(theme);
        }
        w.show();
        w.anim_start -= std::time::Duration::from_millis(150);
        w.refresh_skin_geom();
        let mut root = w.scene.build_tree(&w.scene_input(false));
        let (ml, mt, mr, mb) = w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let g = w.scene.skin_geom.as_ref().unwrap();
        let (_, _, fw, fh) = g.frame_for_test();
        let (bw, bh) = (fw + ml + mr, fh + mt + mb);
        let mut pass = vec![0u8; (bw * bh * 4) as usize];
        g.paint_pass(&mut pass, bw, bh);
        let o = ((bw - 1) * 4) as usize;
        assert_eq!(&pass[o..o + 4], &[0, 255, 0, 255], "穿透层画当前帧");
    }
    #[test]
    fn avoid_mask_ignores_current_animation_frame() {
        // 首帧不透明、第二帧全透明：保护带只按首帧算，换到第二帧结果不变。
        let dir = fixture_dir("anim-avoid");
        let gif = dir.join("decor.gif");
        let clear = [0, 0, 0, 0];
        let bytes = crate::image_cache::anim_fixtures::gif(6, 6, &[(RED, 100), (clear, 200)]);
        std::fs::write(&gif, bytes).unwrap();
        let mut w = fixture_window_decor(&dir, &gif, None);
        w.motion_allowed = || true;
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.show();
        let mask = |w: &mut CandidateWindow| {
            w.refresh_skin_geom();
            let mut root = w.scene.build_tree(&w.scene_input(false));
            w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
            w.scene.skin_place_anchor(1.0, w.scale).avoid_below
        };
        let first = mask(&mut w);
        w.anim_start -= std::time::Duration::from_millis(150);
        let second = mask(&mut w);
        assert_eq!(
            w.scene.skin_geom.as_ref().unwrap().decor_for_test()[0]
                .cur_for_test()
                .1,
            1,
            "已换到第二帧"
        );
        assert!(first.as_ref().unwrap().any(-6, 0), "首帧装饰压在保护带里");
        assert_eq!(first, second);
    }
    #[test]
    fn single_frame_gif_takes_no_budget_share() {
        let mut w = anim_window("anim-share", || true);
        let one = fixture_dir("anim-share").join("one.gif");
        std::fs::write(
            &one,
            crate::image_cache::anim_fixtures::gif(6, 6, &[(RED, 100)]),
        )
        .unwrap();
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            let mut d = v.decor[0].clone();
            d.image = one.to_string_lossy().into_owned();
            v.decor.push(d);
            w.set_theme(theme);
        }
        w.show();
        let g = w.scene.skin_geom.as_ref().unwrap();
        assert!(g.decor_for_test()[0].sources_animated_for_test()[0]);
        assert!(
            !g.decor_for_test()[1].sources_animated_for_test()[0],
            "单帧 GIF 按静态图画"
        );
        assert_eq!(
            g.decor_for_test()[0].budget_for_test(),
            crate::image_cache::ANIM_BUDGET_BYTES,
            "唯一的动画独占预算"
        );
    }

    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    /// 各色在缓冲里的 BGRA。
    const PX_RED: [u8; 4] = [0, 0, 255, 255];
    const PX_GREEN: [u8; 4] = [0, 255, 0, 255];
    const PX_BLUE: [u8; 4] = [255, 0, 0, 255];
    const PX_WHITE: [u8; 4] = [255, 255, 255, 255];

    use std::time::{Duration, Instant};
    use wind_theme::skin::RvDecorAnim;

    /// 装饰是四帧 GIF（红 / 绿 / 蓝 / 白，各 100 ms），状态 `a` = 0-1、`b` = 2-3（初始 a）。
    fn state_window(tag: &str, on: Vec<(DecorEvent, DecorAction)>) -> CandidateWindow {
        state_window_with(tag, Some(0), on)
    }

    fn state_window_with(
        tag: &str,
        initial: Option<usize>,
        on: Vec<(DecorEvent, DecorAction)>,
    ) -> CandidateWindow {
        let dir = fixture_dir(tag);
        let gif = dir.join("decor.gif");
        let frames = [(RED, 100), (GREEN, 100), (BLUE, 100), (WHITE, 100)];
        std::fs::write(&gif, crate::image_cache::anim_fixtures::gif(6, 6, &frames)).unwrap();
        let mut w = fixture_window_decor(&dir, &gif, None);
        w.motion_allowed = || true;
        let mut theme = w.theme_source.clone();
        let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
        v.decor[0].anim = RvDecorAnim {
            states: vec![
                ("a".into(), DecorClip::Frames { first: 0, last: 1 }),
                ("b".into(), DecorClip::Frames { first: 2, last: 3 }),
            ],
            initial,
            on,
        };
        w.set_theme(theme);
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w
    }

    /// 处理一轮 UI 循环里的换帧重绘（同 manager）。
    fn pump(w: &mut CandidateWindow) {
        w.tick();
        if w.take_anim_due() {
            w.repaint_anim_frame(|w| w.show());
        }
    }

    fn play(first: u32, last: u32, then: Option<usize>) -> DecorAction {
        DecorAction::Play {
            clip: DecorClip::Frames { first, last },
            then,
        }
    }

    #[test]
    fn typing_switches_state_and_repaints() {
        let mut w = state_window(
            "sm-typing",
            vec![(DecorEvent::Typing, DecorAction::State(1))],
        );
        w.show();
        assert_eq!(decor_px(&w), PX_RED, "初始状态 a 的首帧");
        w.note_skin_input("a", true, 1, false);
        assert!(
            w.next_deadline().is_some_and(|d| d <= Instant::now()),
            "切了状态要立即醒来重绘"
        );
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_BLUE, "状态 b 从第 2 帧起");
        w.anim_start -= Duration::from_millis(150);
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(decor_px(&w), PX_WHITE, "b 在 2-3 之间循环");
        w.anim_start -= Duration::from_millis(100);
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(decor_px(&w), PX_BLUE);
    }

    #[test]
    fn input_diff_maps_to_typing_backspace_page_and_empty() {
        use DecorEvent::*;
        let on = vec![
            (Typing, play(1, 1, None)),
            (Backspace, play(2, 2, None)),
            (Page, play(3, 3, None)),
            (Empty, DecorAction::State(1)),
        ];
        let mut w = state_window("sm-diff", on);
        w.show();
        let step = |w: &mut CandidateWindow, pre: &str, page: usize, none: bool| {
            w.note_skin_input(pre, true, page, none);
            pump(w);
            decor_px(w)
        };
        assert_eq!(step(&mut w, "ab", 1, false), PX_GREEN, "typing");
        assert_eq!(step(&mut w, "a", 1, false), PX_BLUE, "前缀变短 = backspace");
        assert_eq!(step(&mut w, "a", 2, false), PX_WHITE, "同编码换页 = page");
        assert_eq!(
            step(&mut w, "ac", 2, false),
            PX_GREEN,
            "改了一个字也是 typing"
        );
        assert_eq!(
            step(&mut w, "acx", 1, true),
            PX_BLUE,
            "空码：typing 之后紧跟 empty，切到 b"
        );
    }

    #[test]
    fn commit_plays_clip_then_lands_on_state() {
        let on = vec![(DecorEvent::Commit, play(3, 3, Some(1)))];
        let mut w = state_window("sm-commit", on);
        w.show();
        w.skin_signal(wind_ui_types::SkinSignal::Commit);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_WHITE, "一次性片段");
        assert!(
            due_within(w.next_deadline(), Duration::from_millis(100)),
            "片段播完的时刻"
        );
        w.anim_start -= Duration::from_millis(100);
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(decor_px(&w), PX_BLUE, "播完转 then = b");
    }

    #[test]
    fn hidden_commit_lands_on_then_for_next_show() {
        let on = vec![
            (DecorEvent::Typing, DecorAction::State(1)),
            (DecorEvent::Commit, play(3, 3, Some(0))),
        ];
        let mut w = state_window("sm-hidden", on);
        w.show();
        w.note_skin_input("a", true, 1, false);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_BLUE);
        w.hide();
        w.skin_signal(wind_ui_types::SkinSignal::Commit);
        assert!(!w.take_anim_due(), "隐藏时不重绘");
        w.show();
        assert_eq!(decor_px(&w), PX_RED, "隐藏时片段丢弃、直接落到 then = a");
    }

    #[test]
    fn mode_switch_ignores_first_toolbar_state() {
        let on = vec![
            (DecorEvent::ModeEn, DecorAction::State(1)),
            (DecorEvent::ModeCn, DecorAction::State(0)),
        ];
        let mut w = state_window("sm-mode", on);
        w.show();
        w.note_skin_mode(false);
        assert!(!w.take_anim_due(), "首次只记下");
        assert_eq!(decor_px(&w), PX_RED);
        w.note_skin_mode(false);
        assert!(!w.take_anim_due(), "没变");
        w.note_skin_mode(true);
        w.note_skin_mode(false);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_BLUE, "切到英文");
    }

    #[test]
    fn idle_seconds_after_last_event() {
        let on = vec![
            (DecorEvent::Typing, DecorAction::State(1)),
            (DecorEvent::Idle(2), DecorAction::State(0)),
        ];
        let mut w = state_window("sm-idle", on);
        w.show();
        w.note_skin_input("a", true, 1, false);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_BLUE);
        w.anim_start -= Duration::from_millis(2050);
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(decor_px(&w), PX_RED, "静默 2 秒回 a");
    }

    #[test]
    fn idle_is_registered_as_wakeup() {
        // 只有 idle 会动：初始状态是单帧。
        let mut w = state_window_with(
            "sm-idle-wake",
            Some(0),
            vec![(DecorEvent::Idle(1), DecorAction::State(1))],
        );
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.decor[0].anim.states[0].1 = DecorClip::Frames { first: 0, last: 0 };
            w.set_theme(theme);
        }
        w.show();
        let d = w.next_deadline().expect("登记了 idle");
        let left = d.saturating_duration_since(Instant::now());
        assert!(left > Duration::from_millis(900) && left <= Duration::from_secs(1));
    }

    #[test]
    fn click_on_decor_within_drag_threshold() {
        let on = vec![(DecorEvent::Click, DecorAction::State(1))];
        let mut w = state_window("sm-click", on);
        w.show();
        let (bw, _) = w.window.size();
        let (x, y) = (bw as f32 - 2.0, 2.0);
        {
            let mut m = w.mouse.borrow_mut();
            m.press_decor(x, y, (100, 100));
            m.release_decor(x, y, (110, 100));
            assert_eq!(m.skin_click, None, "挪过拖动阈值是拖动");
            m.press_decor(x, y, (100, 100));
            m.release_decor(x, y, (103, 97));
            assert_eq!(m.skin_click, Some(0));
        }
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_BLUE);
        let mut m = w.mouse.borrow_mut();
        m.press_decor(5.0, 30.0, (0, 0));
        assert_eq!(m.decor_press, None, "窗体上按下不算装饰");
    }

    #[test]
    fn pass_decor_is_not_clickable() {
        let mut w = state_window("sm-click-pass", vec![]);
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.decor[0].hit = DecorHit::Pass;
            w.set_theme(theme);
        }
        w.show();
        assert!(w.mouse.borrow().skin_click_rects.is_empty());
    }

    #[test]
    fn animation_off_freezes_initial_state_first_frame() {
        let on = vec![(DecorEvent::Typing, DecorAction::State(0))];
        let mut w = state_window_with("sm-off", Some(1), on);
        w.set_theme_animation(false);
        w.show();
        assert_eq!(decor_px(&w), PX_BLUE, "初始状态 b 的首帧（第 2 帧）");
        assert_eq!(w.next_deadline(), None, "不登记换帧");
        w.note_skin_input("a", true, 1, false);
        assert!(!w.take_anim_due(), "事件不切换");
        w.show();
        assert_eq!(decor_px(&w), PX_BLUE);
        w.set_theme_animation(true);
        assert!(w.take_anim_due(), "打开后就地重绘");
        w.repaint_anim_frame(|w| w.show());
        assert!(
            due_within(w.next_deadline(), Duration::from_millis(100)),
            "开始播放"
        );
    }

    #[test]
    fn system_reduced_motion_also_freezes_states() {
        let on = vec![(DecorEvent::Typing, DecorAction::State(0))];
        let mut w = state_window_with("sm-reduced", Some(1), on);
        w.motion_allowed = || false;
        w.show();
        assert_eq!(decor_px(&w), PX_BLUE);
        w.note_skin_input("a", true, 1, false);
        assert!(!w.take_anim_due());
    }

    #[test]
    fn out_of_range_state_is_dropped() {
        let on = vec![(DecorEvent::Typing, DecorAction::State(1))];
        let mut w = state_window("sm-range", on);
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.decor[0].anim.states[1].1 = DecorClip::Frames { first: 2, last: 9 };
            w.set_theme(theme);
        }
        w.show();
        w.note_skin_input("a", true, 1, false);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_RED, "越界的状态当作没写，仍在 a");
    }

    #[test]
    fn missing_image_clip_is_dropped() {
        for (tag, name) in [
            ("sm-miss-rel", "no_such_clip.gif".to_string()),
            (
                "sm-miss-abs",
                fixture_dir("sm-miss-abs")
                    .join("gone.gif")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ] {
            let mut w = state_window(tag, vec![]);
            {
                let mut theme = w.theme_source.clone();
                let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
                let clip = DecorClip::Image(name.clone());
                v.decor[0].anim.on =
                    vec![(DecorEvent::Commit, DecorAction::Play { clip, then: None })];
                w.set_theme(theme);
            }
            w.show();
            {
                let g = w.scene.skin_geom.as_ref().unwrap();
                assert_eq!(
                    g.decor_for_test()[0].sources_animated_for_test().len(),
                    1,
                    "{name}: 缺失文件不进源"
                );
                assert!(
                    !g.decor_for_test()[0].on_for_test()[0],
                    "{name}: 片段不可用"
                );
            }
            w.skin_signal(wind_ui_types::SkinSignal::Commit);
            assert!(!w.take_anim_due(), "{name}: 引用它的动作当作没写");
            assert_eq!(decor_px(&w), PX_RED);
        }
    }

    #[test]
    fn leaving_composition_is_not_backspace() {
        use DecorEvent::*;
        let on = vec![(Typing, play(1, 1, None)), (Backspace, play(2, 2, None))];
        let mut w = state_window("sm-assoc", on);
        w.show();
        w.note_skin_input("ab", true, 1, false);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_GREEN, "typing");
        w.anim_start -= Duration::from_millis(150);
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(decor_px(&w), PX_RED, "片段播完");
        w.note_skin_input("", false, 1, false);
        assert!(!w.take_anim_due(), "上屏后编码清空不是退格");
        w.note_skin_input("联", false, 1, false);
        assert!(!w.take_anim_due(), "联想标识不是打字");
        w.note_skin_input("x", true, 1, false);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_GREEN, "联想之后再打字照常 typing");
    }

    #[test]
    fn image_clip_plays_another_file() {
        let mut w = state_window("sm-image", vec![]);
        let other = fixture_dir("sm-image").join("other.gif");
        let frames = [(GREEN, 100), (WHITE, 100)];
        std::fs::write(
            &other,
            crate::image_cache::anim_fixtures::gif(6, 6, &frames),
        )
        .unwrap();
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            let clip = DecorClip::Image(other.to_string_lossy().into_owned());
            v.decor[0].anim.on = vec![(DecorEvent::Commit, DecorAction::Play { clip, then: None })];
            w.set_theme(theme);
        }
        w.show();
        w.skin_signal(wind_ui_types::SkinSignal::Commit);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_GREEN, "另一个文件的首帧");
        let g = w.scene.skin_geom.as_ref().unwrap();
        assert_eq!(g.decor_for_test()[0].sources_animated_for_test().len(), 2);
        assert_eq!(
            g.decor_for_test()[0].budget_for_test(),
            crate::image_cache::ANIM_BUDGET_BYTES / 2,
            "两个动图文件均分预算"
        );
        w.anim_start -= Duration::from_millis(250);
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(decor_px(&w), PX_RED, "播完回到 a");
    }
}
