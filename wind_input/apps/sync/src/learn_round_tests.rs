use super::*;
use crate::testing::FakeCore;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use wind_sync::backend::{BackendError, ObjMeta};
use wind_sync::learn::{Key, norm_key};
use wind_sync::{Dek, MemBackend};

const FREQ_HEAD: &str = "---\ncolumns: [code, text, count, last_used]\nwind:\n  kind: freq\n  code_spacing: syllable\n  escape: true\n...\n# no comment\n";
const TEMP_HEAD: &str = "---\ncolumns: [code, text, weight, count, created_at]\nwind:\n  kind: temp\n  escape: true\n...\n# no comment\n";
const COMP_HEAD: &str = "---\ncolumns: [kind, text, count, last_used]\nwind: {kind: completion, escape: true}\n...\n# no comment\n";
const LIB: &str = "loc\nk1";
const LIB2: &str = "loc\nk2";
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
    lib: String,
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
            lib: LIB.into(),
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
            &self.lib,
        );
        c.fail_at = fail_at;
        c
    }

    fn round_with(&mut self, b: &dyn Backend, fail_at: Option<FailPoint>) -> Result<(), String> {
        self.now += 60;
        let ctx = self.ctx(fail_at);
        let mut st = ensure_state(&ctx, b, &key())?;
        count_in(&ctx, &mut st)?;
        exchange_and_materialize(&ctx, b, &key(), &mut st).map(|_| ())
    }

    fn round(&mut self, b: &dyn Backend) {
        self.round_with(b, None).unwrap();
    }

    fn init(&mut self, b: &dyn Backend, how: Joined) {
        self.now += 60;
        let ctx = self.ctx(None);
        let mut st = load_state(&ctx).unwrap();
        init_for_library(&ctx, &mut st, how, b, &key()).unwrap();
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

fn clear_freq(d: &Dev) {
    let p = d.path(FREQ);
    let f = table::parse(LearnKind::Freq, Some(&std::fs::read(&p).unwrap())).unwrap();
    std::fs::write(p, f.head).unwrap();
}

fn own_contrib(d: &Dev, group: &str, kind: LearnKind) -> Contribution {
    let ctx = d.ctx(None);
    let st = load_state(&ctx).unwrap();
    contrib_of(kind, &st.groups[group].contrib).unwrap()
}

fn w_key() -> wind_sync::learn::NormKey {
    norm_key(LearnKind::Freq, &Key("ni hao".into(), "你好".into()))
}

/// B 删词后，A 在不知情时（两段之间）又学了一次：删除优先，两边都没有该词，A 的贡献里也没有。
#[test]
fn deleted_word_not_resurrected_across_devices() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&bb, 2);
    bb.round(&b);
    a.round(&b);
    assert_eq!(freq(&a), Some(2));

    clear_freq(&bb);
    bb.round(&b);
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
                std::fs::write(&path, s.replace("\t2\t", "\t3\t")).unwrap();
            }
        }));
    }
    a.round(&b);
    assert_eq!(begins.load(Ordering::SeqCst), 2);
    *a.core.on_call.lock().unwrap() = None;
    bb.round(&b);
    a.round(&b);
    assert_eq!(freq(&a), None);
    assert_eq!(freq(&bb), None);
    assert!(
        !own_contrib(&a, "pinyin/freq", LearnKind::Freq)
            .rows
            .contains_key(&w_key())
    );
}

/// 学习数据关闭期间收到删除、本机又学了该词；重新打开后删除仍优先。
#[test]
fn deleted_word_stays_deleted_after_reenable() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&bb, 2);
    bb.round(&b);
    a.round(&b);
    a.content.learned = false;
    clear_freq(&bb);
    bb.round(&b);
    a.round(&b);
    set_freq(&a, 3);
    a.content.learned = true;
    a.round(&b);
    bb.round(&b);
    a.round(&b);
    assert_eq!(freq(&a), None);
    assert_eq!(freq(&bb), None);
}

