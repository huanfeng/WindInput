//! HostRenderManager（Windows）：全局 SHM per kind + SetupSeq 守卫 + hide 必达
//!
//! 对齐 Go `internal/bridge/host_render.go`，根治「单进程多实例致候选窗不隐藏」bug。

#![cfg(windows)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, bail};
use tracing::info;
use tracing::warn;
use wind_ipc::protocol::{
    HOST_RENDER_CAP_PASS_LAYER, HOST_WINDOW_CANDIDATE, HOST_WINDOW_KIND_COUNT, HOST_WINDOW_STATUS,
    HOST_WINDOW_TOOLTIP, HostRenderSetupEntry, MAX_SHARED_RENDER_SIZE,
};

use crate::named_event::NamedEvent;
use crate::shared_memory_windows::WindowsSharedMemory;
use crate::shared_render_frame::FrameParams;

// kind → SHM/Event 名称后缀
const KIND_SUFFIXES: [&str; HOST_WINDOW_KIND_COUNT] = ["", "_TIP", "_STS"];
const ALL_KINDS: [u32; HOST_WINDOW_KIND_COUNT] = [
    HOST_WINDOW_CANDIDATE,
    HOST_WINDOW_TOOLTIP,
    HOST_WINDOW_STATUS,
];

/// HostRender 渲染目标（活跃实例）
#[derive(Clone, Debug)]
pub struct HostRenderTarget {
    pub pid: u32,
    pub instance_id: u32, // == conn_id（由服务器从 1 起分配）
    /// 该实例的 DLL 声明能画穿透装饰层（`HOST_RENDER_CAP_PASS_LAYER`）。旧 DLL 不声明 → 合成成单层发。
    pub pass_layer: bool,
}

struct ClientState {
    pid: u32,
    setup_seq: u64,
    events: HashMap<u32, Arc<NamedEvent>>, // kind → 私有 Event
    caps: u32,                             // HOST_RENDER_CAP_*（请求载荷里带的）
}

struct Inner {
    shms: HashMap<u32, WindowsSharedMemory>, // kind → 全局段（懒建）
    clients: HashMap<u32, ClientState>,      // conn_id → 状态
    setup_seq: u64,                          // 单调递增
    visible_owner: HashMap<u32, (u32, u32)>, // kind → (pid, conn_id)
    whitelist: Vec<String>,                  // 进程名通配模式（大小写不敏感）
    active: Option<(u32, u32)>,              // (conn_id, pid) 最近焦点
}

pub struct HostRenderManager {
    inner: Mutex<Inner>,
    suffix: String,
}

// HostRenderManager 包含 Mutex<Inner>，Inner 中字段均实现 Send，
// 故 HostRenderManager 自动推导为 Send + Sync，无需手写 unsafe impl。

// ---------- 命名辅助 ----------

fn shm_name_for(suffix: &str, kind: u32) -> String {
    format!(
        "Local\\WindInput_SHM{}{}",
        suffix, KIND_SUFFIXES[kind as usize]
    )
}

fn event_name_for(suffix: &str, conn_id: u32, kind: u32) -> String {
    format!(
        "Local\\WindInput_EVT{}_C{}{}",
        suffix, conn_id, KIND_SUFFIXES[kind as usize]
    )
}

// ---------- 通配符匹配（`*`/`?`，大小写不敏感，DP O(m·n)） ----------

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (m, n) = (p.len(), t.len());
    // dp[i][j] = p[..i] 能匹配 t[..j]
    let mut dp = vec![vec![false; n + 1]; m + 1];
    dp[0][0] = true;
    for i in 1..=m {
        if p[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if p[i - 1] == '*' {
                dp[i - 1][j] || dp[i][j - 1]
            } else if p[i - 1] == '?' || p[i - 1] == t[j - 1] {
                dp[i - 1][j - 1]
            } else {
                false
            };
        }
    }
    dp[m][n]
}

// ---------- 进程名查询（锁外调用，失败返回 None） ----------

