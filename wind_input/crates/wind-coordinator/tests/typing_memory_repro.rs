//! 本机复现「注释 + 悬停开着时打字常驻 +18 MB」（`docs/design/memory-footprint.md` S5）的探针。
//!
//! ⚠️ **本文件只能有这一个用例**：用户目录靠 `set_var("WIND_INSTALL_ROOT")` + 便携标记重定向，
//! `set_var` 要求进程里没有别的线程同时读环境、且路径缓存（`OnceLock`）只认第一次；再加用例
//! 就会与它并行（或抢先初始化路径缓存），重定向失效、读写到真实用户目录。另起文件。
//!
//! **观测用、不断言**，默认 `#[ignore]`。按桌面构造的顺序跑：构造协调器 → 等价于桌面的启动
//! 预热 → 经真实按键入口打字（五笔简码 + 拼音分步上屏，持续刷新候选、触发注释 / 悬停渲染与
//! 拼音自动造词）→ 空闲。每阶段打点：
//!
//! - `live`：本测试二进制的计数分配器统计的**在用堆字节**（与分配器是否归还无关，
//!   最接近「常驻数据」的口径）；
//! - `RssAnon`（≈ Windows Private：mmap 的词库页是 file-backed，不计）不整理 / `malloc_trim(0)` 后
//!   两个值（后者近似 Windows 堆会自行归还大块）；`VmHWM` 高水位；
//! - 各结构自报大小与已加载引擎（`Coordinator::debug_memory_report`）。
//!
//! # 用法
//!
//! ```text
//! WIND_REPRO_DIR=~/.cache/wi-s5-repro \
//! WIND_REPRO_VARIANT=orig            # 逗号分隔，见 `apply_variant`
//! WIND_REPRO_COMMITS=400             # 上屏次数
//! WIND_REPRO_KEY_MS=15               # 键间隔
//! WIND_REPRO_STOP_AFTER=3            # 「一阶段一进程」：跑到第 N 阶段就停（缺省全跑）
//! WIND_REPRO_LONG_IDLE=65            # 末阶段真等这么多秒（缺省直接调 debug_reclaim_now 模拟）
//! WIND_REPRO_PREBUILD_PINYIN=1       # 打字前先单独建 `${pinyin}` 的读音索引（S2 后多一拍）
//! cargo test -p wind-coordinator --release --test typing_memory_repro -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `WIND_REPRO_DIR` 是从靶机拉下来的用户目录副本（`config.toml`、`userdata.redb`、`schemas/`、
//! `schema_overrides/` 等）。用户目录经便携标记重定向到 `$TMPDIR/wi-s5-root/userdata`，每次运行
//! 从 `WIND_REPRO_DIR` **复制**一份再用（打字会写库，原件不动）；`localdata/cache` 跨运行保留，
//! 首跑要建缓存，先跑一次热身再取数。系统数据取 `build_dev/data`。headless 没有 caret 事件，
//! 首显闸门改成 `instant`，否则首帧不渲染注释与悬停。
//!
//! # 结论（2026-10-09，靶机配置：wubi86_pinyin、注释含 `${pinyin}`、19 万拼音用户词）
//!
//! 400 次上屏 / 2553 键，单位 MB，RssAnon 取 `malloc_trim` 后；各跑两次、数字一致：
//!
//! | 配置 | 预热后 live / RssAnon | 长档回收后 live / RssAnon |
//! |---|---|---|
//! | 原配置（紧凑化前） | 18.4 / 20.4 | 27.5 / 36.8 |
//! | 只关注释、或模板去掉 `${pinyin}` | 18.4 / 20.4 | 18.4 / 24.2 |
//! | 只关悬停 / 去 `${code_rev}` / 关拼音造词 | 同原配置 | 27.5 / 36.6–36.8 |
//! | 原配置（`CharPinyinIndex` 紧凑化后） | 18.4 / 20.4 | 19.1 / 24.9–25.1 |
//!
//! - 「打字常驻 +18 MB」的主体是 `${pinyin}` 对五笔候选推读音时建的 `CharPinyinIndex`：
//!   紧凑化前 4 万字 / 5.2 万读音拆成 13.3 万个小块，在用堆 9.1 MB、RssAnon +12.5 MB；
//!   紧凑化后 3 块共 675 KB（在用堆 +0.7 MB），构建 30 ms → 13–15 ms。
//! - `UserTextIndex` 不在其中：19 万词都在 `pinyin` 桶，按方案代次只推进那一份，wubi86 槽
//!   1 KB、整段打字重建 1 次。
//! - 其余约 4 MB（各配置都有）是 redb 读缓存（回收后落）与打字碎片。
//! - 60 秒长档回收实际要停手约 60–70 秒才触发（闲置慢拍 10 秒 + 60 个 1 秒拍）。

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_ui_types::UiCommand;