/// 某设备的新镜像解不开：本组改用它的旧镜像，本组对它的依据编号不推进，其他设备照常物化。
#[test]
fn undecodable_peer_does_not_block_group() {
    let b = MemBackend::new();
    let (mut a, mut bb, mut c) = (Dev::new("pc-a"), Dev::new("pc-b"), Dev::new("pc-c"));
    set_freq(&bb, 2);
    set_freq(&c, 3);
    bb.round(&b);
    c.round(&b);
    a.round(&b);
    assert_eq!(freq(&a), Some(5));
    let old_b = load_state(&a.ctx(None)).unwrap().peer_seen["pc-b"];

    // B 发布一份更高版本的贡献（本程序解不开）
    let bst = load_state(&bb.ctx(None)).unwrap();
    let mut objs = BTreeMap::new();
    objs.insert(
        "pinyin/freq.base".to_string(),
        b"#wind-learn 99 freq epoch=0\n".to_vec(),
    );
    wind_sync::learn::lane::publish(
        &b,
        &key(),
        "pc-b",
        &bst.owner_nonce,
        bst.own_index.as_ref(),
        &objs,
    )
    .unwrap();
    assert_eq!(freq(&c), Some(5));
    set_freq(&c, 6);
    c.round(&b);
    a.round(&b);
    assert_eq!(freq(&a), Some(6));
    let st = load_state(&a.ctx(None)).unwrap();
    assert!(st.peer_seen["pc-b"] > old_b);
    assert_eq!(st.groups["pinyin/freq"].peer_seen["pc-b"], old_b);
    assert_eq!(
        st.groups["pinyin/freq"].peer_seen["pc-c"],
        st.peer_seen["pc-c"]
    );
}

/// 发布失败：本机增量留在贡献里，下一轮照常上传，次数不丢不重。
#[test]
fn publish_failure_keeps_increments() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&a, 3);
    a.round(&b);
    set_freq(&a, 5);
    b.fail_all(Some(wind_sync::BackendError::Transient("断网".into())));
    assert!(a.round_with(&b, None).is_err());
    b.fail_all(None);
    set_freq(&a, 6);
    a.round(&b);
    bb.round(&b);
    a.round(&b);
    assert_eq!(freq(&a), Some(6));
    assert_eq!(freq(&bb), Some(6));
}

/// 两组里只有一组要写文件时崩在提交前：写了的组按令牌采用待定，没写的组已直接提交，均不重复计数。
#[test]
fn two_groups_one_written_adopts_per_group() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&bb, 4);
    bb.round(&b);
    set_freq(&a, 3);
    set_comp(&a, 2);
    assert!(a.round_with(&b, Some(FailPoint::AfterWrite)).is_err());
    assert_eq!(freq(&a), Some(7));
    {
        let st = load_state(&a.ctx(None)).unwrap();
        let p = st.pending.as_ref().unwrap();
        assert_eq!(p.groups.keys().collect::<Vec<_>>(), ["pinyin/freq"]);
        assert!(st.groups.contains_key("completion"));
    }
    set_freq(&a, 8);
    set_comp(&a, 3);
    a.round(&b);
    bb.round(&b);
    a.round(&b);
    assert_eq!((freq(&a), comp(&a)), (Some(8), Some(3)));
    assert_eq!((freq(&bb), comp(&bb)), (Some(8), Some(3)));
}

// ── 加入、重置、认领 ───────────────────────────────────

fn own_count(d: &Dev) -> Option<u64> {
    own_contrib(d, "pinyin/freq", LearnKind::Freq)
        .rows
        .get(&w_key())
        .map(|(_, r)| r.count)
}

/// a 3、b 4 同步到 7。
fn synced_pair(b: &MemBackend) -> (Dev, Dev) {
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&a, 3);
    set_freq(&bb, 4);
    for _ in 0..2 {
        a.round(b);
        bb.round(b);
    }
    assert_eq!((freq(&a), freq(&bb)), (Some(7), Some(7)));
    (a, bb)
}

#[test]
fn fresh_join_contributes_local_file() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&a, 3);
    set_freq(&bb, 4);
    bb.round(&b);
    assert_eq!(import_joined(&a.state, &a.lib), Some(Joined::Fresh));
    a.init(&b, Joined::Fresh);
    assert_eq!(import_joined(&a.state, &a.lib), None, "已有该库状态则沿用");
    a.round(&b);
    assert_eq!(own_count(&a), Some(3), "当前文件全部计入");
    bb.round(&b);
    assert_eq!((freq(&a), freq(&bb)), (Some(7), Some(7)));
}

/// a 重置远端（新库）：贡献 = 当前文件全部（已含 b 的那部分）。
fn reset_by_a(a: &mut Dev) -> MemBackend {
    let b2 = MemBackend::new();
    a.lib = LIB2.into();
    a.init(&b2, Joined::ResetByMe);
    b2
}