fn query_process_filename(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()? };

    let mut buf = [0u16; 1024];
    let mut size = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };
    unsafe {
        let _ = CloseHandle(handle);
    }
    if ok.is_ok() {
        let path = String::from_utf16_lossy(&buf[..size as usize]);
        std::path::Path::new(&path)
            .file_name()
            .and_then(|f| f.to_str())
            .map(|s| s.to_lowercase())
    } else {
        None
    }
}

// ---------- HostRenderManager ----------

impl HostRenderManager {
    pub fn new(suffix: &str, whitelist: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                shms: HashMap::new(),
                clients: HashMap::new(),
                setup_seq: 0,
                visible_owner: HashMap::new(),
                whitelist,
                active: None,
            }),
            suffix: suffix.to_owned(),
        })
    }

    /// 热更新白名单（已 setup 的 client 不受影响，下次焦点评估自然生效）
    pub fn set_whitelist(&self, patterns: Vec<String>) {
        self.inner.lock().unwrap().whitelist = patterns;
    }

    /// pid 是否在白名单中（QueryFullProcessImageNameW 在锁外执行）
    pub fn is_process_whitelisted(&self, pid: u32) -> bool {
        let patterns = self.inner.lock().unwrap().whitelist.clone();
        let Some(name) = query_process_filename(pid) else {
            warn!("is_process_whitelisted: 无法查询进程名 pid={pid}");
            return false;
        };
        patterns.iter().any(|p| wildcard_match(p, &name))
    }

    /// 注册 HostRender 实例。
    ///
    /// 返回 `(instance_id, entries)`；`instance_id == conn_id`。
    /// 未命中白名单或 `conn_id == 0` 返回 `Err`。
    pub fn setup(
        &self,
        conn_id: u32,
        pid: u32,
        caps: u32,
    ) -> anyhow::Result<(u32, Vec<HostRenderSetupEntry>)> {
        if conn_id == 0 {
            bail!("setup: conn_id 必须 >= 1（0 保留为广播 target）");
        }
        if !self.is_process_whitelisted(pid) {
            bail!("setup: pid={pid} 不在 host-render 白名单中");
        }

        let mut inner = self.inner.lock().unwrap();
        inner.setup_seq += 1;
        let seq = inner.setup_seq;

        // 丢弃旧 events（同 conn_id 重连）
        inner.clients.remove(&conn_id);

        let mut events: HashMap<u32, Arc<NamedEvent>> = HashMap::with_capacity(3);
        let mut entries: Vec<HostRenderSetupEntry> = Vec::with_capacity(3);

        for &kind in &ALL_KINDS {
            // 懒建全局 SHM
            if let std::collections::hash_map::Entry::Vacant(e) = inner.shms.entry(kind) {
                let name = shm_name_for(&self.suffix, kind);
                let shm = WindowsSharedMemory::create(&name, MAX_SHARED_RENDER_SIZE)
                    .with_context(|| format!("create SHM kind={kind}"))?;
                e.insert(shm);
            }
            let shm_name = inner.shms[&kind].name().to_owned();

            // 新建私有 Event
            let evt_name = event_name_for(&self.suffix, conn_id, kind);
            let evt = NamedEvent::create(&evt_name)
                .with_context(|| format!("create Event kind={kind} conn_id={conn_id}"))?;
            events.insert(kind, Arc::new(evt));

            entries.push(HostRenderSetupEntry {
                window_kind: kind,
                max_buffer_size: MAX_SHARED_RENDER_SIZE as u32,
                shm_name,
                event_name: evt_name,
            });
        }

        inner.clients.insert(
            conn_id,
            ClientState {
                pid,
                setup_seq: seq,
                events,
                caps,
            },
        );
        info!("host_render setup: conn_id={conn_id} pid={pid} seq={seq}");
        Ok((conn_id, entries))
    }

    /// 记录最近焦点实例（焦点/激活时调）。
    ///
    /// **焦点抖动免疫不变量**：本方法仅在 FOCUS_GAINED / IME_ACTIVATED 时调用，
    /// FOCUS_LOST **不清除** active。原因：同进程内 SearchHost.exe 等受限宿主会在
    /// FOCUS_LOST 后立即发出 FOCUS_GAINED（焦点抖动），两次事件之间若清除 active
    /// 会导致写帧目标短暂丢失；旧 active 保留至下一次 GAINED 覆盖是安全的，
    /// 因为写帧路径（coordinator 的 notify_ui_update / notify_ui_hide）仅在候选可见
    /// 时才读取 active_target()，不可见期间的滞留目标不会被错误写帧。
    /// 此语义由 server.rs CMD_FOCUS_LOST 分支不调用 note_focus 保证。
    pub fn note_focus(&self, conn_id: u32, pid: u32) {
        self.inner.lock().unwrap().active = Some((conn_id, pid));
    }

    // 注意：不要基于全局 active 槽计算 hostRenderAvail 位（曾有 focused_whitelisted()，
    // 已删除）。avail 位必须按「事件源客户端的 PID」查白名单（对齐 Go
    // PushActivationStatusToActiveClient(status, processID)），否则兄弟进程
    // （StartMenuExperienceHost 等）的激活事件会污染推送、触发 DLL 销毁重建循环。

    /// 活跃实例（已 setup 才返回 Some）
    pub fn active_target(&self) -> Option<HostRenderTarget> {
        let inner = self.inner.lock().unwrap();
        let (conn_id, pid) = inner.active?;
        let cs = inner.clients.get(&conn_id)?;
        Some(HostRenderTarget {
            pid,
            instance_id: conn_id,
            pass_layer: cs.caps & HOST_RENDER_CAP_PASS_LAYER != 0,
        })
    }

    /// 写帧到全局 SHM，登记 visible_owner，唤醒目标 pid 全部实例（锁外 SetEvent）
    pub fn write_frame_for_kind(
        &self,
        kind: u32,
        target: &HostRenderTarget,
        p: &FrameParams,
    ) -> anyhow::Result<()> {
        let events_to_signal: Vec<Arc<NamedEvent>> = {
            let mut inner = self.inner.lock().unwrap();

            // 懒建全局 SHM
            if let std::collections::hash_map::Entry::Vacant(e) = inner.shms.entry(kind) {
                let name = shm_name_for(&self.suffix, kind);
                let shm = WindowsSharedMemory::create(&name, MAX_SHARED_RENDER_SIZE)
                    .with_context(|| format!("create SHM kind={kind}"))?;
                e.insert(shm);
            }

            // 以 target.instance_id 覆盖 p.target_instance_id
            let shm = inner.shms.get_mut(&kind).unwrap();
            let modified = FrameParams {
                sequence: p.sequence,
                x: p.x,
                y: p.y,
                width: p.width,
                height: p.height,
                bgra: p.bgra,
                rects: p.rects,
                rendered_hover_index: p.rendered_hover_index,
                target_instance_id: target.instance_id,
                software_shadow: p.software_shadow,
                pass: p.pass,
            };
            shm.write_frame(&modified)
                .map_err(|_| anyhow::anyhow!("write_frame_for_kind: 帧过大 kind={kind}"))?;

            // 登记 visible_owner
            inner
                .visible_owner
                .insert(kind, (target.pid, target.instance_id));

            // 收集目标 pid 全部实例该 kind 的 event（锁内克隆 Arc，锁外 signal）
            inner
                .clients
                .iter()
                .filter(|(_, cs)| cs.pid == target.pid)
                .filter_map(|(_, cs)| cs.events.get(&kind).cloned())
                .collect()
        };

        for evt in events_to_signal {
            evt.signal();
        }
        Ok(())
    }

    /// hide 必达：不查白名单/评估态，只查 visible_owner。
    ///
    /// - 存在 owner → write_hidden(0)（target=0 广播隐藏）→ 唤醒 owner_pid 全部实例 → 清 owner
    /// - 不存在 → no-op
    pub fn hide_kind(&self, kind: u32) {
        let events_to_signal: Vec<Arc<NamedEvent>> = {
            let mut inner = self.inner.lock().unwrap();

            let Some((owner_pid, _)) = inner.visible_owner.remove(&kind) else {
                return; // no-op
            };

            // write_hidden(0)：flags=0，target_instance_id=0 → 广播隐藏
            if let Some(shm) = inner.shms.get_mut(&kind) {
                shm.write_hidden(0);
            }

            // 收集 owner_pid 全部实例该 kind 的 event
            inner
                .clients
                .iter()
                .filter(|(_, cs)| cs.pid == owner_pid)
                .filter_map(|(_, cs)| cs.events.get(&kind).cloned())
                .collect()
        };

        for evt in events_to_signal {
            evt.signal();
        }
    }

    /// 内部方法：单临界区内校验 visible_owner 仍是 conn_id 才执行隐藏。
    ///
    /// 用于 cleanup_client 步骤2，防止在锁外窗口期被新连接的帧误隐藏。
    fn hide_kind_if_owner(&self, kind: u32, conn_id: u32) {
        let events_to_signal: Vec<Arc<NamedEvent>> = {
            let mut inner = self.inner.lock().unwrap();

            // 校验 visible_owner 的 instance 仍是 conn_id
            let Some(&(owner_pid, owner_conn)) = inner.visible_owner.get(&kind) else {
                return; // 无 owner，no-op
            };
            if owner_conn != conn_id {
                return; // 已被新 conn 接管，不操作
            }

            inner.visible_owner.remove(&kind);
            if let Some(shm) = inner.shms.get_mut(&kind) {
                shm.write_hidden(0);
            }

            // 收集 owner_pid 全部实例该 kind 的 event
            inner
                .clients
                .iter()
                .filter(|(_, cs)| cs.pid == owner_pid)
                .filter_map(|(_, cs)| cs.events.get(&kind).cloned())
                .collect()
        };

        for evt in events_to_signal {
            evt.signal();
        }
    }

    /// 取 conn_id 的 setup_seq（断线时记录，配合 cleanup_client SetupSeq 守卫）
    pub fn setup_seq_of(&self, conn_id: u32) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .clients
            .get(&conn_id)
            .map_or(0, |s| s.setup_seq)
    }

    /// 断线清理（SetupSeq 守卫防旧清理误删新状态）
    pub fn cleanup_client(&self, conn_id: u32, expected_seq: u64) {
        // 步骤1：seq 守卫，收集需要 hide 的 kind，记录 actual_seq 供步骤3用
        let (kinds_to_hide, actual_seq): (Vec<u32>, u64) = {
            let inner = self.inner.lock().unwrap();
            let Some(state) = inner.clients.get(&conn_id) else {
                return;
            };
            if expected_seq != 0 && state.setup_seq != expected_seq {
                info!(
                    "cleanup_client: seq 不匹配跳过 conn_id={conn_id} \
                     expected={expected_seq} actual={}",
                    state.setup_seq
                );
                return;
            }
            let actual_seq = state.setup_seq;
            let kinds = inner
                .visible_owner
                .iter()
                .filter(|(_, (_, cid))| *cid == conn_id)
                .map(|(kind, _)| *kind)
                .collect();
            (kinds, actual_seq)
        };

        // 步骤2：单临界区身份校验 hide（防竞态：锁外窗口期新 conn 成为 owner 时不误隐藏）
        for kind in kinds_to_hide {
            self.hide_kind_if_owner(kind, conn_id);
        }

        // 步骤3：单临界区内校验 actual_seq（覆盖 expected_seq==0 的力清场景），
        //        防 hide 期间重连的新 client 被误删。
        let mut inner = self.inner.lock().unwrap();
        match inner.clients.get(&conn_id) {
            None => return, // 已被其他路径清理
            Some(state) => {
                if state.setup_seq != actual_seq {
                    // hide 期间发生了重连，新 setup 已就绪，不再移除
                    return;
                }
            }
        }
        inner.clients.remove(&conn_id);
        if inner.active.is_some_and(|(cid, _)| cid == conn_id) {
            inner.active = None;
        }
        info!("cleanup_client: 已清理 conn_id={conn_id}");
    }

    /// Shutdown 用：三 kind 各 hide 一次（幂等）
    pub fn hide_all(&self) {
        for &kind in &ALL_KINDS {
            self.hide_kind(kind);
        }
    }
}

