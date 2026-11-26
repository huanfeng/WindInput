use std::collections::BTreeMap;
use wind_sync::learn::model::*;
use wind_sync::learn::table::{self, LearnedFile};
use wind_sync::learn::*;

const FREQ: LearnKind = LearnKind::Freq;
const DAY_MS: u64 = 24 * 3600 * 1000;

fn g(s: &str) -> Gen {
    Gen::parse(s).unwrap()
}

fn nk(kind: LearnKind, code: &str, text: &str) -> NormKey {
    norm_key(kind, &Key(code.into(), text.into()))
}

fn empty_contrib(kind: LearnKind) -> Contribution {
    Contribution {
        kind,
        epoch: Gen::initial(),
        rows: BTreeMap::new(),
        tombs: BTreeMap::new(),
    }
}

fn put_row(c: &mut Contribution, code: &str, text: &str, gen_: &str, count: u64, f: &[&str]) {
    let k = Key(code.into(), text.into());
    c.rows.insert(
        norm_key(c.kind, &k),
        (
            k,
            Row {
                gen_: g(gen_),
                count,
                fields: f.iter().map(|s| s.to_string()).collect(),
            },
        ),
    );
}

fn put_tomb(c: &mut Contribution, code: &str, text: &str, gen_: &str) {
    let k = Key(code.into(), text.into());
    c.tombs.insert(norm_key(c.kind, &k), (k, g(gen_)));
}

fn empty_file(kind: LearnKind) -> LearnedFile {
    table::parse(kind, None).unwrap()
}

fn count_of(kind: LearnKind, f: &LearnedFile, code: &str, text: &str) -> Option<u64> {
    f.rows.get(&nk(kind, code, text)).map(|(_, c, _)| *c)
}

/// 一台设备：本机贡献、当前文件 F、基线 M。
struct Dev {
    name: &'static str,
    c: Contribution,
    f: LearnedFile,
    m: LearnedFile,
}

impl Dev {
    fn new(name: &'static str) -> Dev {
        Dev {
            name,
            c: empty_contrib(FREQ),
            f: empty_file(FREQ),
            m: empty_file(FREQ),
        }
    }

    fn learn(&mut self, code: &str, text: &str, n: u64, t: u64) {
        let k = Key(code.into(), text.into());
        let e = self
            .f
            .rows
            .entry(norm_key(FREQ, &k))
            .or_insert((k, 0, vec![String::new()]));
        e.1 += n;
        e.2 = vec![t.to_string()];
    }

    fn set_count(&mut self, code: &str, text: &str, n: u64) {
        self.f.rows.get_mut(&nk(FREQ, code, text)).unwrap().1 = n;
    }

    fn remove(&mut self, code: &str, text: &str) {
        self.f.rows.remove(&nk(FREQ, code, text));
    }

    fn commit(&mut self, now_ms: u64, peers: &[&Contribution]) {
        diff(&mut self.c, &self.f, &self.m, now_ms, self.name, peers);
        self.m = self.f.clone();
    }

    fn sync(&mut self, peers: &[&Contribution]) {
        absorb_peer_deletes(&mut self.c, peers);
        let mut all = vec![&self.c];
        all.extend_from_slice(peers);
        self.f = materialize(FREQ, &all, &self.f);
        self.m = self.f.clone();
    }
}

fn mat(all: &[&Contribution]) -> LearnedFile {
    materialize(FREQ, all, &empty_file(FREQ))
}

#[test]
fn two_devices_sum() {
    let (mut a, mut b) = (Dev::new("a"), Dev::new("b"));
    a.learn("ni hao", "你好", 10, 100);
    a.commit(1000, &[]);
    b.learn("nihao", "你好", 10, 200);
    b.commit(1000, &[]);
    let f = mat(&[&a.c, &b.c]);
    assert_eq!(count_of(FREQ, &f, "nihao", "你好"), Some(20));
    assert_eq!(f.rows[&nk(FREQ, "nihao", "你好")].2, vec!["200"]);
}