#[test]
fn reset_by_me_absorbs_totals() {
    let b = MemBackend::new();
    let (mut a, _bb) = synced_pair(&b);
    let b2 = reset_by_a(&mut a);
    a.round(&b2);
    assert_eq!(own_count(&a), Some(7));
    let mut c = Dev::new("pc-c");
    c.lib = LIB2.into();
    c.round(&b2);
    assert_eq!(freq(&c), Some(7));
    assert_eq!(freq(&a), Some(7));
}

#[test]
fn rejoin_after_peer_reset_adds_only_new() {
    let b = MemBackend::new();
    let (mut a, mut bb) = synced_pair(&b);
    let b2 = reset_by_a(&mut a);
    a.round(&b2);
    // b 在旧库最后一次同步之后又学了 2 次，再用新密钥加入
    set_freq(&bb, 9);
    bb.lib = LIB2.into();
    assert_eq!(import_joined(&bb.state, &bb.lib), Some(Joined::Rejoin));
    bb.init(&b2, Joined::Rejoin);
    assert_eq!(own_count(&bb), Some(2));
    bb.round(&b2);
    a.round(&b2);
    assert_eq!((freq(&a), freq(&bb)), (Some(9), Some(9)));
}

#[test]
fn rejoin_drops_lowered_and_deleted_keys() {
    let b = MemBackend::new();
    let (mut a, mut bb) = synced_pair(&b);
    let b2 = reset_by_a(&mut a);
    a.round(&b2);
    set_freq(&bb, 5);
    bb.lib = LIB2.into();
    bb.init(&b2, Joined::Rejoin);
    let c = own_contrib(&bb, "pinyin/freq", LearnKind::Freq);
    assert!(c.rows.is_empty() && c.tombs.is_empty(), "{c:?}");
    assert_eq!(c.epoch, Gen::initial());
}

#[test]
fn claim_adopts_remote_contribution() {
    let b = MemBackend::new();
    let (_a, mut bb) = synced_pair(&b);
    // 新装的机器恢复为 pc-a；本机原有的学习数据视为已计入
    let mut c = Dev::new("pc-a");
    set_freq(&c, 10);
    c.init(&b, Joined::Claim { dev: "pc-a".into() });
    assert_eq!(own_count(&c), Some(3));
    c.round(&b);
    bb.round(&b);
    assert_eq!((freq(&c), freq(&bb)), (Some(7), Some(7)));
}

#[test]
fn state_lost_but_remote_has_mine_redownloads() {
    let b = MemBackend::new();
    let (mut a, mut bb) = synced_pair(&b);
    std::fs::remove_dir_all(a.state.join("learn")).unwrap();
    a.round(&b);
    assert_eq!(
        own_count(&a),
        Some(3),
        "沿用远端的本机贡献，不按新设备计入 7"
    );
    bb.round(&b);
    assert_eq!((freq(&a), freq(&bb)), (Some(7), Some(7)));
}

#[test]
fn private_dirs_of_same_location_share_prefix() {
    let s = Path::new("/s");
    let (d1, d2) = (private_dir_for(s, LIB), private_dir_for(s, LIB2));
    let n = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
    let (n1, n2) = (n(&d1), n(&d2));
    assert_ne!(n1, n2);
    assert_eq!(n1.split_once('-').unwrap().0, n2.split_once('-').unwrap().0);
    assert_ne!(
        n(&private_dir_for(s, "elsewhere\nk1"))
            .split_once('-')
            .unwrap()
            .0,
        n1.split_once('-').unwrap().0
    );
}

#[test]
fn purged_peer_is_dropped_by_other_devices() {
    let b = MemBackend::new();
    let (_a, mut bb) = synced_pair(&b);
    // 另一台设备清除了 pc-a 的学习数据
    wind_sync::learn::lane::purge_device(&b, "pc-a").unwrap();
    bb.round(&b);
    assert_eq!(freq(&bb), Some(4));
    let ctx = bb.ctx(None);
    let st = load_state(&ctx).unwrap();
    assert!(!st.peer_seen.contains_key("pc-a"));
    assert!(!private_dir(&ctx).join("peers/pc-a").exists());
}

/// 列表可漏掉含某串的键、写入可对含某串的键失败；stat 不受影响（真实后端是单对象请求）。
struct Faulty {
    inner: MemBackend,
    hide: Option<String>,
    fail_put: Option<String>,
    fail_delete: Option<String>,
}

