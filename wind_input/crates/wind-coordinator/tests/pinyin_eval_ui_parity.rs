//! `pinyin_eval` 界面序与协调器对拍：读评测写出的 `<out>.ranks.tsv`，逐条敲进 `Coordinator`，
//! 比对界面首选与期望词位次。
//!
//! ```text
//! WIND_UI_PARITY_TSV=target/pinyin_eval/latest.ranks.tsv \
//!   cargo test -p wind-coordinator --test pinyin_eval_ui_parity -- --ignored --nocapture
//! ```
//!
//! - `WIND_UI_PARITY_CLASSES` 只对拍这几类（逗号分隔的类名前缀，如 `E,F,G`），默认全部；
//! - `WIND_PINYIN_EVAL_CORRECTION=off` 关纠错（须与产出 TSV 的那次评测一致；只认 `off` 与不设）。
//!
//! 含非字母的击键串（`x'ai`）跳过：这里按虚拟键码敲，`'` 不是它的键码。
//!
//! 已知差异：B 类（单音节串）约 2% 位次差 1~8 位——协调器的检索范围过滤滤掉了生僻单字
//! （`rua` 的「挼」），评测不模拟这一步。只看纠错 / 排序时用 `WIND_UI_PARITY_CLASSES=A,C,D,E,F,G`。

use std::path::PathBuf;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

const VK_ESCAPE: u32 = 0x1B;

fn key(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

#[test]
#[ignore = "对拍：需要 pinyin_eval 产出的 ranks.tsv 与 build_dev 真实词库"]
fn pinyin_eval_ui_order_matches_coordinator() {
    let Ok(tsv) = std::env::var("WIND_UI_PARITY_TSV") else {
        eprintln!("跳过：未设 WIND_UI_PARITY_TSV");
        return;
    };
    let raw = std::fs::read_to_string(&tsv).expect("读 ranks.tsv");
    let classes: Option<Vec<String>> = std::env::var("WIND_UI_PARITY_CLASSES")
        .ok()
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect());

    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    let mut c = Config::default();
    c.schema.available = vec!["pinyin".into()];
    c.schema.active = "pinyin".into();
    c.input.default.chinese_mode = true;
    if std::env::var("WIND_PINYIN_EVAL_CORRECTION").is_ok_and(|v| v == "off") {
        c.input.correction.enabled = false;
    }
    let coord = Coordinator::new_headless(c, Some(&data));

    let (mut checked, mut skipped) = (0usize, 0usize);
    let mut diffs: Vec<String> = Vec::new();
    for line in raw.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        let [
            class,
            _form,
            input,
            expect,
            _eng,
            ui_rank,
            _corr,
            ui_top1,
            ..,
        ] = f[..]
        else {
            continue;
        };
        if classes
            .as_ref()
            .is_some_and(|cs| !cs.iter().any(|p| class.starts_with(p.as_str())))
        {
            continue;
        }
        if !input.bytes().all(|b| b.is_ascii_lowercase()) {
            skipped += 1;
            continue;
        }
        for ch in input.chars() {
            coord.handle_key_event(&key(ch.to_ascii_uppercase() as u32));
        }
        let all = coord.debug_all_candidate_texts();
        coord.handle_key_event(&key(VK_ESCAPE));
        checked += 1;
        let got_rank = all
            .iter()
            .position(|t| t == expect)
            .map_or("-".to_string(), |r| r.to_string());
        let got_top1 = all.first().map(String::as_str).unwrap_or("");
        if got_rank != ui_rank || got_top1 != ui_top1 {
            diffs.push(format!(
                "{class} {input} 期望 {expect}：评测 位次 {ui_rank} 首选 {ui_top1} / 协调器 位次 {got_rank} 首选 {got_top1}"
            ));
        }
    }
    println!(
        "对拍 {checked} 条（跳过 {skipped}），不一致 {} 条",
        diffs.len()
    );
    for d in diffs.iter().take(200) {
        println!("  {d}");
    }
    assert!(diffs.is_empty(), "界面序与协调器不一致 {} 条", diffs.len());
}
