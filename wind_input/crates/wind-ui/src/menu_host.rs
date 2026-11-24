//! 外部宿主（Linux Fcitx5 addon / macOS `.app`）的自绘菜单：主菜单 / 候选右键菜单。
//!
//! **复用 Windows 的 [`PopupMenu`] 整套**（级联状态机、视图树、主题、定位、命中测试、增量重绘），
//! 只换掉两头：
//!
//! - **出**：`PopupMenu` 的窗口在非 Windows 上是只持缓冲区的 mock。每次状态变化后把各级的
//!   像素写进该级自己的 SHM 段（`endpoint::overlay_shm_name`，`_MN0`…），推一帧
//!   `CMD_OVERLAY_FRAME`（kind = `OVERLAY_KIND_MENU + 级`，落位 `EXACT`）交 addon 贴图。
//!   落位在这边算完（工作区由 addon 随「打开菜单」请求报来），addon 原样摆放——命中测试
//!   按这个位置做，两边不能各算各的。
//! - **进**：Windows 菜单窗口自己收鼠标消息；这里改由 addon 抓住指针后把原始事件报上来
//!   （`CMD_MENU_POINTER` → 协调器 → [`UiCommand::MenuPointer`](crate::manager::UiCommand)）。
//!   键盘照旧由协调器 `forward_menu_key` 转发 `MenuKey`。
//!
//! 关闭与 `menu_open` 的对齐见 [`MenuHost::key`] / [`MenuHost::pointer`] 的自愈说明。

use wind_bridge::HostRenderSink;
use wind_bridge::shared_memory_posix::PosixSharedMemory;
use wind_ipc::codec::{OverlayFrameMeta, encode_overlay_frame};
use wind_ipc::protocol::overlay::{OVERLAY_KIND_MENU, OVERLAY_MENU_LEVELS, OVERLAY_PLACE_EXACT};
use wind_ipc::protocol::{MAX_SHARED_RENDER_SIZE, SharedRenderHeader};

use crate::manager::{MenuAnchor, MenuItemSpec, MenuPointerEvent, UiEvent};
use crate::popup_menu::{LevelKey, PopupMenu, set_host_menu_scale, set_host_work_area};

const LEVELS: usize = OVERLAY_MENU_LEVELS as usize;

/// 建 SHM 段的函数。测试注入一个必失败的，覆盖「段建不起来」这条路。
type CreateShm = fn(&str, usize) -> std::io::Result<PosixSharedMemory>;

pub(crate) struct MenuHost {
    menu: Option<PopupMenu>,
    events: std::sync::mpsc::Sender<UiEvent>,
    suffix: String,
    shms: Vec<Option<PosixSharedMemory>>,
    /// 各级上次推给宿主的内容（`None` = 该级没在显示）。
    pushed: Vec<Option<LevelKey>>,
    /// 最近一次指针位置：`MenuAnchor` 的 `i32::MIN` 哨兵（「更多…」弹主菜单）取它，
    /// 对位 Windows 的 `GetCursorPos`。
    last_pointer: Option<(i32, i32)>,
    create_shm: CreateShm,
    /// 宿主坐标单位对应多少菜单像素（见 [`crate::overlay_host::px_per_unit`]）：Linux 恒 1.0；
    /// macOS 是目标屏的缩放。宿主进出的坐标（锚点、工作区、指针、落位）都按它与菜单内部的像素换算。
    ppu: f32,
}

impl MenuHost {
    pub(crate) fn new(events: std::sync::mpsc::Sender<UiEvent>, suffix: String) -> Self {
        let menu = PopupMenu::new(events.clone())
            .map_err(|e| tracing::warn!("菜单渲染器创建失败: {e}"))
            .ok();
        Self {
            menu,
            events,
            suffix,
            shms: (0..LEVELS).map(|_| None).collect(),
            pushed: vec![None; LEVELS],
            last_pointer: None,
            create_shm: PosixSharedMemory::create,
            ppu: 1.0,
        }
    }

    pub(crate) fn set_theme(&mut self, sink: &dyn HostRenderSink, t: &wind_theme::Resolved) {
        if let Some(m) = &mut self.menu {
            m.set_theme(t);
            m.repaint();
        }
        self.sync(sink);
    }

    /// 宿主坐标 → 菜单像素。
    fn px(&self, v: i32) -> i32 {
        (v as f32 * self.ppu).round() as i32
    }