// ---------- 计数分配器 ----------

struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);
static LIVE_BLOCKS: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn add_live(delta: isize) {
    let now = LIVE.fetch_add(delta, Relaxed) + delta;
    if delta > 0 {
        PEAK.fetch_max(now.max(0) as usize, Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            add_live(l.size() as isize);
            LIVE_BLOCKS.fetch_add(1, Relaxed);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            add_live(l.size() as isize);
            LIVE_BLOCKS.fetch_add(1, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        add_live(-(l.size() as isize));
        LIVE_BLOCKS.fetch_sub(1, Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            add_live(new as isize - l.size() as isize);
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

// ---------- 进程内存 ----------

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn trim() {
    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    unsafe {
        malloc_trim(0);
    }
}
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn trim() {}

/// `/proc/self/status` 里的 (RssAnon, RssFile, VmHWM)，单位 KB。
fn proc_kb() -> (u64, u64, u64) {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let get = |k: &str| {
        s.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    (get("RssAnon:"), get("RssFile:"), get("VmHWM:"))
}

fn mb(kb: u64) -> f64 {
    kb as f64 / 1024.0
}

fn report(stage: u32, name: &str, c: Option<&Coordinator>) {
    let live = LIVE.load(Relaxed).max(0) as f64 / 1048576.0;
    let blocks = LIVE_BLOCKS.load(Relaxed);
    let peak = PEAK.load(Relaxed) as f64 / 1048576.0;
    let (anon, file, hwm) = proc_kb();
    trim();
    let (anon_t, _, _) = proc_kb();
    println!(
        "@@ S{stage} {name:<10} live={live:7.1}MB blocks={blocks:>8} livePeak={peak:7.1}MB \
         RssAnon={:7.1}MB trim后={:7.1}MB RssFile={:7.1}MB VmHWM={:7.1}MB",
        mb(anon),
        mb(anon_t),
        mb(file),
        mb(hwm)
    );
    if let Some(c) = c {
        for l in c.debug_memory_report().lines() {
            println!("   | {l}");
        }
    }
}

// ---------- 目录 ----------

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}

/// 便携根目录：用户层复制自 `repro`，缓存目录跨运行保留。
fn setup_root(repro: &Path) -> PathBuf {
    let root = std::env::temp_dir().join("wi-s5-root");
    let _ = std::fs::remove_dir_all(root.join("userdata"));
    std::fs::create_dir_all(root.join("data")).unwrap();
    copy_dir(repro, &root.join("userdata"));
    std::fs::write(root.join(wind_config::variant::PORTABLE_MARKER_NAME), "").unwrap();
    // SAFETY: 单线程阶段、任何路径缓存初始化之前设置。
    unsafe {
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }
    assert_eq!(
        Config::user_config_dir(),
        Some(root.join("userdata")),
        "前置条件：用户目录须已重定向"
    );
    root
}

/// 配置变体（逗号分隔，可叠加）。
fn apply_variant(cfg: &mut Config, variant: &str) {
    for v in variant.split(',').map(str::trim).filter(|v| !v.is_empty()) {
        let strip = |cfg: &mut Config, var: &str| {
            for t in [
                &mut cfg.ui.candidate.comment_template_vertical,
                &mut cfg.ui.candidate.comment_template_horizontal,
            ] {
                *t = t.replace(var, "");
            }
        };
        let off_section = |cfg: &mut Config, var: &str| {
            for s in &mut cfg.ui.tooltip.sections {
                if s.template.contains(var) {
                    s.enabled = false;
                }
            }
        };
        match v {
            "orig" => {}
            "comment_off" => cfg.ui.candidate.comment_enabled = false,
            "tooltip_off" => cfg.ui.tooltip.enabled = false,
            "no_pinyin" => strip(cfg, "${pinyin}"),
            "no_code_rev" => strip(cfg, "${code_rev}"),
            "no_dict" => strip(cfg, "${dict}"),
            "no_chaizi_code" => strip(cfg, "${chaizi_code_all}"),
            "no_tip_code" => off_section(cfg, "${word_code}"),
            "no_tip_chaizi" => off_section(cfg, "${chaizi}"),
            "no_tip_debug" => off_section(cfg, "${debug}"),
            "no_learn" => cfg.schema.pinyin.auto_learn.enabled = false,
            other => panic!("未知变体 {other}"),
        }
    }
}

// ---------- 打字 ----------

fn key(vk: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

const VK_SPACE: u32 = 0x20;
const VK_ESCAPE: u32 = 0x1B;

/// xorshift，固定种子：各变体喂同一串键。
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[(self.next() % v.len() as u64) as usize]
    }
}

const SYLLABLES: &[&str] = &[
    "ni", "hao", "wo", "men", "shi", "jie", "zhong", "guo", "ren", "min", "da", "xue", "sheng",
    "huo", "gong", "zuo", "xi", "dian", "nao", "shou", "ji", "peng", "you", "jia", "ting", "kai",
    "xin", "tian", "qi", "wen", "ti", "fang", "fa", "jing", "ji", "she", "hui", "ke", "xue", "li",
    "shu", "kan", "dao", "zhi", "dao", "xiang", "yao", "qu", "lai", "bu", "yi", "yang", "chang",
    "chuan", "shan", "shui", "hua", "cao", "mu", "feng", "yu", "lei", "dian", "che", "lu",
];

struct Typist<'a> {
    c: &'a Coordinator,
    gap: Duration,
    keys: u64,
}

impl Typist<'_> {
    fn press(&mut self, vk: u32) {
        self.c.handle_key_event(&key(vk));
        self.keys += 1;
        if !self.gap.is_zero() {
            std::thread::sleep(self.gap);
        }
    }
    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.press(ch.to_ascii_uppercase() as u32);
        }
    }
    /// 空格把缓冲打空（分步上屏要多按几次）；打不空就 Esc。
    fn drain(&mut self) {
        for _ in 0..8 {
            if self.c.debug_input_buffer().is_empty() {
                return;
            }
            if self.c.debug_page_texts().is_empty() {
                break;
            }
            self.press(VK_SPACE);
        }
        if !self.c.debug_input_buffer().is_empty() {
            self.press(VK_ESCAPE);
        }
    }
}