impl Backend for Faulty {
    fn list(&self, prefix: &str) -> Result<Vec<ObjMeta>, BackendError> {
        let mut v = self.inner.list(prefix)?;
        if let Some(h) = &self.hide {
            v.retain(|o| !o.key.contains(h.as_str()));
        }
        Ok(v)
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, BackendError> {
        self.inner.get(key)
    }
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), BackendError> {
        if self.fail_put.as_deref().is_some_and(|f| key.contains(f)) {
            return Err(BackendError::Transient("写入失败（桩）".into()));
        }
        self.inner.put(key, bytes)
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        if self.fail_delete.as_deref().is_some_and(|f| key.contains(f)) {
            return Err(BackendError::Transient("删除失败（桩）".into()));
        }
        self.inner.delete(key)
    }
    fn stat(&self, key: &str) -> Result<Option<ObjMeta>, BackendError> {
        self.inner.stat(key)
    }
}

#[test]
fn transient_list_omission_keeps_peer() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&a, 3);
    set_comp(&a, 2);
    set_freq(&bb, 4);
    for _ in 0..2 {
        a.round(&b);
        bb.round(&b);
    }
    // b 删词立标记；a 的镜像里还有该词的旧世代行，标记不该回收
    clear_freq(&bb);
    bb.round(&b);
    assert!(
        own_contrib(&bb, "pinyin/freq", LearnKind::Freq)
            .tombs
            .contains_key(&w_key())
    );
    bb.now += wind_sync::learn::model::TOMB_MIN_AGE_SECS + 60;
    let hiding = Faulty {
        inner: b.clone(),
        hide: Some("/learn/pc-a/".into()),
        fail_put: None,
        fail_delete: None,
    };
    bb.round(&hiding);
    assert_eq!(comp(&bb), Some(2), "列表漏掉 pc-a 不应让它的次数消失");
    assert!(
        own_contrib(&bb, "pinyin/freq", LearnKind::Freq)
            .tombs
            .contains_key(&w_key())
    );
    let ctx = bb.ctx(None);
    assert!(load_state(&ctx).unwrap().peer_seen.contains_key("pc-a"));
}

#[test]
fn own_prefix_purged_is_republished() {
    let b = MemBackend::new();
    let (mut a, _bb) = synced_pair(&b);
    wind_sync::learn::lane::purge_device(&b, "pc-a").unwrap();
    a.round(&b);
    let mut c = Dev::new("pc-c");
    c.round(&b);
    assert_eq!(freq(&c), Some(7));
}

#[test]
fn rename_publish_failure_heals_next_round() {
    let b = MemBackend::new();
    let (mut a, mut bb) = synced_pair(&b);
    a.id = "pc-x".into();
    let failing = Faulty {
        inner: b.clone(),
        hide: None,
        fail_put: Some("/learn/pc-x/".into()),
        fail_delete: None,
    };
    assert!(rename_own(&a.ctx(None), &failing, &key(), true).is_err());
    let has = |dev: &str| !b.keys(&wind_sync::manifest::learn_prefix(dev)).is_empty();
    assert!(has("pc-a"), "发布失败时旧号前缀保留");
    a.round(&b);
    assert!(has("pc-x") && !has("pc-a"));
    bb.round(&b);
    assert_eq!(freq(&bb), Some(7), "不重复计入");
}

/// 模拟「目录已写成功、本地状态没存下」：退回发布前的状态（发布者标识已先落盘，保留）。
fn forget_last_publish(d: &Dev, owner: Option<&str>) {
    let ctx = d.ctx(None);
    let mut st = load_state(&ctx).unwrap();
    st.own_index = None;
    st.index_gen = 0;
    st.device.clear();
    for g in st.groups.values_mut() {
        g.last_base.clear();
    }
    if let Some(o) = owner {
        st.owner_nonce = o.into();
    }
    save(&ctx, &st).unwrap();
}

#[test]
fn first_publish_lost_locally_heals_next_round() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_freq(&a, 3);
    set_freq(&bb, 4);
    a.round(&b);
    forget_last_publish(&a, None);
    a.round(&b);
    bb.round(&b);
    a.round(&b);
    assert_eq!((freq(&a), freq(&bb)), (Some(7), Some(7)));
}