    /// 宿主报来的工作区（宿主坐标）。**同时定下本次菜单的缩放**：取工作区中心点所在屏。
    pub(crate) fn set_work_area(&mut self, left: i32, top: i32, right: i32, bottom: i32) {
        let valid = right > left && bottom > top;
        // 宿主没报出有效工作区（指针落在屏边缘判不中屏等）：缩放退回主屏，不能硬写 1.0——
        // 否则 Retina 上 `ppu` 为 1 而渲染缩放仍按坐标查到 2，位图按 2 倍画、帧里却带 scale=1。
        let scale = if valid {
            crate::dpi::scale_for_point((left + right) / 2, (top + bottom) / 2)
        } else {
            crate::dpi::scale_for_point(0, 0)
        };
        self.set_work_area_scaled(
            (left, top, right, bottom),
            scale,
            crate::overlay_host::px_per_unit(scale),
        );
    }

    /// [`Self::set_work_area`] 的内核：`ppu` 由调用方按平台给出，便于在 Linux 上测点坐标的宿主。
    fn set_work_area_scaled(&mut self, wa: (i32, i32, i32, i32), scale: f32, ppu: f32) {
        let (left, top, right, bottom) = wa;
        self.ppu = ppu;
        // 只有宿主坐标是点的平台才需要覆盖渲染缩放：其余照旧按坐标查。
        set_host_menu_scale((ppu != 1.0).then_some(scale));
        let valid = right > left && bottom > top;
        set_host_work_area(valid.then_some((
            self.px(left),
            self.px(top),
            self.px(right),
            self.px(bottom),
        )));
    }

    pub(crate) fn show(
        &mut self,
        sink: &dyn HostRenderSink,
        items: Vec<MenuItemSpec>,
        anchor: MenuAnchor,
    ) {
        let anchor = if anchor.x == i32::MIN || anchor.y == i32::MIN {
            // 宿主从没报过指针位置（键盘选的「更多…」且此前没动过鼠标）：落到工作区左上，
            // 总比 mock 的 GetCursorPos 给的 (0,0) 可控——两者此时其实同一个点。
            let (x, y) = self.last_pointer.unwrap_or((0, 0));
            MenuAnchor::at_point(self.px(x), self.px(y))
        } else {
            MenuAnchor {
                x: self.px(anchor.x),
                y: self.px(anchor.y),
                right: self.px(anchor.right),
                bottom: self.px(anchor.bottom),
                placement: anchor.placement,
            }
        };
        let Some(m) = &mut self.menu else {
            // 画不出来就当场关掉，否则协调器的 menu_open 一直为真、方向键回车全被吞。
            let _ = self.events.send(UiEvent::MenuClose);
            return;
        };
        m.show(items, anchor);
        self.sync(sink);
    }

    /// 协调器转发的菜单键。
    ///
    /// **自愈**：菜单已经不在屏上（被别的路径收掉了）而协调器还在转发键，说明两边对「菜单开着」
    /// 的认识错开了——回一条 `MenuClose` 让协调器复位，否则 Esc 也关不掉一个看不见的菜单，
    /// 方向键 / 回车 / Esc 会一直被吞，直到超时兜底。
    pub(crate) fn key(&mut self, sink: &dyn HostRenderSink, vk: u32) {
        match &mut self.menu {
            Some(m) if m.is_visible() => m.on_key(vk),
            _ => {
                let _ = self.events.send(UiEvent::MenuClose);
                return;
            }
        }
        self.sync(sink);
    }

    /// 宿主报来的指针事件。自愈同 [`Self::key`]。
    pub(crate) fn pointer(
        &mut self,
        sink: &dyn HostRenderSink,
        event: MenuPointerEvent,
        x: i32,
        y: i32,
    ) {
        self.last_pointer = Some((x, y));
        let (px, py) = (self.px(x), self.px(y));
        match &mut self.menu {
            Some(m) if m.is_visible() => m.on_pointer(event, px, py),
            _ => {
                let _ = self.events.send(UiEvent::MenuClose);
                // 宿主那边可能还挂着上一帧：再补推一遍隐藏帧。
                self.sync(sink);
                return;
            }
        }
        self.sync(sink);
    }

    /// 协调器要求收菜单（`HideMenu`）。收掉的是**可见**菜单时回送 `MenuClose`：协调器有几条路
    /// （候选收起时收候选右键菜单，见 `notify_ui_hide`）是在持 `state` 锁时发的 `HideMenu`，
    /// 那里不能顺手复位 `menu_open`，靠这条回送收口。菜单已自己收起（点了条目 / Esc）时不回送，
    /// 免得把紧接着打开的下一个菜单（「更多…」）关掉；协调器那边重复收到也是幂等的。
    pub(crate) fn hide(&mut self, sink: &dyn HostRenderSink) {
        if let Some(m) = &mut self.menu
            && m.is_visible()
        {
            m.hide();
            let _ = self.events.send(UiEvent::MenuClose);
        }
        self.sync(sink);
    }