// ---------- 测试 ----------

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use wind_ipc::protocol::{HOST_WINDOW_CANDIDATE, HostRenderHitRect, SharedRenderHeader};

    /// 构造测试专用 manager（suffix 含 tag + pid 防并发冲突）
    fn make_mgr(tag: &str) -> Arc<HostRenderManager> {
        let pid = std::process::id();
        let suffix = format!("_{}_{}", tag, pid);
        HostRenderManager::new(&suffix, vec!["*".to_string()])
    }

    #[test]
    fn setup_returns_three_entries_with_instance_id() {
        let mgr = make_mgr("S1");
        let pid = std::process::id();
        let (instance_id, entries) = mgr.setup(1, pid, 0).expect("setup should succeed");
        assert_eq!(instance_id, 1);
        assert_eq!(entries.len(), 3, "should return entries for all 3 kinds");
        let mut kinds: Vec<u32> = entries.iter().map(|e| e.window_kind).collect();
        kinds.sort_unstable();
        assert_eq!(
            kinds,
            vec![0, 1, 2],
            "should cover CANDIDATE/TOOLTIP/STATUS"
        );
        for entry in &entries {
            assert!(
                entry.event_name.contains("_C1"),
                "event_name 应含 _C1: {}",
                entry.event_name
            );
        }
    }

    #[test]
    fn setup_rejects_non_whitelisted() {
        let pid = std::process::id();
        let suffix = format!("_S2_{}", pid);
        let mgr = HostRenderManager::new(&suffix, vec!["notepad.exe".to_string()]);
        let result = mgr.setup(1, pid, 0);
        assert!(result.is_err(), "未命中白名单应返回 Err");
    }

    #[test]
    fn whitelist_wildcard_match_case_insensitive() {
        let pid = std::process::id();
        let suffix = format!("_S3_{}", pid);
        // "*.EXE"（大写）应通过大小写不敏感匹配命中当前测试进程（.exe 小写）
        let mgr = HostRenderManager::new(
            &suffix,
            vec!["SearchHost.exe".to_string(), "*.EXE".to_string()],
        );
        assert!(
            mgr.is_process_whitelisted(pid),
            "*.EXE 应大小写不敏感匹配当前进程"
        );
    }

    /// DLL 在请求里声明了穿透层能力，活跃目标才带 `pass_layer`；旧 DLL（能力 0）不带。
    #[test]
    fn active_target_carries_pass_layer_capability() {
        let mgr = make_mgr("SCAP");
        let pid = std::process::id();
        mgr.setup(1, pid, 0).expect("setup");
        mgr.setup(2, pid, HOST_RENDER_CAP_PASS_LAYER)
            .expect("setup");
        mgr.note_focus(1, pid);
        assert!(!mgr.active_target().unwrap().pass_layer);
        mgr.note_focus(2, pid);
        assert!(mgr.active_target().unwrap().pass_layer);
    }

    #[test]
    fn write_frame_records_visible_owner_and_hide_clears() {
        let mgr = make_mgr("S4");
        let pid = std::process::id();
        let (instance_id, _) = mgr.setup(1, pid, 0).expect("setup");
        mgr.note_focus(1, pid);
        let target = mgr.active_target().expect("active_target 应为 Some");
        assert_eq!(target.instance_id, instance_id);

        let bgra = vec![0xAAu8; 4 * 4 * 4];
        let rects = [HostRenderHitRect {
            index: 0,
            x: 0,
            y: 0,
            w: 4,
            h: 4,
        }];
        let frame = FrameParams {
            sequence: 0,
            x: 0,
            y: 0,
            width: 4,
            height: 4,
            bgra: &bgra,
            rects: &rects,
            rendered_hover_index: -1,
            target_instance_id: 0, // 应被 target.instance_id 覆盖
            software_shadow: false,
            pass: None,
        };
        mgr.write_frame_for_kind(HOST_WINDOW_CANDIDATE, &target, &frame)
            .expect("write_frame");

        // 写帧后：flags 含 VISIBLE，target_instance_id == instance_id
        {
            let inner = mgr.inner.lock().unwrap();
            let shm = inner.shms.get(&HOST_WINDOW_CANDIDATE).expect("SHM 应存在");
            let (hdr, _) = shm.read_back();
            assert_ne!(
                { hdr.flags } & SharedRenderHeader::FLAG_VISIBLE,
                0,
                "写帧后应有 VISIBLE 标志"
            );
            assert_eq!(
                { hdr.target_instance_id },
                instance_id,
                "target_instance_id 应被覆盖为 instance_id"
            );
        }

        // 第一次 hide
        mgr.hide_kind(HOST_WINDOW_CANDIDATE);
        let seq_after_first = {
            let inner = mgr.inner.lock().unwrap();
            let shm = inner
                .shms
                .get(&HOST_WINDOW_CANDIDATE)
                .expect("SHM 仍应存在");
            let (hdr, _) = shm.read_back();
            assert_eq!(
                { hdr.flags } & SharedRenderHeader::FLAG_VISIBLE,
                0,
                "hide 后 VISIBLE 应清零"
            );
            assert_eq!(
                { hdr.target_instance_id },
                0,
                "hide 后 target_instance_id 应为 0（广播）"
            );
            hdr.sequence
        };

        // 第二次 hide 应为 no-op（sequence 不再递增）
        mgr.hide_kind(HOST_WINDOW_CANDIDATE);
        let seq_after_second = {
            let inner = mgr.inner.lock().unwrap();
            let shm = inner.shms.get(&HOST_WINDOW_CANDIDATE).unwrap();
            shm.read_back().0.sequence
        };
        assert_eq!(
            seq_after_first, seq_after_second,
            "第二次 hide 应幂等（sequence 不变）"
        );
    }

    #[test]
    fn hide_kind_without_owner_is_noop() {
        let mgr = make_mgr("S5");
        // 未写过帧，直接 hide → 不 panic，SHM 未创建
        mgr.hide_kind(HOST_WINDOW_CANDIDATE);
        let inner = mgr.inner.lock().unwrap();
        assert!(
            !inner.shms.contains_key(&HOST_WINDOW_CANDIDATE),
            "no-op hide 不应创建 SHM"
        );
    }

    #[test]
    fn cleanup_with_stale_seq_skipped() {
        let mgr = make_mgr("S6");
        let pid = std::process::id();

        mgr.setup(1, pid, 0).expect("第一次 setup");
        let seq_a = mgr.setup_seq_of(1);

        mgr.setup(1, pid, 0).expect("第二次 setup（重连）");
        let seq_b = mgr.setup_seq_of(1);
        assert_ne!(seq_a, seq_b, "两次 setup seq 应不同");

        mgr.note_focus(1, pid);

        // 旧 seq_a cleanup → 应被跳过
        mgr.cleanup_client(1, seq_a);
        assert!(
            mgr.active_target().is_some(),
            "旧 seq cleanup 后 active_target 仍应为 Some"
        );

        // 当前 seq_b cleanup → 成功清除
        mgr.cleanup_client(1, seq_b);
        assert!(
            mgr.active_target().is_none(),
            "有效 cleanup 后 active_target 应为 None"
        );
    }

    #[test]
    fn active_target_requires_setup() {
        let mgr = make_mgr("S7");
        let pid = std::process::id();
        // 只 note_focus，未 setup → None
        mgr.note_focus(1, pid);
        assert!(
            mgr.active_target().is_none(),
            "未 setup 时 active_target 应为 None"
        );
    }

    #[test]
    fn instance_ids_start_at_one_never_zero() {
        let mgr = make_mgr("S8");
        let pid = std::process::id();
        // conn_id == 0 应 Err
        assert!(mgr.setup(0, pid, 0).is_err(), "setup(0, ..) 应返回 Err");
        // conn_id >= 1 应成功，instance_id == conn_id != 0
        let (instance_id, _) = mgr.setup(1, pid, 0).expect("setup(1, ..) 应成功");
        assert_eq!(instance_id, 1);
        assert_ne!(instance_id, 0);
    }

    // ---------- OpenEventW + WaitForSingleObject(0) 辅助 ----------

    /// 以 OpenEventW + WaitForSingleObject(timeout=0) 验证 Event 已置信号（消费一次）
    fn open_and_wait0(name: &str) -> bool {
        use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{
            OpenEventW, SYNCHRONIZATION_ACCESS_RIGHTS, WaitForSingleObject,
        };
        let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe {
            OpenEventW(
                SYNCHRONIZATION_ACCESS_RIGHTS(0x0010_0000u32),
                false,
                windows::core::PCWSTR(name_w.as_ptr()),
            )
        }
        .expect("OpenEventW failed");
        let result = unsafe { WaitForSingleObject(handle, 0) };
        unsafe {
            let _ = CloseHandle(handle);
        }
        result == WAIT_OBJECT_0
    }

    /// 同一 pid 两个 conn：write_frame_for_kind 应唤醒该 pid 全部实例的 event
    #[test]
    fn wake_all_instances_of_same_pid() {
        let mgr = make_mgr("W1");
        let pid = std::process::id();

        // conn1 和 conn2 同 pid
        let (_, entries1) = mgr.setup(1, pid, 0).expect("setup conn1");
        let (_, entries2) = mgr.setup(2, pid, 0).expect("setup conn2");

        // 取 conn1 的 CANDIDATE event 名称
        let evt1_name = entries1
            .iter()
            .find(|e| e.window_kind == HOST_WINDOW_CANDIDATE)
            .expect("entries1 应含 CANDIDATE")
            .event_name
            .clone();
        // 取 conn2 的 CANDIDATE event 名称
        let evt2_name = entries2
            .iter()
            .find(|e| e.window_kind == HOST_WINDOW_CANDIDATE)
            .expect("entries2 应含 CANDIDATE")
            .event_name
            .clone();

        // 以 conn1 为 target 写帧
        mgr.note_focus(1, pid);
        let target = mgr.active_target().expect("active_target");
        let bgra = vec![0xCCu8; 4 * 4 * 4];
        let rects = [wind_ipc::protocol::HostRenderHitRect {
            index: 0,
            x: 0,
            y: 0,
            w: 4,
            h: 4,
        }];
        let frame = FrameParams {
            sequence: 0,
            x: 0,
            y: 0,
            width: 4,
            height: 4,
            bgra: &bgra,
            rects: &rects,
            rendered_hover_index: -1,
            target_instance_id: 0,
            software_shadow: false,
            pass: None,
        };
        mgr.write_frame_for_kind(HOST_WINDOW_CANDIDATE, &target, &frame)
            .expect("write_frame");

        // conn1 的 event 应已置信号
        assert!(
            open_and_wait0(&evt1_name),
            "conn1 的 CANDIDATE event 应被置信号"
        );
        // conn2（同 pid）的 event 也应已置信号
        assert!(
            open_and_wait0(&evt2_name),
            "conn2（同 pid）的 CANDIDATE event 也应被置信号"
        );
    }

    /// cleanup_client 不得误隐藏已被新 conn 接管的帧
    #[test]
    fn cleanup_does_not_hide_newer_owner() {
        let mgr = make_mgr("W2");
        let pid = std::process::id();
        use wind_ipc::protocol::SharedRenderHeader;

        // conn1 setup 并写帧 → 成为 CANDIDATE owner
        mgr.setup(1, pid, 0).expect("setup conn1");
        let seq1 = mgr.setup_seq_of(1);
        mgr.note_focus(1, pid);
        let target1 = mgr.active_target().expect("active_target conn1");
        let bgra = vec![0xAAu8; 4 * 4 * 4];
        let rects = [wind_ipc::protocol::HostRenderHitRect {
            index: 0,
            x: 0,
            y: 0,
            w: 4,
            h: 4,
        }];
        let frame = FrameParams {
            sequence: 0,
            x: 0,
            y: 0,
            width: 4,
            height: 4,
            bgra: &bgra,
            rects: &rects,
            rendered_hover_index: -1,
            target_instance_id: 0,
            software_shadow: false,
            pass: None,
        };
        mgr.write_frame_for_kind(HOST_WINDOW_CANDIDATE, &target1, &frame)
            .expect("write_frame conn1");

        // conn2 setup 并写帧 → 抢占 CANDIDATE owner（target_instance_id == 2）
        mgr.setup(2, pid, 0).expect("setup conn2");
        mgr.note_focus(2, pid);
        let target2 = mgr.active_target().expect("active_target conn2");
        assert_eq!(target2.instance_id, 2);
        mgr.write_frame_for_kind(HOST_WINDOW_CANDIDATE, &target2, &frame)
            .expect("write_frame conn2");

        // SHM 应记录 conn2 为 owner
        {
            let inner = mgr.inner.lock().unwrap();
            let (hdr, _) = inner.shms[&HOST_WINDOW_CANDIDATE].read_back();
            assert_eq!({ hdr.target_instance_id }, 2, "写帧后 target 应为 conn2");
        }

        // cleanup conn1（正确 seq）→ 步骤2应识别 owner 已是 conn2，跳过 hide
        mgr.cleanup_client(1, seq1);

        // SHM 应仍然 VISIBLE，target 仍为 2
        {
            let inner = mgr.inner.lock().unwrap();
            let (hdr, _) = inner.shms[&HOST_WINDOW_CANDIDATE].read_back();
            assert_ne!(
                { hdr.flags } & SharedRenderHeader::FLAG_VISIBLE,
                0,
                "cleanup conn1 不应隐藏 conn2 的帧（SHM 仍应 VISIBLE）"
            );
            assert_eq!(
                { hdr.target_instance_id },
                2,
                "cleanup conn1 不应改变 target（仍应为 conn2）"
            );
        }

        // conn2 状态仍在
        assert_ne!(mgr.setup_seq_of(2), 0, "conn2 仍应在 clients 中");
    }

    // ---------- wildcard_match 纯逻辑测试（不需要 Windows 原语） ----------

    #[test]
    fn wildcard_exact_match() {
        assert!(wildcard_match("notepad.exe", "notepad.exe"));
        assert!(!wildcard_match("notepad.exe", "wordpad.exe"));
    }

    #[test]
    fn wildcard_star_prefix() {
        assert!(wildcard_match("*.exe", "notepad.exe"));
        assert!(wildcard_match("*.exe", "a.exe"));
        assert!(!wildcard_match("*.exe", "notepad.txt"));
    }

    #[test]
    fn wildcard_star_any() {
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("*", ""));
    }

    #[test]
    fn wildcard_question_mark() {
        assert!(wildcard_match("note?.exe", "notea.exe"));
        assert!(!wildcard_match("note?.exe", "noteab.exe"));
    }

    #[test]
    fn wildcard_case_insensitive() {
        assert!(wildcard_match("*.EXE", "notepad.exe"));
        assert!(wildcard_match("SearchHost.exe", "searchhost.exe"));
        assert!(wildcard_match("NOTEPAD.EXE", "notepad.exe"));
    }
}