#[test]
fn foreign_owner_on_my_prefix_is_rejected() {
    let b = MemBackend::new();
    let mut a = Dev::new("pc-a");
    set_freq(&a, 3);
    a.round(&b);
    forget_last_publish(&a, Some("someone-else"));
    let e = a.round_with(&b, None).unwrap_err();
    assert!(e.contains("别处的数据"), "{e}");
}

#[test]
fn retired_own_id_not_counted_while_purge_fails() {
    let b = MemBackend::new();
    let (mut a, mut bb) = synced_pair(&b);
    a.id = "pc-x".into();
    let failing = Faulty {
        inner: b.clone(),
        hide: None,
        fail_put: None,
        fail_delete: Some("/learn/pc-a/".into()),
    };
    let ws = rename_own(&a.ctx(None), &failing, &key(), true).unwrap();
    assert!(!ws.is_empty(), "删除失败应有警告");
    set_freq(&a, 8);
    a.round(&failing);
    assert_eq!(freq(&a), Some(8), "本机不把自己的旧号当他机计入");
    a.round(&b);
    assert!(
        b.keys(&wind_sync::manifest::learn_prefix("pc-a"))
            .is_empty()
    );
    bb.round(&b);
    assert_eq!(freq(&bb), Some(8));
}

// ── 打字统计 ───────────────────────────────────────────

fn stats_file(days: &[(&str, u32)]) -> String {
    let mut s = String::from("version = 1\n");
    for (d, n) in days {
        s += &format!("\n[days.\"{d}\"]\nchinese = {n}\n");
    }
    s
}

fn set_stats(d: &Dev, name: &str, days: &[(&str, u32)]) {
    let p = d.path(&format!("stats/{name}"));
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, stats_file(days)).unwrap();
}

fn peer_stats(d: &Dev, dev: &str, name: &str) -> Option<String> {
    std::fs::read_to_string(d.path(&format!("stats/devices/{dev}/{name}"))).ok()
}

/// 远端某设备目录里的对象名。
fn remote_objects(b: &MemBackend, dev: &str) -> BTreeMap<String, String> {
    let (idx, _) = wind_sync::learn::lane::fetch_device(b, &key(), dev)
        .unwrap()
        .unwrap();
    idx.objects
}

#[test]
fn own_stats_uploaded_not_meta() {
    let b = MemBackend::new();
    let mut a = Dev::new("pc-a");
    set_stats(&a, "2026-11.toml", &[("2026-11-01", 5)]);
    std::fs::write(a.path("stats/meta.toml"), "x = 1\n").unwrap();
    std::fs::write(a.path("stats/2026-11.toml.bak"), "x").unwrap();
    std::fs::create_dir_all(a.path("stats/devices/pc-z")).unwrap();
    std::fs::write(a.path("stats/devices/pc-z/2026-10.toml"), "x").unwrap();
    a.round(&b);
    let objs = remote_objects(&b, "pc-a");
    assert_eq!(
        objs.keys().map(String::as_str).collect::<Vec<_>>(),
        ["stats/2026-11.toml"]
    );
    // 没变化不再上传；本机删除月份后对象消失
    let puts = b.put_count();
    a.round(&b);
    assert_eq!(b.put_count(), puts);
    std::fs::remove_file(a.path("stats/2026-11.toml")).unwrap();
    a.round(&b);
    assert!(remote_objects(&b, "pc-a").is_empty());
}

#[test]
fn peer_stats_written_under_devices() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_stats(&a, "2026-11.toml", &[("2026-11-01", 5)]);
    a.round(&b);
    bb.round(&b);
    assert_eq!(
        peer_stats(&bb, "pc-a", "2026-11.toml").unwrap(),
        stats_file(&[("2026-11-01", 5)])
    );
    assert!(peer_stats(&a, "pc-b", "2026-11.toml").is_none());
    // 没变化不重写
    let p = bb.path("stats/devices/pc-a/2026-11.toml");
    let m = std::fs::metadata(&p).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    bb.round(&b);
    assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), m);
    // 内容变化会更新
    set_stats(&a, "2026-11.toml", &[("2026-11-01", 6)]);
    a.round(&b);
    bb.round(&b);
    assert_eq!(
        peer_stats(&bb, "pc-a", "2026-11.toml").unwrap(),
        stats_file(&[("2026-11-01", 6)])
    );
}