#[test]
fn repeat_and_reorder_idempotent() {
    let mut a = empty_contrib(FREQ);
    put_row(&mut a, "ni", "你", "0", 3, &["100"]);
    put_row(&mut a, "hao", "好", "50:a", 2, &["300"]);
    put_tomb(&mut a, "hao", "好", "50:a");
    let mut b = empty_contrib(FREQ);
    put_row(&mut b, "ni", "你", "0", 4, &["200"]);
    put_row(&mut b, "hao", "好", "0", 9, &["900"]);
    put_row(&mut b, "men", "们", "0", 1, &["50"]);
    let mut c = empty_contrib(FREQ);
    c.epoch = g("10:c");
    put_row(&mut c, "men", "们", "10:c", 5, &["60"]);
    put_row(&mut c, "hao", "好", "50:a", 1, &["400"]);

    let perms: [[&Contribution; 3]; 6] = [
        [&a, &b, &c],
        [&a, &c, &b],
        [&b, &a, &c],
        [&b, &c, &a],
        [&c, &a, &b],
        [&c, &b, &a],
    ];
    let first = mat(&perms[0]);
    assert_eq!(count_of(FREQ, &first, "hao", "好"), Some(3));
    assert_eq!(count_of(FREQ, &first, "men", "们"), Some(5));
    assert_eq!(count_of(FREQ, &first, "ni", "你"), None);
    for p in &perms {
        for _ in 0..3 {
            assert_eq!(mat(p), first);
        }
    }
}

#[test]
fn delete_voids_old_gen_everywhere() {
    let (mut a, mut b) = (Dev::new("a"), Dev::new("b"));
    // 留一个别的键，否则删掉唯一的键等于清空
    a.learn("x", "叉", 1, 100);
    a.learn("ni", "你", 10, 100);
    a.commit(1000, &[]);
    b.learn("ni", "你", 10, 100);
    b.commit(1000, &[]);
    a.sync(&[&b.c]);
    assert_eq!(count_of(FREQ, &a.f, "ni", "你"), Some(20));

    a.remove("ni", "你");
    a.commit(2000, &[&b.c]);
    assert!(!a.c.rows.contains_key(&nk(FREQ, "ni", "你")));
    assert!(a.c.tombs.contains_key(&nk(FREQ, "ni", "你")));
    assert_eq!(count_of(FREQ, &mat(&[&a.c, &b.c]), "ni", "你"), None);

    absorb_peer_deletes(&mut b.c, &[&a.c]);
    assert!(b.c.rows.is_empty());
}

#[test]
fn relearn_after_delete_starts_from_one() {
    let (mut a, mut b) = (Dev::new("a"), Dev::new("b"));
    a.learn("ni", "你", 10, 100);
    a.commit(1000, &[]);
    b.learn("ni", "你", 10, 100);
    b.commit(1000, &[]);
    a.sync(&[&b.c]);
    a.remove("ni", "你");
    a.commit(2000, &[&b.c]);
    a.sync(&[&b.c]);

    a.learn("ni", "你", 1, 300);
    a.commit(3000, &[&b.c]);
    assert_eq!(count_of(FREQ, &mat(&[&a.c, &b.c]), "ni", "你"), Some(1));

    b.sync(&[&a.c]);
    assert_eq!(count_of(FREQ, &b.f, "ni", "你"), Some(1));
}

#[test]
fn offline_learning_before_knowing_delete_is_dropped() {
    let (mut a, mut b) = (Dev::new("a"), Dev::new("b"));
    a.learn("ni", "你", 10, 100);
    a.commit(1000, &[]);
    b.learn("ni", "你", 10, 100);
    b.commit(1000, &[]);
    a.sync(&[&b.c]);
    b.sync(&[&a.c]);

    a.remove("ni", "你");
    a.commit(2000, &[&b.c]);
    // b 没见到删除，旧世代行 10 → 11
    let a_before = empty_contrib(FREQ);
    b.learn("ni", "你", 1, 200);
    b.commit(2500, &[&a_before]);
    assert_eq!(b.c.rows[&nk(FREQ, "ni", "你")].1.count, 11);

    assert_eq!(count_of(FREQ, &mat(&[&a.c, &b.c]), "ni", "你"), None);
    b.sync(&[&a.c]);
    assert_eq!(count_of(FREQ, &b.f, "ni", "你"), None);
    assert!(b.c.rows.is_empty());
}

