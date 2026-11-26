use super::*;
use crate::testing::FakeCore;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use wind_sync::learn::{Key, norm_key};
use wind_sync::{Dek, MemBackend};

const FREQ_HEAD: &str = "---\ncolumns: [code, text, count, last_used]\nwind:\n  kind: freq\n  code_spacing: syllable\n  escape: true\n...\n# no comment\n";
const TEMP_HEAD: &str = "---\ncolumns: [code, text, weight, count, created_at]\nwind:\n  kind: temp\n  escape: true\n...\n# no comment\n";
const COMP_HEAD: &str = "---\ncolumns: [kind, text, count, last_used]\nwind: {kind: completion, escape: true}\n...\n# no comment\n";
const LIB: &str = "lib-1";
const FREQ: &str = "pinyin/freq.table.yaml";
const COMP: &str = "completion.table.yaml";

struct Dev {
    _tmp: tempfile::TempDir,
    user: PathBuf,
    state: PathBuf,
    core: Arc<FakeCore>,
    id: String,
    content: SyncContent,
    now: u64,
}

impl Dev {
    fn new(id: &str) -> Dev {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        std::fs::create_dir_all(user.join("data/pinyin")).unwrap();
        Dev {
            user,
            state: tmp.path().join("state"),
            _tmp: tmp,
            core: Arc::new(FakeCore::default()),
            id: id.into(),
            content: SyncContent::default(),
            now: 1_000_000,
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.user.join("data").join(rel)
    }

    fn write(&self, rel: &str, head: &str, rows: &str) {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, format!("{head}{rows}")).unwrap();
    }

    fn ctx(&self, fail_at: Option<FailPoint>) -> LearnCtx<'_> {
        let mut c = LearnCtx::new(
            &self.user,
            &self.state,
            &*self.core,
            &self.id,
            self.now,
            self.content,
            LIB,
        );
        c.fail_at = fail_at;
        c
    }

    fn round_with(&mut self, b: &MemBackend, fail_at: Option<FailPoint>) -> Result<(), String> {
        self.now += 60;
        let ctx = self.ctx(fail_at);
        let mut st = load_state(&ctx)?;
        count_in(&ctx, &mut st)?;
        exchange_and_materialize(&ctx, b, &key(), &mut st).map(|_| ())
    }

    fn round(&mut self, b: &MemBackend) {
        self.round_with(b, None).unwrap();
    }
}

fn key() -> wind_sync::crypto::FilesKey {
    static DEK: std::sync::OnceLock<Dek> = std::sync::OnceLock::new();
    DEK.get_or_init(Dek::generate).files_key()
}

/// 文件里某键的 count；文件或键不存在为 None。
fn count(d: &Dev, rel: &str, kind: LearnKind, k0: &str, k1: &str) -> Option<u64> {
    let bytes = std::fs::read(d.path(rel)).ok()?;
    let f = table::parse(kind, Some(&bytes)).unwrap();
    let nk = norm_key(kind, &Key(k0.into(), k1.into()));
    f.rows.get(&nk).map(|r| r.1)
}

fn freq(d: &Dev) -> Option<u64> {
    count(d, FREQ, LearnKind::Freq, "ni hao", "你好")
}

fn comp(d: &Dev) -> Option<u64> {
    count(d, COMP, LearnKind::Completion, "email", "a@b.c")
}

fn set_freq(d: &Dev, n: u64) {
    let p = d.path(FREQ);
    // 保留现有头部（core 折回只按行补丁）
    let head = std::fs::read(&p)
        .ok()
        .and_then(|b| table::parse(LearnKind::Freq, Some(&b)).ok())
        .map(|f| f.head)
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| FREQ_HEAD.into());
    std::fs::write(p, format!("{head}ni hao\t你好\t{n}\t100\n")).unwrap();
}

fn set_comp(d: &Dev, n: u64) {
    d.write(COMP, COMP_HEAD, &format!("email\ta@b.c\t{n}\t100\n"));
}