    /// 把各级的当前帧与上次推过的比对，变了的写 SHM + 推帧，消失的推隐藏帧。
    ///
    /// 第 0 级推不出去（SHM 段建不起来 / 位图无效）时屏上根本没有菜单，协调器却已标记
    /// `menu_open`：就地收掉菜单并回送 `MenuClose`，否则方向键 / 回车 / Esc 会被一个看不见的
    /// 菜单吞掉，直到空闲超时。子菜单级推不出去不收：顶层还在，键盘照常可用。
    fn sync(&mut self, sink: &dyn HostRenderSink) {
        if self.push_levels(sink) {
            return;
        }
        tracing::warn!("菜单顶层推不出帧，收起菜单");
        if let Some(m) = &mut self.menu {
            m.hide();
        }
        let _ = self.events.send(UiEvent::MenuClose);
        // 此前推过的级（顶层上一帧、子菜单）补推隐藏帧。
        self.push_levels(sink);
    }

    /// [`Self::sync`] 的推帧部分。返回 false = 第 0 级有内容却没推出去。
    fn push_levels(&mut self, sink: &dyn HostRenderSink) -> bool {
        let ppu = self.ppu;
        let mut top_ok = true;
        let frames = self
            .menu
            .as_ref()
            .map(|m| m.level_frames())
            .unwrap_or_default();
        for k in 0..LEVELS {
            let kind = OVERLAY_KIND_MENU + k as u32;
            match frames.get(k) {
                Some(f) => {
                    if self.pushed[k].as_ref() == Some(&f.key) {
                        continue;
                    }
                    let (x, y, w, h) = f.geom;
                    let (hx, hy) = (host_of(x, ppu), host_of(y, ppu));
                    let bytes = (w * h * 4) as usize;
                    if f.buf.len() < bytes
                        || bytes > MAX_SHARED_RENDER_SIZE - SharedRenderHeader::SIZE
                    {
                        tracing::warn!("菜单第 {k} 级位图 {w}x{h} 无效或过大，丢弃");
                        top_ok &= k != 0;
                        continue;
                    }
                    let Some(shm) =
                        ensure_shm(&mut self.shms[k], &self.suffix, kind, self.create_shm)
                    else {
                        top_ok &= k != 0;
                        continue;
                    };
                    let seq = shm.write_frame(0, 0, w, h, &f.buf[..bytes]);
                    let mut flags =
                        SharedRenderHeader::FLAG_VISIBLE | SharedRenderHeader::FLAG_CONTENT_READY;
                    if f.software_shadow {
                        flags |= SharedRenderHeader::FLAG_SOFTWARE_SHADOW;
                    }
                    sink.push_frame(&encode_overlay_frame(&OverlayFrameMeta {
                        kind,
                        seq,
                        width: w,
                        height: h,
                        flags,
                        place: OVERLAY_PLACE_EXACT,
                        x: hx,
                        y: hy,
                        alt_x: hx,
                        alt_y: hy,
                        content_w: w,
                        content_h: h,
                        scale: ppu,
                        ..Default::default()
                    }));
                    self.pushed[k] = Some(f.key.clone());
                }
                None => {
                    if self.pushed[k].take().is_none() {
                        continue;
                    }
                    let seq = self.shms[k].as_mut().map(|s| s.write_hidden()).unwrap_or(0);
                    sink.push_frame(&encode_overlay_frame(&OverlayFrameMeta {
                        kind,
                        seq,
                        ..Default::default()
                    }));
                }
            }
        }
        top_ok
    }
}

/// 菜单像素 → 宿主坐标（不借 `&self`，推帧循环里 `shms` 正被可变借用）。
fn host_of(v: i32, ppu: f32) -> i32 {
    (v as f32 / ppu).round() as i32
}

fn ensure_shm<'a>(
    slot: &'a mut Option<PosixSharedMemory>,
    suffix: &str,
    kind: u32,
    create: CreateShm,
) -> Option<&'a mut PosixSharedMemory> {
    if slot.is_none() {
        let name = wind_bridge::endpoint::overlay_shm_name(suffix, kind)?;
        match create(&name, MAX_SHARED_RENDER_SIZE) {
            Ok(s) => *slot = Some(s),
            Err(e) => {
                tracing::warn!("create menu SHM {name} failed: {e}");
                return None;
            }
        }
    }
    slot.as_mut()
}

#[cfg(test)]
#[path = "menu_host_tests.rs"]
mod tests;
