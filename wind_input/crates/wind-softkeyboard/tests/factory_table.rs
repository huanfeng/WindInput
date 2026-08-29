//! 零回归闸门：出厂 `data/system.softkeyboard.toml` 的内容正确性。
//!
//! ★ 画布是**按位置对位**的，所以「某一行少写一个 token」不会报错，只会让那一行之后的
//! 符号整体前移一格。这种错位在肉眼看来只是「某些符号跑到了别的键上」，极难归因——
//! 它是本格式唯一的静默失败模式，必须由测试守。

use wind_softkeyboard::{KEY_ROWS, SoftKeyboardTable};

/// 出厂文件的实际路径（crate → 仓库根 → data/）。
fn factory_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../data/")
        .join(wind_softkeyboard::FILE_NAME)
}

fn factory_text() -> String {
    let path = factory_path();
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不到出厂软键盘表 {}: {}", path.display(), e))
}

fn factory_table() -> SoftKeyboardTable {
    SoftKeyboardTable::parse(&factory_text()).expect("出厂软键盘表必须能解析")
}

/// 出厂 13 面齐全、id 与顺序都不漂移。
///
/// 顺序有意义：它就是切面顺序，也是标签行的排列。热键 `softkeyboard:<id>` 认的是 id，
/// 改名会让用户配的直通车静默失效。
#[test]
fn factory_pages_are_the_expected_thirteen() {
    let t = factory_table();
    let ids: Vec<&str> = t.pages().iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "pc", "punct", "num", "math", "unit", "tab", "special", "greek", "russian", "zhuyin",
            "pinyin", "hiragana", "katakana",
        ],
        "出厂面的集合或顺序变了——id 是用户直通车的匹配依据，改名前先确认没人在用"
    );
    for p in t.pages() {
        assert!(!p.name.is_empty(), "面 {} 没有显示名", p.id);
    }
}

/// ★★★ 每行 token 数不得超过该行的键位数。
///
/// 超出的部分会被加载器截断并告警，但告警只进日志、用户看不到；作者会以为自己写的符号
/// 进去了。这里直接读原文数 token，绕开加载器的截断。
#[test]
fn no_row_overflows_its_key_count() {
    let text = factory_text();
    let doc: toml::Value = toml::from_str(&text).expect("出厂表必须是合法 TOML");
    let pages = doc["pages"].as_array().expect("pages 必须是数组");

    for page in pages {
        let id = page["id"].as_str().unwrap_or("<无 id>");
        for field in ["rows", "rows_shift"] {
            let Some(rows) = page.get(field).and_then(|v| v.as_array()) else {
                continue;
            };
            assert!(
                rows.len() <= KEY_ROWS.len(),
                "面 {id} 的 {field} 有 {} 行，最多 {} 行",
                rows.len(),
                KEY_ROWS.len()
            );
            for (r, line) in rows.iter().enumerate() {
                let line = line.as_str().unwrap_or_default();
                // 与加载器同一套切分：按 ASCII 空格 / 制表符，U+3000 是符号不是分隔符。
                let n = line.split([' ', '\t']).filter(|s| !s.is_empty()).count();
                assert!(
                    n <= KEY_ROWS[r].len(),
                    "面 {id} 的 {field} 第 {} 行有 {n} 个 token，该行只有 {} 个键位——\
                     多出的会被静默截断",
                    r + 1,
                    KEY_ROWS[r].len()
                );
            }
        }
    }
}

/// ★★★ PC键盘面必须是恒等映射：每个键位输出它自己。
///
/// 这是最强的一条错位检测——它把全部 47 个键位逐个查一遍。任何一行漏写或多写一个
/// token，这个面立刻对不上，而别的面的同类错误肉眼几乎发现不了。
#[test]
fn pc_page_is_an_identity_map() {
    let t = factory_table();
    let pc = t.page("pc").expect("缺 PC键盘面");

    let base: [&[&str]; 4] = [
        &[
            "`", "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-", "=",
        ],
        &[
            "q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "[", "]", "\\",
        ],
        &["a", "s", "d", "f", "g", "h", "j", "k", "l", ";", "'"],
        &["z", "x", "c", "v", "b", "n", "m", ",", ".", "/"],
    ];
    let shift: [&[&str]; 4] = [
        &[
            "~", "!", "@", "#", "$", "%", "^", "&", "*", "(", ")", "_", "+",
        ],
        &[
            "Q", "W", "E", "R", "T", "Y", "U", "I", "O", "P", "{", "}", "|",
        ],
        &["A", "S", "D", "F", "G", "H", "J", "K", "L", ":", "\""],
        &["Z", "X", "C", "V", "B", "N", "M", "<", ">", "?"],
    ];

    for (r, row) in KEY_ROWS.iter().enumerate() {
        for (c, slot) in row.iter().enumerate() {
            assert_eq!(
                pc.output(slot, false),
                Some(base[r][c]),
                "PC键盘面基础层第 {} 行第 {} 个键位（{slot}）对不上——画布错位",
                r + 1,
                c + 1
            );
            assert_eq!(
                pc.output(slot, true),
                Some(shift[r][c]),
                "PC键盘面第二层第 {} 行第 {} 个键位（{slot}）对不上——画布错位",
                r + 1,
                c + 1
            );
        }
    }
}