#[test]
fn two_devices_counts_add_up() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&a, 3);
    set_freq(&bb, 4);
    for _ in 0..2 {
        a.round(&b);
        bb.round(&b);
    }
    assert_eq!(freq(&a), Some(7));
    assert_eq!(freq(&bb), Some(7));
}

#[test]
fn rerun_without_changes_writes_nothing() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&a, 3);
    set_freq(&bb, 4);
    for _ in 0..2 {
        a.round(&b);
        bb.round(&b);
    }
    let snap = |d: &Dev| {
        let p = d.path(FREQ);
        (
            std::fs::read(&p).unwrap(),
            std::fs::metadata(&p).unwrap().modified().unwrap(),
        )
    };
    let (sa, sb) = (snap(&a), snap(&bb));
    let puts = b.put_count();
    std::thread::sleep(std::time::Duration::from_millis(20));
    a.round(&b);
    bb.round(&b);
    assert_eq!(snap(&a), sa);
    assert_eq!(snap(&bb), sb);
    assert_eq!(b.put_count(), puts, "没有变化不应上传");
}

#[test]
fn learning_between_phases_is_kept() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&a, 3);
    set_freq(&bb, 4);
    bb.round(&b);
    // 第 2 次 beginEdit（物化段）前，core 又学了 2 次
    let begins = Arc::new(AtomicUsize::new(0));
    let path = a.path(FREQ);
    {
        let begins = begins.clone();
        *a.core.on_call.lock().unwrap() = Some(Box::new(move |m, p| {
            if m == "data.beginEdit"
                && p.get("path").and_then(|v| v.as_str()) == Some(FREQ)
                && begins.fetch_add(1, Ordering::SeqCst) == 1
            {
                let s = std::fs::read_to_string(&path).unwrap();
                std::fs::write(&path, s.replace("\t3\t", "\t5\t")).unwrap();
            }
        }));
    }
    a.round(&b);
    assert_eq!(begins.load(Ordering::SeqCst), 2);
    assert_eq!(freq(&a), Some(9));
    *a.core.on_call.lock().unwrap() = None;
    a.round(&b);
    bb.round(&b);
    a.round(&b);
    assert_eq!(freq(&a), Some(9));
    assert_eq!(freq(&bb), Some(9));
}

#[test]
fn crash_after_write_before_commit_keeps_counts() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&a, 3);
    set_freq(&bb, 4);
    bb.round(&b);
    assert!(a.round_with(&b, Some(FailPoint::AfterWrite)).is_err());
    assert_eq!(freq(&a), Some(7), "F' 已写入");
    // core 代为结束租约并立即折回租约期间学到的 1 次
    set_freq(&a, 8);
    a.round(&b);
    assert_eq!(freq(&a), Some(8));
    bb.round(&b);
    a.round(&b);
    assert_eq!(freq(&a), Some(8));
    assert_eq!(freq(&bb), Some(8));
}

#[test]
fn crash_before_write_drops_pending() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&a, 3);
    set_freq(&bb, 4);
    bb.round(&b);
    assert!(a.round_with(&b, Some(FailPoint::BeforeWrite)).is_err());
    assert_eq!(freq(&a), Some(3), "F' 没写成");
    set_freq(&a, 4);
    a.round(&b);
    assert_eq!(freq(&a), Some(8));
    bb.round(&b);
    a.round(&b);
    assert_eq!(freq(&a), Some(8));
    assert_eq!(freq(&bb), Some(8));
}