#[test]
fn slow_clock_delete_still_wins() {
    // b 曾在 9000 调低，当前有效世代 9000:b；a 的时钟只有 1000
    let mut b = empty_contrib(FREQ);
    put_tomb(&mut b, "ni", "你", "9000:b");
    put_row(&mut b, "ni", "你", "9000:b", 5, &["100"]);
    put_row(&mut b, "x", "叉", "0", 1, &["100"]);
    let mut a = Dev::new("a");
    a.sync(&[&b]);
    assert_eq!(count_of(FREQ, &a.f, "ni", "你"), Some(5));

    a.remove("ni", "你");
    a.commit(1000, &[&b]);
    assert_eq!(a.c.tombs[&nk(FREQ, "ni", "你")].1, g("9001:a"));
    assert_eq!(count_of(FREQ, &mat(&[&a.c, &b]), "ni", "你"), None);
}

#[test]
fn lower_count_bumps_generation() {
    let (mut a, mut b) = (Dev::new("a"), Dev::new("b"));
    a.learn("ni", "你", 10, 100);
    a.commit(1000, &[]);
    b.learn("ni", "你", 10, 100);
    b.commit(1000, &[]);
    a.sync(&[&b.c]);
    assert_eq!(count_of(FREQ, &a.f, "ni", "你"), Some(20));

    a.set_count("ni", "你", 5);
    a.commit(2000, &[&b.c]);
    assert_eq!(count_of(FREQ, &mat(&[&a.c, &b.c]), "ni", "你"), Some(5));
    b.sync(&[&a.c]);
    assert_eq!(count_of(FREQ, &b.f, "ni", "你"), Some(5));
}

#[test]
fn raising_count_adds_difference() {
    let (mut a, mut b) = (Dev::new("a"), Dev::new("b"));
    b.learn("ni", "你", 10, 100);
    b.commit(1000, &[]);
    a.sync(&[&b.c]);
    a.learn("ni", "你", 3, 500);
    a.commit(2000, &[&b.c]);
    let row = &a.c.rows[&nk(FREQ, "ni", "你")].1;
    assert_eq!((row.count, row.gen_.clone()), (3, Gen::initial()));
    let f = mat(&[&a.c, &b.c]);
    assert_eq!(count_of(FREQ, &f, "ni", "你"), Some(13));
    assert_eq!(f.rows[&nk(FREQ, "ni", "你")].2, vec!["500"]);
}

#[test]
fn clear_bumps_epoch() {
    let mut b = empty_contrib(FREQ);
    put_row(&mut b, "x", "叉", "0", 7, &["1"]);
    let mut a = Dev::new("a");
    for (code, text) in [("ni", "你"), ("hao", "好"), ("men", "们")] {
        a.learn(code, text, 2, 100);
    }
    a.commit(1000, &[&b]);
    a.sync(&[&b]);
    assert_eq!(a.f.rows.len(), 4);
    let old_epoch = a.c.epoch.clone();

    a.f.rows.clear();
    a.commit(2000, &[&b]);
    assert!(a.c.epoch > old_epoch);
    assert!(a.c.rows.is_empty());
    assert!(a.c.tombs.is_empty());
    assert!(mat(&[&a.c, &b]).rows.is_empty());
}

#[test]
fn clear_epoch_beats_known_peer_generations() {
    // b 的时钟快：删除后重学在 9_000_000:b；a 知道它，用 1000 的时钟清空
    let mut b = empty_contrib(FREQ);
    put_tomb(&mut b, "ni", "你", "9000000:b");
    put_row(&mut b, "ni", "你", "9000000:b", 1, &["1"]);
    let mut a = Dev::new("a");
    a.sync(&[&b]);
    assert_eq!(a.f.rows.len(), 1);
    a.f.rows.clear();
    a.commit(1000, &[&b]);
    assert!(mat(&[&a.c, &b]).rows.is_empty());
}