/// 抽查几个面的**行首与行尾**键位。
///
/// 行首行尾是错位最先暴露的位置：中间漏一个 token 会把该行尾部的符号整体前移，
/// 于是尾键位要么变成前一个符号、要么空掉。
#[test]
fn row_boundaries_land_on_the_right_keys() {
    let t = factory_table();

    let punct = t.page("punct").unwrap();
    assert_eq!(punct.output("grave", false), Some("·"));
    assert_eq!(punct.output("equal", false), Some("＝"), "数字行尾");
    assert_eq!(punct.output("q", false), Some("“"));
    assert_eq!(punct.output("backslash", false), Some("＼"), "QWERTY 行尾");
    assert_eq!(punct.output("a", false), Some("、"));
    assert_eq!(punct.output("quote", false), Some("＇"), "ASDF 行尾");
    assert_eq!(punct.output("z", false), Some("〃"));
    assert_eq!(punct.output("slash", false), Some("／"), "ZXCV 行尾");

    let math = t.page("math").unwrap();
    assert_eq!(math.output("q", false), Some("√"));
    assert_eq!(math.output("z", false), Some("∞"));
    assert_eq!(math.output("slash", false), Some("⊙"));
    assert_eq!(math.output("grave", true), Some("∴"), "第二层行首");

    // 希腊面按希腊文键盘键位：a=α s=σ d=δ，z=ζ x=χ c=ψ v=ω。
    let greek = t.page("greek").unwrap();
    assert_eq!(greek.output("a", false), Some("α"));
    assert_eq!(greek.output("v", false), Some("ω"));
    assert_eq!(greek.output("a", true), Some("Α"));

    // 俄文面按 ЙЦУКЕН：q=й a=ф z=я。
    let russian = t.page("russian").unwrap();
    assert_eq!(russian.output("q", false), Some("й"));
    assert_eq!(russian.output("a", false), Some("ф"));
    assert_eq!(russian.output("z", false), Some("я"));
    assert_eq!(
        russian.output("slash", false),
        Some("."),
        "\\. 应解成字面点"
    );
}

/// 出厂表里不该有全空的面（它会占一个标签位却什么都打不出）。
#[test]
fn no_empty_pages_survive() {
    let t = factory_table();
    assert_eq!(t.len(), 13, "有面在加载期被剔除了——看告警日志");
    for p in t.pages() {
        assert!(!p.is_empty(), "面 {} 是空的", p.id);
    }
}

/// 只写 `keys` 的用户覆盖不动出厂画布；写了 `rows` 才整面替换。
///
/// 这条语义直接决定用户改一个键会不会把整面清空，用出厂表跑一遍比单测更接近真实。
#[test]
fn user_override_against_the_factory_table() {
    let mut t = factory_table();
    t.merge_user(
        r#"
[[pages]]
id = "math"
keys = { q = "㊙" }
"#,
    )
    .expect("用户覆盖必须能解析");

    let math = t.page("math").unwrap();
    assert_eq!(math.output("q", false), Some("㊙"), "补丁生效");
    assert_eq!(math.output("w", false), Some("∛"), "同一行其它键位不受影响");
    assert_eq!(math.output("z", false), Some("∞"), "其它行不受影响");
    assert_eq!(t.len(), 13, "面数不变");
}

/// 出厂表里**只有 PC 键盘面是键盘面**。
///
/// 这条钉住的是一句用户可见的行为：在 PC 键盘面上点 n-i-h-a-o 应该出「你好」，
/// 而不是往文档里塞五个字母。它靠 `send_keys` 一路传到 C++ 的吃键判定
/// （`STATUS_SOFT_KEYBOARD_KEYS`）——这一位丢了，那一面就退化成「打字母上屏字母」，
/// 而面板看起来完全正常。
///
/// 反过来，符号面**不能**是键盘面：那些字符键盘上根本敲不出来，合成按键只会发出
/// 对应键位的原字符。
#[test]
fn only_the_pc_page_sends_keys() {
    let t = factory_table();
    for p in t.pages() {
        let expect = p.id == "pc";
        assert_eq!(
            p.send_keys, expect,
            "面 {} 的 send_keys 应为 {expect}（只有 PC 键盘面发按键）",
            p.id
        );
    }
}

/// 用户覆盖的三态：不写 `send_keys` 时沿用出厂值，写了才改。
///
/// ★ 只想换 PC 面画布的用户不该顺带把它降级成符号面——那会让整面突然打不出中文，
/// 而配置里一个字都没提到这件事。
#[test]
fn send_keys_survives_a_canvas_only_override() {
    let mut t = factory_table();
    t.merge_user(
        r#"
[[pages]]
id = "pc"
rows = ["` 1 2 3 4 5 6 7 8 9 0 - ="]
"#,
    )
    .expect("用户覆盖必须能解析");
    assert!(
        t.page("pc").unwrap().send_keys,
        "整面替换未写 send_keys ⇒ 沿用出厂的 true"
    );

    t.merge_user(
        r#"
[[pages]]
id = "pc"
send_keys = false
keys = { q = "Q" }
"#,
    )
    .expect("用户覆盖必须能解析");
    assert!(
        !t.page("pc").unwrap().send_keys,
        "显式写了 false 就该改过来"
    );
}