#[test]
fn materialize_creates_missing_file() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    a.write(
        "english/temp.dict.yaml",
        TEMP_HEAD,
        "hello\thello\t5\t2\t100\n",
    );
    std::fs::create_dir_all(bb.path("english")).unwrap();
    a.round(&b);
    bb.round(&b);
    let raw = std::fs::read_to_string(bb.path("english/temp.dict.yaml")).unwrap();
    let f = table::parse(LearnKind::Temp, Some(raw.as_bytes())).unwrap();
    assert_eq!(f.columns, ["code", "text", "weight", "count", "created_at"]);
    let nk = norm_key(LearnKind::Temp, &Key("hello".into(), "hello".into()));
    let row = &f.rows[&nk];
    assert_eq!((row.1, row.2.clone()), (2, vec!["5".into(), "100".into()]));
    assert!(
        raw.contains("  kind: temp\n  escape: true\n  sync: "),
        "{raw}"
    );
    assert!(!raw.contains("code_spacing"), "{raw}");
    assert!(table::token_of(raw.as_bytes()).is_some());
}

#[test]
fn skips_file_under_user_edit() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&a, 3);
    set_comp(&a, 2);
    a.core.user_sessions.lock().unwrap().push(FREQ.into());
    a.round(&b);
    assert!(
        !a.core
            .methods()
            .iter()
            .any(|m| m == &format!("data.beginEdit {FREQ}")),
        "{:?}",
        a.core.methods()
    );
    bb.round(&b);
    assert_eq!(freq(&bb), None);
    assert_eq!(comp(&bb), Some(2));
}

#[test]
fn content_switch_off_stops_group() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_comp(&a, 2);
    a.round(&b);
    bb.round(&b);
    assert_eq!(comp(&bb), Some(2));

    a.content.completion = false;
    a.core.clear_calls();
    set_comp(&a, 5);
    a.round(&b);
    bb.round(&b);
    assert_eq!(comp(&bb), Some(2), "关闭期间不上传");
    set_comp(&bb, 3);
    bb.round(&b);
    a.round(&b);
    assert_eq!(comp(&a), Some(5), "关闭期间不物化");
    assert!(
        !a.core.methods().iter().any(|m| m.ends_with(COMP)),
        "关闭期间不碰文件"
    );

    a.content.completion = true;
    a.round(&b);
    assert_eq!(comp(&a), Some(6));
    bb.round(&b);
    a.round(&b);
    assert_eq!(comp(&a), Some(6));
    assert_eq!(comp(&bb), Some(6));
}

#[test]
fn unknown_schema_mirror_kept_not_materialized() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    bb.write("wubi86/freq.table.yaml", FREQ_HEAD, "wgkq\t输入\t2\t100\n");
    bb.round(&b);
    a.round(&b);
    assert!(!a.path("wubi86").exists());
    let ctx = a.ctx(None);
    let st = load_state(&ctx).unwrap();
    let n = st.peer_seen["pc-b"];
    let mirror = private_dir(&ctx)
        .join("peers/pc-b")
        .join(n.to_string())
        .join("wubi86/freq.base");
    assert!(mirror.is_file(), "{}", mirror.display());
}

#[test]
fn old_mirror_dirs_are_removed_after_commit() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&bb, 1);
    bb.round(&b);
    a.round(&b);
    set_freq(&bb, 2);
    bb.round(&b);
    a.round(&b);
    let ctx = a.ctx(None);
    let st = load_state(&ctx).unwrap();
    let dirs: Vec<String> = std::fs::read_dir(private_dir(&ctx).join("peers/pc-b"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(dirs, [st.peer_seen["pc-b"].to_string()]);
    assert_eq!(freq(&a), Some(2));
}

#[test]
fn learned_groups_filter_by_content() {
    let d = Dev::new("pc-a");
    std::fs::create_dir_all(d.path("charsets")).unwrap();
    std::fs::create_dir_all(d.path("stats")).unwrap();
    let names = |c: SyncContent| -> Vec<String> {
        learned_groups(&d.user, c)
            .into_iter()
            .map(|(n, ..)| n)
            .collect()
    };
    assert_eq!(
        names(SyncContent::default()),
        ["completion", "pinyin/assoc", "pinyin/freq", "pinyin/temp"]
    );
    let off = SyncContent {
        learned: false,
        ..Default::default()
    };
    assert_eq!(names(off), ["completion"]);
}
