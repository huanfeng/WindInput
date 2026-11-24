use super::*;

const XIAOHE: &str = r#"
[meta]
id = "xiaohe"
name = "小鹤双拼"
[initials]
v = "zh"
i = "ch"
u = "sh"
[finals]
o = ["uo", "o"]
k = ["uai", "ing"]
v = ["ui", "v"]
[zero_initials]
a = ["a", "ai", "an", "ang", "ao"]
"#;

#[test]
fn layout_parse_and_self_map() {
    let lay = Layout::from_toml_str(XIAOHE).unwrap();
    assert_eq!(lay.id, "xiaohe");
    assert_eq!(lay.name, "小鹤双拼");
    // 显式声母
    assert_eq!(lay.initial_of(b'v'), Some("zh"));
    assert_eq!(lay.initial_of(b'i'), Some("ch"));
    // 自映射补全：未在 [initials] 列出的普通声母键映射自身
    assert_eq!(lay.initial_of(b'b'), Some("b"));
    assert_eq!(lay.initial_of(b'p'), Some("p"));
    // 韵母多值
    assert_eq!(lay.finals_of(b'o'), &["uo".to_string(), "o".to_string()]);
    assert!(lay.is_final_key(b'k'));
    assert!(!lay.is_final_key(b'q')); // q 不在 finals
    // 零声母
    assert_eq!(lay.zero_of(b'a').len(), 5);
}

#[test]
fn layout_symbol_key_as_final() {
    let t = "[meta]\nid=\"x\"\nname=\"x\"\n[finals]\n\";\" = [\"ing\"]\n";
    let lay = Layout::from_toml_str(t).unwrap();
    assert!(lay.is_final_key(b';'));
    assert_eq!(lay.finals_of(b';'), &["ing".to_string()]);
}

// --- 边界测试 6.1-1：from_toml 不存在的路径 → Err，不 panic ---
#[test]
fn from_toml_nonexistent_path_returns_err() {
    let p = std::path::Path::new("/nonexistent/path/does_not_exist.toml");
    let result = Layout::from_toml(p);
    assert!(result.is_err(), "不存在的文件应返回 Err，而非 panic");
}

// --- 边界测试 6.1-2a：from_str 缺少 [meta] → Err ---
#[test]
fn from_str_missing_meta_returns_err() {
    let bad = r#"
[finals]
k = ["ao"]
"#;
    let result = Layout::from_toml_str(bad);
    assert!(result.is_err(), "缺 [meta] 的 toml 应返回 Err");
}

// --- 边界测试 6.1-2b：from_str meta 缺少 id 字段 → Err ---
#[test]
fn from_str_meta_missing_id_returns_err() {
    let bad = r#"
[meta]
name = "无 id 方案"
[finals]
k = ["ao"]
"#;
    let result = Layout::from_toml_str(bad);
    assert!(result.is_err(), "meta 缺 id 字段应返回 Err");
}

// --- 边界测试 6.1-3：from_str 多字符键名 → key_byte 校验返回 Err ---
#[test]
fn from_str_multichar_key_returns_err() {
    // initials 里使用 2 字符键名 "zh"（而非单字符），应被 key_byte 拒绝
    let bad = r#"
[meta]
id = "bad"
name = "bad"
[initials]
zh = "x"
[finals]
k = ["ao"]
"#;
    let result = Layout::from_toml_str(bad);
    assert!(result.is_err(), "多字符键名应被 key_byte 校验拒绝");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("单字符") || err_msg.contains("zh"),
        "错误信息应提及多字符键: {err_msg}"
    );
}