#[test]
fn peer_month_deleted_removes_local() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_stats(&a, "2026-10.toml", &[("2026-10-01", 1)]);
    set_stats(&a, "2026-11.toml", &[("2026-11-01", 5)]);
    a.round(&b);
    bb.round(&b);
    assert!(peer_stats(&bb, "pc-a", "2026-10.toml").is_some());
    std::fs::remove_file(a.path("stats/2026-10.toml")).unwrap();
    a.round(&b);
    bb.round(&b);
    assert!(peer_stats(&bb, "pc-a", "2026-10.toml").is_none());
    assert!(peer_stats(&bb, "pc-a", "2026-11.toml").is_some());
}

#[test]
fn purged_device_stats_removed() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_stats(&a, "2026-11.toml", &[("2026-11-01", 5)]);
    a.round(&b);
    bb.round(&b);
    assert!(peer_stats(&bb, "pc-a", "2026-11.toml").is_some());
    wind_sync::learn::lane::purge_device(&b, "pc-a").unwrap();
    bb.round(&b);
    assert!(!bb.path("stats/devices/pc-a").exists());
}

#[test]
fn stats_switch_off_keeps_remote_and_local() {
    let b = MemBackend::new();
    let (mut a, mut bb) = (Dev::new("pc-a"), Dev::new("pc-b"));
    set_stats(&a, "2026-11.toml", &[("2026-11-01", 5)]);
    set_stats(&bb, "2026-11.toml", &[("2026-11-02", 7)]);
    a.round(&b);
    bb.round(&b);
    a.round(&b);
    assert!(peer_stats(&bb, "pc-a", "2026-11.toml").is_some());
    // a 关闭统计：远端保留旧对象，不上传新内容；b 的统计也不再写入 a
    a.content.stats = false;
    set_stats(&a, "2026-11.toml", &[("2026-11-01", 99)]);
    set_stats(&a, "2026-12.toml", &[("2026-12-01", 1)]);
    set_stats(&bb, "2026-11.toml", &[("2026-11-02", 8)]);
    bb.round(&b);
    a.round(&b);
    bb.round(&b);
    assert_eq!(
        remote_objects(&b, "pc-a").keys().collect::<Vec<_>>(),
        ["stats/2026-11.toml"]
    );
    assert_eq!(
        peer_stats(&bb, "pc-a", "2026-11.toml").unwrap(),
        stats_file(&[("2026-11-01", 5)])
    );
    assert_eq!(
        peer_stats(&a, "pc-b", "2026-11.toml").unwrap(),
        stats_file(&[("2026-11-02", 7)])
    );
    // 关闭期间学习数据照常同步
    set_freq(&a, 3);
    a.round(&b);
    assert!(remote_objects(&b, "pc-a").contains_key("stats/2026-11.toml"));
    assert!(remote_objects(&b, "pc-a").contains_key("pinyin/freq.base"));
}

#[test]
fn claim_merges_stats_by_day() {
    let b = MemBackend::new();
    let mut a = Dev::new("pc-a");
    set_stats(&a, "2026-11.toml", &[("2026-11-01", 5), ("2026-11-02", 6)]);
    set_stats(&a, "2026-10.toml", &[("2026-10-01", 1)]);
    a.round(&b);
    // 新装的机器认领 pc-a：同一天取本地，远端独有的天加入
    let mut c = Dev::new("pc-a");
    set_stats(&c, "2026-11.toml", &[("2026-11-02", 60), ("2026-11-03", 7)]);
    c.init(&b, Joined::Claim { dev: "pc-a".into() });
    let got = std::fs::read_to_string(c.path("stats/2026-11.toml")).unwrap();
    let t: toml::Table = got.parse().unwrap();
    let days = t["days"].as_table().unwrap();
    let chinese = |d: &str| days[d]["chinese"].as_integer().unwrap();
    assert_eq!(days.len(), 3);
    assert_eq!(
        (
            chinese("2026-11-01"),
            chinese("2026-11-02"),
            chinese("2026-11-03")
        ),
        (5, 60, 7)
    );
    assert!(
        c.path("stats/2026-10.toml").is_file(),
        "本地没有的月份直接落盘"
    );
    // 合并结果在下一轮上传
    c.round(&b);
    let (_, objs) = wind_sync::learn::lane::fetch_device(&b, &key(), "pc-a")
        .unwrap()
        .unwrap();
    let up: toml::Table = String::from_utf8(objs["stats/2026-11.toml"].clone())
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(up["days"].as_table().unwrap().len(), 3);
}
