use super::*;

fn syls(s: &str) -> Vec<String> {
    s.split(' ').map(str::to_string).collect()
}

fn table(body: &str) -> MisreadTable {
    MisreadTable::parse(&format!("---\nname: t\nversion: \"1\"\n...\n{body}"), "t")
}

#[test]
fn parses_flow_entries_after_meta_head() {
    let t = table(
        "# 形声字\n\
             - {text: 莘莘, wrong: xin xin, right: shen shen}\n\
             - {text: 酗酒, wrong: xiong jiu, right: xu jiu, note: 酗读xù, group: 形声}\n",
    );
    assert_eq!(t.len(), 2);
    let e = t.lookup("莘莘", &["xin", "xin"]).expect("应收");
    assert_eq!(e.correct, syls("shen shen"));
    assert!(
        t.lookup("酗酒", &["xiongjiu"]).is_some(),
        "切法不论、未知字段忽略"
    );
}

#[test]
fn wrong_list_expands_to_one_entry_each() {
    let t = table("- {text: 炽热, wrong: [zhi re, zhi le], right: chi re}\n");
    assert_eq!(t.len(), 2);
    assert!(t.lookup("炽热", &["zhire"]).is_some());
    assert!(t.lookup("炽热", &["zhile"]).is_some());
}

#[test]
fn bad_entries_are_skipped() {
    let t = table(
        "- {text: 馄饨, wrong: hun dun}\n\
             - {text: 炽热, wrong: zhi, right: chi re}\n\
             - {text: 龋齿, wrong: yu chi, right: qu}\n\
             - {text: 呆板, wrong: dai ban, right: dai ban}\n\
             - {text: 惬意, wrong: [], right: qie yi}\n\
             - {text: 酗酒, wrong: xiong9 jiu, right: xu jiu}\n\
             - 一行字符串\n\
             - {text: 莘莘, wrong: xin xin, right: shen shen}\n",
    );
    assert_eq!(
        t.len(),
        1,
        "缺字段、音节数不等、错读同正读、空列表、非字母、非映射都跳过"
    );
    assert!(t.has_text("莘莘"));
}

#[test]
fn broken_yaml_loses_only_bad_lines() {
    let t = table(
        "- {text: 莘莘, wrong: xin xin, right: shen shen}\n\
             - {text: 馄饨, wrong: hun dun, right: [hun tun\n\
             - {text: 酗酒, wrong: xiong jiu, right: xu jiu}\n",
    );
    assert_eq!(t.len(), 2);
    assert!(t.has_text("酗酒"));
}

#[test]
fn entries_in_meta_head_are_detected() {
    let src = "---\nname: t\n- {text: 莘莘, wrong: xin xin, right: shen shen}\n...\n";
    let (head, body) = split_head_body(src);
    assert!(head_has_entries(head));
    assert!(body.is_empty());
    assert!(
        !head_has_entries("---\nname: t\n# - {text: 例}\n"),
        "注释不算"
    );
}

#[test]
fn without_meta_head_whole_file_is_body() {
    let t = MisreadTable::parse("- {text: 莘莘, wrong: xin xin, right: shen shen}\n", "t");
    assert_eq!(t.len(), 1);
}

#[test]
fn user_table_overrides_builtin() {
    let builtin = table(
        "- {text: 莘莘, wrong: xin xin, right: shen shen}\n\
             - {text: 馄饨, wrong: hun dun, right: hun tun}\n",
    );
    let user = table(
        "- {text: 馄饨, wrong: hun dun, right: hun tun, note: 用户改}\n\
             - {text: 炽热, wrong: zhi re, right: chi re}\n\
             - {text: 莘莘, wrong: xin xin, right: xin xin}\n",
    );
    let t = MisreadTable::merge(builtin, user);
    assert_eq!(t.len(), 3, "同键覆盖、新键追加、坏项跳过");
    assert!(t.lookup("炽热", &["zhi", "re"]).is_some());
    assert_eq!(
        t.lookup("莘莘", &["xin", "xin"]).unwrap().correct,
        syls("shen shen"),
        "用户坏项不覆盖内置"
    );
}

#[test]
fn recall_by_whole_wrong_code() {
    let t = table(
        "- {text: 堤防, wrong: ti fang, right: di fang}\n\
             - {text: 提防, wrong: ti fang, right: di fang}\n\
             - {text: 莘莘, wrong: xin xin, right: shen shen}\n",
    );
    let mut got: Vec<&str> = t.recall("tifang").map(|e| e.text.as_str()).collect();
    got.sort();
    assert_eq!(got, ["堤防", "提防"]);
    assert_eq!(t.recall("xin").count(), 0, "只认整串");
}

#[test]
fn lookup_needs_both_text_and_reading() {
    let t = table("- {text: 莘莘, wrong: xin xin, right: shen shen}\n");
    assert!(t.lookup("莘莘", &["shen", "shen"]).is_none());
    assert!(t.lookup("欣欣", &["xin", "xin"]).is_none());
}

#[test]
fn umlaut_is_written_as_v() {
    let t = table("- {text: 戏谑, wrong: xi nüe, right: xi xue}\n");
    assert!(t.lookup("戏谑", &["xinve"]).is_some());
}

fn builtin_src() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../data/schemas/pinyin_misread.yaml"
    );
    std::fs::read_to_string(path).expect("内置误读表应存在")
}