#[test]
fn builtin_layouts_load() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    for id in [
        "xiaohe", "ziranma", "mspy", "sogou", "abc", "ziguang", "shoudao", "gb2017",
    ] {
        let p = dir.join(format!("{id}.toml"));
        let lay = Layout::from_toml(&p).unwrap_or_else(|e| panic!("加载 {id} 失败: {e}"));
        assert_eq!(lay.id, id, "{id} 的 meta.id 不匹配");
        assert!(lay.is_final_key(b'k'), "{id} 应有 finals['k']");
    }
    // 差异点对拍：小鹤 ao=c，ai=d
    let xh = Layout::from_toml(&dir.join("xiaohe.toml")).unwrap();
    assert_eq!(
        xh.finals_of(b'c'),
        &["ao".to_string()],
        "小鹤 finals[c] 应为 ao"
    );
    assert_eq!(
        xh.finals_of(b'd'),
        &["ai".to_string()],
        "小鹤 finals[d] 应为 ai"
    );
    assert_eq!(xh.initial_of(b'v'), Some("zh"), "小鹤 initials[v] 应为 zh");

    // 自然码 ao=k，ai=l（与小鹤不同）
    let zrm = Layout::from_toml(&dir.join("ziranma.toml")).unwrap();
    assert_eq!(
        zrm.finals_of(b'k'),
        &["ao".to_string()],
        "自然码 finals[k] 应为 ao"
    );
    assert_eq!(
        zrm.finals_of(b'l'),
        &["ai".to_string()],
        "自然码 finals[l] 应为 ai"
    );

    // 微软 ;=ing
    let ms = Layout::from_toml(&dir.join("mspy.toml")).unwrap();
    assert!(ms.is_final_key(b';'), "微软双拼应有 finals[;]");
    assert_eq!(
        ms.finals_of(b';'),
        &["ing".to_string()],
        "微软 finals[;] 应为 ing"
    );

    // 智能ABC a=zh（声母），zero_initials 只有 o
    let abc = Layout::from_toml(&dir.join("abc.toml")).unwrap();
    assert_eq!(
        abc.initial_of(b'a'),
        Some("zh"),
        "智能ABC initials[a] 应为 zh"
    );
    assert_eq!(
        abc.initial_of(b'e'),
        Some("ch"),
        "智能ABC initials[e] 应为 ch"
    );
    assert_eq!(
        abc.initial_of(b'v'),
        Some("sh"),
        "智能ABC initials[v] 应为 sh"
    );
    assert!(
        abc.zero_of(b'a').is_empty(),
        "智能ABC zero_initials 不应有 a 键（a=zh 冲突）"
    );

    // 紫光 u=zh，i=sh，a=ch；zero_initials 无 a
    let zg = Layout::from_toml(&dir.join("ziguang.toml")).unwrap();
    assert_eq!(zg.initial_of(b'u'), Some("zh"), "紫光 initials[u] 应为 zh");
    assert_eq!(zg.initial_of(b'i'), Some("sh"), "紫光 initials[i] 应为 sh");
    assert_eq!(zg.initial_of(b'a'), Some("ch"), "紫光 initials[a] 应为 ch");
    assert!(
        zg.zero_of(b'a').is_empty(),
        "紫光 zero_initials 不应有 a 键（a=ch 冲突）"
    );
    assert!(zg.is_final_key(b';'), "紫光应有 finals[;]");
    assert_eq!(
        zg.finals_of(b';'),
        &["ing".to_string()],
        "紫光 finals[;] 应为 ing"
    );

    // 首道双拼：e=sh，使用 zero_pairs 而非 zero_initials
    let sd = Layout::from_toml(&dir.join("shoudao.toml")).unwrap();
    assert_eq!(sd.initial_of(b'e'), Some("sh"), "首道 initials[e] 应为 sh");
    assert!(sd.has_zero_pairs(), "首道双拼应使用 zero_pairs");
    assert_eq!(sd.zero_pair(b'u', b'e'), Some("e"), "首道 ue → e");
    assert_eq!(sd.zero_pair(b'u', b'i'), Some("ei"), "首道 ui → ei");
    assert_eq!(sd.zero_pair(b'u', b'f'), Some("eng"), "首道 uf → eng");
    assert_eq!(sd.zero_pair(b'a', b'a'), Some("a"), "首道 aa → a");
    assert_eq!(sd.zero_pair(b'o', b'o'), Some("o"), "首道 oo → o");
    assert!(
        sd.zero_of(b'a').is_empty(),
        "首道使用 zero_pairs 时 zero_initials 应为空"
    );
}