fn run_typing(c: &Coordinator, commits: usize, gap: Duration) -> (u64, usize) {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut t = Typist { c, gap, keys: 0 };
    let letters = "abcdefghijklmnopqrstuvwxy".as_bytes();
    let mut stepwise = 0;
    for i in 0..commits {
        if i % 2 == 0 {
            // 五笔：一 / 二级简码，空格上屏。
            let n = 1 + (rng.next() % 2) as usize;
            let code: String = (0..n)
                .map(|_| letters[(rng.next() % 25) as usize] as char)
                .collect();
            t.type_str(&code);
            t.drain();
        } else {
            // 拼音：2~4 个音节（超码长走拼音）；一半先按 2 分步选再空格收尾（分步上屏 ⇒ 自动造词）。
            let n = 2 + (rng.next() % 3) as usize;
            let py: String = (0..n).map(|_| rng.pick(SYLLABLES)).collect();
            t.type_str(&py);
            if rng.next().is_multiple_of(2) && c.debug_page_texts().len() > 1 {
                t.press(0x32);
                stepwise += 1;
            }
            t.drain();
        }
    }
    (t.keys, stepwise)
}

#[test]
#[ignore = "观测用：WIND_REPRO_DIR 指向靶机环境副本，release 跑"]
fn typing_memory_repro() {
    let Some(repro) = std::env::var_os("WIND_REPRO_DIR").map(PathBuf::from) else {
        eprintln!("跳过：未设 WIND_REPRO_DIR");
        return;
    };
    let data = data_dir();
    assert!(
        data.join("schemas/wubi86_pinyin.schema.toml").exists(),
        "缺 build_dev/data"
    );
    let variant = std::env::var("WIND_REPRO_VARIANT").unwrap_or_else(|_| "orig".into());
    let commits: usize = std::env::var("WIND_REPRO_COMMITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400);
    let gap = Duration::from_millis(
        std::env::var("WIND_REPRO_KEY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15),
    );
    let stop_after: u32 = std::env::var("WIND_REPRO_STOP_AFTER")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(u32::MAX);
    // 秒数；设了就真等（缺省直接调 debug_reclaim_now 模拟长档回收）。
    let long_idle: Option<u64> = std::env::var("WIND_REPRO_LONG_IDLE")
        .ok()
        .map(|v| v.parse().unwrap_or(65));
    println!(
        "@@ variant={variant} commits={commits} key_ms={}",
        gap.as_millis()
    );

    let root = setup_root(&repro);
    let mut cfg = Config::load(Some(&data)).expect("加载配置");
    apply_variant(&mut cfg, &variant);
    // headless 没有 caret 事件：首显闸门改「立即」，否则首帧被挂起、注释与悬停不渲染。
    cfg.ui.candidate.first_show_mode = "instant".into();
    report(0, "进程起点", None);

    let user_dir = root.join("userdata");
    let (c, rx) = Coordinator::new_headless_with_ui_at(cfg, Some(&data), Some(&user_dir));
    // UI 通道必须有人收：不收的话命令堆在通道里，测到的是通道而不是协调器。
    let frames = Arc::new(AtomicUsize::new(0));
    let tipped = Arc::new(AtomicUsize::new(0));
    let commented = Arc::new(AtomicUsize::new(0));
    {
        let (frames, tipped, commented) = (frames.clone(), tipped.clone(), commented.clone());
        std::thread::spawn(move || {
            for cmd in rx {
                if let UiCommand::UpdateCandidates { candidates, .. } = cmd {
                    frames.fetch_add(1, Relaxed);
                    if candidates.iter().any(|x| !x.tooltip.sections.is_empty()) {
                        tipped.fetch_add(1, Relaxed);
                    }
                    if candidates
                        .iter()
                        .any(|x| !x.comment.is_empty() || !x.comment_above.is_empty())
                    {
                        commented.fetch_add(1, Relaxed);
                    }
                }
            }
        });
    }
    report(1, "构造后", Some(&c));
    if stop_after < 2 {
        return;
    }

    let t0 = Instant::now();
    c.debug_desktop_startup_and_prewarm();
    println!("@@ 预热用时 {:?}", t0.elapsed());
    std::thread::sleep(Duration::from_secs(2));
    report(2, "预热后", Some(&c));
    if stop_after < 3 {
        return;
    }
    // 「预建」：在打字之前单独建一次 `${pinyin}` 用的读音索引，单测它自己的常驻量与耗时，
    // 并看打字期间才建（与按键临时分配交错）会不会多留碎片。
    if std::env::var_os("WIND_REPRO_PREBUILD_PINYIN").is_some() {
        let t0 = Instant::now();
        let r = c.debug_word_pinyin("银行行长");
        println!(
            "@@ 预建读音索引用时 {:?}（推断结果 {} 字节）",
            t0.elapsed(),
            r.len()
        );
        report(2, "预建读音后", Some(&c));
    }

    let t0 = Instant::now();
    let (keys, stepwise) = run_typing(&c, commits, gap);
    println!(
        "@@ 打字 {commits} 次上屏 / {keys} 键 / 分步 {stepwise} 次，用时 {:?}；候选帧 {} 带悬停 {} 带注释 {}",
        t0.elapsed(),
        frames.load(Relaxed),
        tipped.load(Relaxed),
        commented.load(Relaxed)
    );
    report(3, "打字刚停", Some(&c));
    if stop_after < 4 {
        return;
    }

    // 后台重建线程收尾 + 短档（3 秒）回收。
    std::thread::sleep(Duration::from_secs(5));
    report(4, "停手5秒", Some(&c));
    if stop_after < 5 {
        return;
    }

    if let Some(secs) = long_idle {
        std::thread::sleep(Duration::from_secs(secs));
    } else {
        c.debug_reclaim_now();
    }
    report(5, "长档回收后", Some(&c));
}