/// 内置表每项都收得进来（坏项会被跳过，条数对不上就是有坏项），且没有重复的「文字 + 错误读音」。
#[test]
fn builtin_table_parses_cleanly() {
    let src = builtin_src();
    let items: Vec<serde_yaml::Value> =
        serde_yaml::from_str(split_head_body(&src).1).expect("内置表应是合法 YAML 列表");
    let expanded: usize = items
        .iter()
        .map(|v| match v.get("wrong") {
            Some(serde_yaml::Value::Sequence(ws)) => ws.len(),
            _ => 1,
        })
        .sum();
    let t = MisreadTable::parse(&src, "builtin");
    assert_eq!(t.len(), expanded, "内置表有坏项");
    let mut keys = std::collections::HashSet::new();
    let dup: Vec<String> = t
        .entries
        .iter()
        .filter(|e| !keys.insert((e.text.clone(), e.wrong_code())))
        .map(|e| e.text.clone())
        .collect();
    assert!(dup.is_empty(), "重复条目：{dup:?}");
    assert!(t.len() >= 400, "内置表 {} 条", t.len());
}

/// 错误拼写连写后撞上的词库词权重达到这个值就不收（打常用词时召回项会混进首页）。
const HIGH_FREQ_WEIGHT: i64 = 5000;

/// 撞上高频词也照收的条目：(文字, 错误读音, 理由)。加条目须写明为什么值得。
const HIGH_FREQ_EXEMPT: &[(&str, &str, &str)] = &[];

/// 内置表对照随包词库（`build_dev/data` 的 cn_dicts，不含错音表 `corrections` 与 `others` 的
/// 「错音错字提示」段——那两处收的是错误读音，供提示用，不是合法读音）：
/// 1. 每条的正确读音是词库登记的读音；
/// 2. 错误读音**不是**该词登记的读音（否则那是合法读法，不该标错）；
/// 3. 错误拼写连写后，词库里别的词在这个码下的最高权重 < [`HIGH_FREQ_WEIGHT`]
///    （豁免见 [`HIGH_FREQ_EXEMPT`]）。
///
/// 读大词库较慢且依赖本地数据，故 `#[ignore]`；改表后手动跑：
/// `cargo test -p wind-engine --lib builtin_table_matches_dict_readings -- --ignored --nocapture`
#[test]
#[ignore]
fn builtin_table_matches_dict_readings() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../");
    let table = MisreadTable::parse(&builtin_src(), "builtin");
    let wrong_codes: std::collections::HashSet<String> =
        table.entries.iter().map(|e| e.wrong_code()).collect();
    let dicts = format!("{root}build_dev/data/schemas/pinyin/cn_dicts");
    let mut readings: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    // 错误拼写连写 → 该码下的 (权重, 文字)。
    let mut by_code: HashMap<String, Vec<(i64, String)>> = HashMap::new();
    for entry in std::fs::read_dir(&dicts).expect("需要 build_dev/data 词库") {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.ends_with(".dict.yaml") || name.starts_with("corrections") {
            continue;
        }
        let mut in_misread_section = false;
        for line in std::fs::read_to_string(&path).unwrap().lines() {
            if line.starts_with("#####") {
                in_misread_section = line.contains("错音错字提示");
            }
            if in_misread_section {
                continue;
            }
            let mut cols = line.split('\t');
            let (Some(t), Some(code)) = (cols.next(), cols.next()) else {
                continue;
            };
            if table.has_text(t) {
                readings
                    .entry(t.to_string())
                    .or_default()
                    .insert(code.to_string());
            }
            let joined = code.replace(' ', "");
            if wrong_codes.contains(&joined) {
                let w = cols.next().and_then(|w| w.trim().parse().ok()).unwrap_or(0);
                by_code.entry(joined).or_default().push((w, t.to_string()));
            }
        }
    }
    let has = |e: &MisreadEntry, r: &[String]| {
        readings
            .get(&e.text)
            .is_some_and(|set| set.contains(&r.join(" ")))
    };
    let missing: Vec<String> = table
        .entries
        .iter()
        .filter(|e| !has(e, &e.correct))
        .map(|e| format!("{} {}", e.text, e.correct.join(" ")))
        .collect();
    let legit_wrong: Vec<String> = table
        .entries
        .iter()
        .filter(|e| has(e, &e.wrong))
        .map(|e| format!("{} {}", e.text, e.wrong.join(" ")))
        .collect();
    let high_freq: Vec<String> = table
        .entries
        .iter()
        .filter(|e| {
            let wrong = e.wrong.join(" ");
            !HIGH_FREQ_EXEMPT
                .iter()
                .any(|(t, w, _)| *t == e.text && *w == wrong)
        })
        .filter_map(|e| {
            let (w, t) = by_code
                .get(&e.wrong_code())?
                .iter()
                .filter(|(_, t)| *t != e.text)
                .max()?;
            (*w >= HIGH_FREQ_WEIGHT)
                .then(|| format!("{} {} 撞「{t}」({w})", e.text, e.wrong.join(" ")))
        })
        .collect();
    println!(
        "核对 {} 条：词库缺正确读音 {} 条，错误读音是词库读音 {} 条，错误拼写撞高频词 {} 条",
        table.len(),
        missing.len(),
        legit_wrong.len(),
        high_freq.len()
    );
    assert!(missing.is_empty(), "词库里没有这些正确读音：{missing:?}");
    assert!(
        legit_wrong.is_empty(),
        "这些「错误读音」词库有登记：{legit_wrong:?}"
    );
    assert!(
        high_freq.is_empty(),
        "这些错误拼写撞上高频词（不收，或写进 HIGH_FREQ_EXEMPT 并说明理由）：{high_freq:?}"
    );
}