/// 7 个内置布局的码元集：只有 finals 里带 `;` 的三家产出非默认集，其余为 None。
///
/// ⚠️ 这条断言是「`;` 能不能打出 ing」的**唯一**结构性守卫。布局 TOML 里写了
/// `";" = ["ing"]` 只是数据；数据要变成行为，必须经由本集告诉协调器「`;` 是码元」。
/// 引擎的 `convert("n;") == "ning"` 全绿而用户打不出，正是因为这段接线曾经不存在。
#[test]
fn builtin_layouts_code_char_set() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let load = |id: &str| Layout::from_toml(&dir.join(format!("{id}.toml"))).unwrap();

    for id in ["mspy", "sogou", "ziguang"] {
        let cs = load(id)
            .code_char_set()
            .unwrap_or_else(|| panic!("{id} 的 finals 含 `;`，应产出非默认码元集"));
        assert!(cs.contains(';'), "{id}：`;` 应是码元");
        assert!(
            !cs.contains_leading(';'),
            "{id}：`;` 是韵母（第二码），不得进首码集"
        );
        assert!(
            cs.contains('a') && cs.contains_leading('a'),
            "{id}：字母不受影响"
        );
        assert!(!cs.contains('['), "{id}：布局没用到的符号不得混入");
        assert!(
            cs.has_non_leading(),
            "{id}：应存在「是码元但不能起头」的字符"
        );
    }

    // 韵母键全是字母的布局 → None，协调器回落内置 a-z（零回归）。
    for id in ["xiaohe", "ziranma", "abc", "shoudao"] {
        assert!(
            load(id).code_char_set().is_none(),
            "{id} 的键全是字母，应回落默认集而非构造一份等价副本"
        );
    }
}

/// 非字母键出现在**第一码**（声母/零声母引导）时必须进首码集——判据是「第几码」，
/// 不是「是不是字母」。内置布局都没这么配，故只能构造布局来锁住这条语义。
#[test]
fn code_char_set_leading_follows_key_position() {
    let t = r#"
[meta]
id = "x"
name = "x"
[initials]
"/" = "zh"
[finals]
";" = ["ing"]
"#;
    let cs = Layout::from_toml_str(t).unwrap().code_char_set().unwrap();
    assert!(
        cs.contains('/') && cs.contains_leading('/'),
        "声母键应可起头"
    );
    assert!(
        cs.contains(';') && !cs.contains_leading(';'),
        "韵母键不可起头"
    );
}

/// `-` 作码元键时必须排到规格串末位，否则 `"a-z-;"` 里的 `z-;` 被当范围符、
/// 端点逆序 → 整串解析失败 → 静默回落 `a-z`，同布局里合法的 `;` 一起失效。
#[test]
fn code_char_set_hyphen_key_does_not_break_spec() {
    let t = r#"
[meta]
id = "x"
name = "x"
[finals]
"-" = ["ang"]
";" = ["ing"]
"#;
    let cs = Layout::from_toml_str(t).unwrap().code_char_set().unwrap();
    assert!(cs.contains('-'), "`-` 应是码元");
    assert!(cs.contains(';'), "`;` 不得被 `-` 的解析失败连累");
    assert!(cs.contains('a') && cs.contains('z'), "a-z 范围应完好");
}

#[test]
fn zero_pairs_invalid_key_length() {
    let bad = r#"
[meta]
id = "bad"
name = "bad"
[finals]
k = ["ao"]
[zero_pairs]
abc = "a"
"#;
    let result = Layout::from_toml_str(bad);
    assert!(result.is_err(), "zero_pairs 三字符键应被拒绝");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("两字符"),
        "错误信息应提及两字符: {err_msg}"
    );
}