#[test]
fn fields_merge() {
    let mut a = empty_contrib(FREQ);
    put_row(&mut a, "ni", "你", "0", 1, &["1000"]);
    put_row(&mut a, "hao", "好", "0", 1, &["900"]);
    let mut b = empty_contrib(FREQ);
    put_row(&mut b, "ni", "你", "0", 1, &["2000"]);
    put_row(&mut b, "hao", "好", "0", 1, &["1000"]);
    let f = mat(&[&a, &b]);
    assert_eq!(f.rows[&nk(FREQ, "ni", "你")].2, vec!["2000"]);
    // 按数值比较，不按字符串
    assert_eq!(f.rows[&nk(FREQ, "hao", "好")].2, vec!["1000"]);

    let t = LearnKind::Temp;
    let mut a = empty_contrib(t);
    put_row(&mut a, "ni", "你", "0", 1, &["3", "100"]);
    let mut b = empty_contrib(t);
    put_row(&mut b, "ni", "你", "0", 2, &["7", "50"]);
    let f = materialize(t, &[&a, &b], &empty_file(t));
    let (_, count, fields) = &f.rows[&nk(t, "ni", "你")];
    assert_eq!(*count, 3);
    assert_eq!(fields, &vec!["7".to_string(), "50".to_string()]);
}

#[test]
fn temp_weight_lowered_bumps_generation() {
    let t = LearnKind::Temp;
    let mut b = empty_contrib(t);
    put_row(&mut b, "ni", "你", "0", 4, &["9", "100"]);
    let mut me = empty_contrib(t);
    let m = materialize(t, &[&me, &b], &empty_file(t));
    let mut f = m.clone();
    f.rows.get_mut(&nk(t, "ni", "你")).unwrap().2 = vec!["2".into(), "100".into()];
    diff(&mut me, &f, &m, 5000, "a", &[&b]);
    let out = materialize(t, &[&me, &b], &empty_file(t));
    let (_, count, fields) = &out.rows[&nk(t, "ni", "你")];
    assert_eq!(*count, 4);
    assert_eq!(fields[0], "2");
}

#[test]
fn materialize_keeps_template_head_and_spelling() {
    let mut a = empty_contrib(FREQ);
    put_row(&mut a, "nihao", "你好", "0", 2, &["1"]);
    let mut tpl = empty_file(FREQ);
    tpl.head = "---\ncolumns: [code, text, count, last_used]\n...\n".into();
    tpl.rows.insert(
        nk(FREQ, "ni hao", "你好"),
        (Key("ni hao".into(), "你好".into()), 1, vec!["1".into()]),
    );
    let f = materialize(FREQ, &[&a], &tpl);
    assert_eq!(f.head, tpl.head);
    assert_eq!(f.columns, tpl.columns);
    assert_eq!(f.rows[&nk(FREQ, "nihao", "你好")].0.0, "ni hao");
}

#[test]
fn diff_output_is_normalized() {
    let mut me = empty_contrib(FREQ);
    me.epoch = g("100:a");
    put_row(&mut me, "old", "旧", "0", 3, &["1"]);
    put_tomb(&mut me, "old2", "旧二", "50:a");
    let f = empty_file(FREQ);
    diff(&mut me, &f, &f, 200, "a", &[]);
    assert!(me.rows.is_empty());
    assert!(me.tombs.is_empty());
}

#[test]
fn gc_waits_for_all_peers() {
    let t0 = 1_000_000_000_000;
    let tomb = format!("{t0}:a");
    let mut a = empty_contrib(FREQ);
    put_tomb(&mut a, "ni", "你", &tomb);
    let mut b = empty_contrib(FREQ);
    put_row(&mut b, "ni", "你", "0", 3, &["1"]);
    let c = empty_contrib(FREQ);

    gc_tombs(&mut a, &[&b, &c], t0 + 31 * DAY_MS);
    assert_eq!(a.tombs.len(), 1, "b 仍有旧世代行");

    b.rows.clear();
    gc_tombs(&mut a, &[&b, &c], t0 + 29 * DAY_MS);
    assert_eq!(a.tombs.len(), 1, "不满 30 天");

    gc_tombs(&mut a, &[&b, &c], t0 + 30 * DAY_MS);
    assert!(a.tombs.is_empty());
}

// ---- 随机化：与按世代逐事件回放的参照实现对拍 ----

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const DEVS: [&str; 3] = ["a", "b", "c"];
const KEYS: usize = 6;

fn key_of(i: usize) -> Key {
    Key(format!("k{i}"), format!("字{i}"))
}

/// 参照模型的事件：学习、删除某键、清空整类。
#[derive(Clone)]
enum Ev {
    Learn {
        key: usize,
        gen_: Gen,
        n: u64,
        t: u64,
    },
    Del {
        key: usize,
        gen_: Gen,
    },
    Clear {
        gen_: Gen,
    },
}

struct Sim {
    devs: Vec<Dev>,
    mirrors: Vec<BTreeMap<usize, Contribution>>,
    remote: Vec<Option<Contribution>>,
    /// 参照：各设备自己产生的事件。
    events: Vec<Vec<Ev>>,
    /// 参照：remote_len[d] = d 上次发布时的事件数。
    remote_len: Vec<usize>,
    /// 参照：known[d][p] = d 已知的 p 的事件数（d 自己的永远全知）。
    known: Vec<Vec<usize>>,
}

/// 各设备时钟偏移：a 准、b 慢、c 快到「2099 年」。
fn now_of(d: usize, step: u64) -> u64 {
    let base = 2_000_000 + step * 1000;
    match d {
        0 => base,
        1 => base - 1_900_000,
        _ => base + 4_000_000_000_000,
    }
}

impl Sim {
    fn new() -> Sim {
        Sim {
            devs: DEVS.iter().map(|n| Dev::new(n)).collect(),
            mirrors: vec![BTreeMap::new(); 3],
            remote: vec![None; 3],
            events: vec![Vec::new(); 3],
            remote_len: vec![0; 3],
            known: vec![vec![0; 3]; 3],
        }
    }

    fn known_events(&self, d: usize) -> Vec<Ev> {
        (0..3)
            .flat_map(|p| {
                let n = if p == d {
                    self.events[p].len()
                } else {
                    self.known[d][p]
                };
                self.events[p][..n].iter().cloned()
            })
            .collect()
    }

    fn commit(&mut self, d: usize, now: u64) {
        let peers: Vec<Contribution> = self.mirrors[d].values().cloned().collect();
        let refs: Vec<&Contribution> = peers.iter().collect();
        self.devs[d].commit(now, &refs);
    }

    fn sync_pair(&mut self, i: usize, j: usize) {
        for x in [i, j] {
            self.remote[x] = Some(self.devs[x].c.clone());
            self.remote_len[x] = self.events[x].len();
        }
        for x in [i, j] {
            for p in 0..3 {
                if p == x {
                    continue;
                }
                if let Some(c) = &self.remote[p] {
                    self.mirrors[x].insert(p, c.clone());
                }
                self.known[x][p] = self.remote_len[p];
            }
            let peers: Vec<Contribution> = self.mirrors[x].values().cloned().collect();
            let refs: Vec<&Contribution> = peers.iter().collect();
            self.devs[x].sync(&refs);
        }
    }
}

fn ref_eff(evs: &[Ev], key: usize) -> Gen {
    let mut g = Gen::initial();
    for e in evs {
        let cand = match e {
            Ev::Del { key: k, gen_ } if *k == key => gen_,
            Ev::Clear { gen_ } => gen_,
            _ => continue,
        };
        if *cand > g {
            g = cand.clone();
        }
    }
    g
}

fn ref_max(evs: &[Ev]) -> Gen {
    evs.iter()
        .map(|e| match e {
            Ev::Learn { gen_, .. } | Ev::Del { gen_, .. } | Ev::Clear { gen_ } => gen_.clone(),
        })
        .max()
        .unwrap_or_default()
}

/// 参照视图：每键只数有效世代上的学习事件。
fn ref_view(evs: &[Ev]) -> BTreeMap<usize, (u64, u64)> {
    let mut out = BTreeMap::new();
    for key in 0..KEYS {
        let eff = ref_eff(evs, key);
        let mut hit: Option<(u64, u64)> = None;
        for e in evs {
            if let Ev::Learn { key: k, gen_, n, t } = e
                && *k == key
                && *gen_ == eff
            {
                let h = hit.get_or_insert((0, 0));
                h.0 += n;
                h.1 = h.1.max(*t);
            }
        }
        if let Some(h) = hit {
            out.insert(key, h);
        }
    }
    out
}

fn file_view(f: &LearnedFile) -> BTreeMap<usize, (u64, u64)> {
    (0..KEYS)
        .filter_map(|i| {
            f.rows
                .get(&norm_key(FREQ, &key_of(i)))
                .map(|(_, c, fs)| (i, (*c, fs[0].parse().unwrap())))
        })
        .collect()
}

fn run_random(seed: u64) {
    let mut rng = XorShift(seed);
    let mut sim = Sim::new();
    for step in 1..=200u64 {
        let t = step;
        let d = rng.below(3) as usize;
        let now = now_of(d, step);
        let present: Vec<usize> = (0..KEYS)
            .filter(|i| {
                sim.devs[d]
                    .f
                    .rows
                    .contains_key(&norm_key(FREQ, &key_of(*i)))
            })
            .collect();
        let known = sim.known_events(d);
        match rng.below(10) {
            0..=3 => {
                let key = rng.below(KEYS as u64) as usize;
                let n = 1 + rng.below(3);
                let k = key_of(key);
                sim.devs[d].learn(&k.0, &k.1, n, t);
                let gen_ = ref_eff(&known, key);
                sim.events[d].push(Ev::Learn { key, gen_, n, t });
                sim.commit(d, now);
            }
            4 if !present.is_empty() => {
                let key = present[rng.below(present.len() as u64) as usize];
                let k = key_of(key);
                sim.devs[d].remove(&k.0, &k.1);
                // 删掉最后一个键时文件变空，按规则就是清空
                let ev = if present.len() == 1 {
                    Ev::Clear {
                        gen_: Gen::next(&ref_max(&known), now, DEVS[d]),
                    }
                } else {
                    Ev::Del {
                        key,
                        gen_: Gen::next(&ref_eff(&known, key), now, DEVS[d]),
                    }
                };
                sim.events[d].push(ev);
                sim.commit(d, now);
            }
            5 if !present.is_empty() => {
                let key = present[rng.below(present.len() as u64) as usize];
                let k = key_of(key);
                let cur = sim.devs[d].f.rows[&norm_key(FREQ, &k)].1;
                if cur == 0 {
                    continue;
                }
                let v = rng.below(cur);
                let e = sim.devs[d].f.rows.get_mut(&norm_key(FREQ, &k)).unwrap();
                e.1 = v;
                e.2 = vec![t.to_string()];
                let gen_ = Gen::next(&ref_eff(&known, key), now, DEVS[d]);
                sim.events[d].push(Ev::Del {
                    key,
                    gen_: gen_.clone(),
                });
                sim.events[d].push(Ev::Learn { key, gen_, n: v, t });
                sim.commit(d, now);
            }
            6 if !present.is_empty() && rng.below(3) == 0 => {
                sim.devs[d].f.rows.clear();
                sim.events[d].push(Ev::Clear {
                    gen_: Gen::next(&ref_max(&known), now, DEVS[d]),
                });
                sim.commit(d, now);
            }
            _ => {
                let j = (d + 1 + rng.below(2) as usize) % 3;
                sim.sync_pair(d, j);
            }
        }

        // 各设备的文件 = 参照模型按其已知事件回放的结果
        for x in 0..3 {
            assert_eq!(
                file_view(&sim.devs[x].f),
                ref_view(&sim.known_events(x)),
                "seed {seed} step {step} dev {x}"
            );
        }

        // 交换全部贡献后：各设备物化一致，且等于全部事件的回放
        let all_events: Vec<Ev> = sim.events.iter().flatten().cloned().collect();
        let want = ref_view(&all_events);
        let mut first: Option<LearnedFile> = None;
        for x in 0..3 {
            let mut me = sim.devs[x].c.clone();
            let others: Vec<&Contribution> =
                (0..3).filter(|p| *p != x).map(|p| &sim.devs[p].c).collect();
            absorb_peer_deletes(&mut me, &others);
            let mut all = vec![&me];
            all.extend(others);
            let f = materialize(FREQ, &all, &empty_file(FREQ));
            assert_eq!(file_view(&f), want, "seed {seed} step {step} converge {x}");
            match &first {
                None => first = Some(f),
                Some(f0) => assert_eq!(&f, f0, "seed {seed} step {step}"),
            }
        }

        // 输入顺序与重复调用不影响物化
        let cs: Vec<&Contribution> = sim.devs.iter().map(|d| &d.c).collect();
        let base = mat(&cs);
        for p in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let perm: Vec<&Contribution> = p.iter().map(|&i| cs[i]).collect();
            assert_eq!(mat(&perm), base, "seed {seed} step {step} order");
        }
        assert_eq!(mat(&cs), base);
    }
}

#[test]
fn randomized_model_matches_replay() {
    for seed in 1..=40u64 {
        run_random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    }
}
