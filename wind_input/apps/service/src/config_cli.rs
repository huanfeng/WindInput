//! `wind_input config ...` 命令行：配置的查看/读写/导入导出。
//!
//! - list/describe/get/export/check 纯本地（registry + Config，无需运行中的 core）。
//! - set/import 优先经 RPC 发给运行中的 core（即时热重载），连不上则离线直写
//!   用户配置文件（下次启动生效）。
//! - 写入前一律按 config_schema 注册表校验（未知键/类型/枚举越界即拒绝）。

mod custom_check;

use serde_json::{Value, json};
use wind_config::Config;
use wind_config::config_schema::{
    FieldType, field, is_known_key, leaf_entries, parse_str_value, registry, validate,
};

// 变体后缀经 wind_config::variant::pipe_suffix() 运行时取得：CLI 与 core 同一 exe，自辨一致。

/// 子命令入口。`args` 为 `config` 之后的参数。返回进程退出码。
pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("list") => cmd_list(args.get(1).map(String::as_str)),
        Some("describe") | Some("desc") => match args.get(1) {
            Some(key) => cmd_describe(key),
            None => usage_err("describe <key>"),
        },
        Some("get") => match args.get(1) {
            Some(key) => cmd_get(key),
            None => usage_err("get <key>"),
        },
        Some("set") => match (args.get(1), args.get(2)) {
            (Some(key), Some(raw)) => cmd_set(key, raw),
            _ => usage_err("set <key> <value>"),
        },
        Some("export") => cmd_export(),
        Some("check") => cmd_check(&args[1..]),
        Some("import") => match args.get(1) {
            Some(path) => cmd_import(path),
            None => usage_err("import <file.toml>"),
        },
        Some("help") | Some("--help") | Some("-h") | None => {
            print_usage();
            0
        }
        Some(other) => {
            eprintln!("未知子命令: {other}");
            print_usage();
            2
        }
    }
}

fn print_usage() {
    eprintln!(
        "用法: wind_input config <命令>\n\
         \n\
         命令:\n  \
         list [前缀]          列出配置键与类型（可按键前缀过滤）\n  \
         describe <key>       显示某键的类型/可选值/当前值，并逐层追溯来源\n  \
         get <key>            读取某键当前值\n  \
         set <key> <value>    设置某键（优先热重载，core 未运行则离线写）\n  \
         export               导出当前完整配置（TOML）\n  \
         import <file.toml>   从 TOML 文件批量导入\n  \
         check [选项]         体检定制版数据层（data_custom），给第三方定制者用\n\
         \n\
         check 的选项:\n  \
         --custom <目录>      要体检的 data_custom 目录（省略则用本机安装的那个）\n  \
         --data <目录>        出厂 data 目录（省略则用本机安装的那个）\n\
         \n\
         退出码: 0 无错误 / 1 检出错误 / 2 用法错误"
    );
}

fn usage_err(form: &str) -> i32 {
    eprintln!("用法: wind_input config {form}");
    2
}

fn cmd_list(prefix: Option<&str>) -> i32 {
    for fld in registry() {
        if let Some(p) = prefix
            && !fld.key.starts_with(p)
        {
            continue;
        }
        println!("{:<48} {}", fld.key, type_label(fld.ty));
    }
    0
}

fn cmd_describe(key: &str) -> i32 {
    let Some(fld) = field(key) else {
        eprintln!("未登记的配置键: {key}");
        return 1;
    };
    println!("键:     {key}");
    println!("类型:   {}", type_label(fld.ty));
    if let FieldType::Enum(vals) = fld.ty {
        println!("可选值: {}", vals.join(" | "));
    }
    match load_value(key) {
        Ok(v) => println!("当前值: {}", format_value(&v)),
        Err(e) => println!("当前值: <读取失败: {e}>"),
    }
    print_key_origin(key);
    0
}

/// 单层值的显示上限。Map / StructList 整表打出来能刷屏几十行，而这里要回答的是
/// 「哪一层写了」，不是「写了什么」——完整值有 `config get`。
const ORIGIN_VALUE_MAX: usize = 40;

/// 终端显示宽度：CJK / 全角标点占两列，其余按一列。
///
/// 不能用 `str::len`（UTF-8 字节数）也不能用 `chars().count()`：本表的值列里满是中文
/// （标点映射、方案名、`—` 占位符本身就是全角破折号），按字符数补空格会让路径列参差
/// 不齐——而那正是这张表唯一需要对齐的地方。
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let c = c as u32;
            // CJK 统一表意文字、全角标点、假名、全角 ASCII 区段——覆盖本表实际会出现的字符。
            let wide = (0x1100..=0x115F).contains(&c)
                || (0x2E80..=0xA4CF).contains(&c)
                || (0xAC00..=0xD7A3).contains(&c)
                || (0xF900..=0xFAFF).contains(&c)
                || (0xFE30..=0xFE4F).contains(&c)
                || (0xFF00..=0xFF60).contains(&c)
                || (0xFFE0..=0xFFE6).contains(&c);
            if wide { 2 } else { 1 }
        })
        .sum()
}

/// 左对齐补足到 `width` 显示列（已超宽则原样返回）。
fn pad_display(s: &str, width: usize) -> String {
    let w = display_width(s);
    if w >= width {
        return s.to_string();
    }
    format!("{s}{}", " ".repeat(width - w))
}

/// 把一层的值压成一行。超长截断并给出省略号，空缺显示破折号。
fn origin_value_cell(v: Option<&toml::Value>) -> String {
    let Some(v) = v else {
        return "—".into();
    };
    // 字符串去引号与 `format_value` 对齐；其余走 TOML 的紧凑写法（数组/表都读得懂）。
    let s = match v {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let s = s.replace('\n', " ");
    if s.chars().count() <= ORIGIN_VALUE_MAX {
        return s;
    }
    let head: String = s.chars().take(ORIGIN_VALUE_MAX - 1).collect();
    format!("{head}…")
}

/// 打印四层来源追溯。这是 `describe` 相对 `get` 的全部增量——
/// `get` 回答「现在是什么」，这里回答**「为什么是它」**。
fn print_key_origin(key: &str) {
    let origin = match Config::key_origin(key, Config::data_dir().as_deref()) {
        Ok(o) => o,
        // 追溯失败不影响上面已经打出的类型与当前值，故只提示、不改退出码。
        Err(e) => {
            println!("来源:   <追溯失败: {e}>");
            return;
        }
    };

    // ★ 降级必须先判。`effective_layer == None` 同时承载三种成因（跨层深合并、
    // `normalize` 改写、降级回落），前两种是「正常但指不到单一层」，第三种是「你的配置
    // 根本没生效」——含义相反。不先判降级就会把最严重的那种说成最平常的那种。
    // ★★ 语法故障**不能**照搬 `degraded` 来报「未生效」。
    //
    // 语法故障时 `ConfigDegradation::taints` 对任何路径恒为真——那是**写盘闸**的判据，
    // 保守方向选对了：不知道哪些键受影响就一律不写，损失为零。但同一个判据拿来呈现
    // 就是在撒谎：被跳过的只是某几行，本键很可能好好地生效着（实跑见过一次——
    // `per_page` 明明取到了 user 层的 9，却被报成「本次未生效」）。
    // **保守的方向对写是安全，对读是错误**：呈现说错了，用户会去改一个没问题的键。
    // 所以这里照常报来源，语法故障降级为底部的附加警示。
    if origin.degraded && origin.syntax_error.is_none() {
        println!("来源:   ⚠ 本次未生效——所在配置段解析失败，已整段回落出厂默认");
    } else {
        match origin.effective_layer {
            Some(l) => println!("来源:   {}", layer_label(l)),
            // 说清是哪一种「指不到」，否则用户会以为工具没查出来。
            None if origin.effective.is_none() => println!("来源:   任何一层都没有声明"),
            None => println!("来源:   多层合并（表按键逐层合并，指不到单独一层）"),
        }
    }

    println!("\n各层声明（低 → 高，靠后的覆盖靠前的；→ 标出生效层）:");
    let cells: Vec<String> = origin
        .layers
        .iter()
        .map(|l| origin_value_cell(l.value.as_ref()))
        .collect();
    // 列宽随内容走：绝大多数键的值是个位数字或短枚举，固定宽会把路径推到屏幕外面去。
    let vw = cells.iter().map(|c| display_width(c)).max().unwrap_or(1);
    for (l, cell) in origin.layers.iter().zip(&cells) {
        let mark = if Some(l.layer) == origin.effective_layer {
            "→"
        } else {
            " "
        };
        let path = match (&l.path, l.layer) {
            (Some(p), _) => p.display().to_string(),
            // 各有各的缺席原因——留白会让人以为是工具没读到。
            (None, "default") => "（代码内置，无文件）".into(),
            (None, "custom") => "（本机不是定制版）".into(),
            (None, _) => "（该层目录不可用）".into(),
        };
        println!(
            "{mark} {}  {}  {path}",
            pad_display(l.layer, 7),
            pad_display(cell, vw)
        );
    }

    if let Some(err) = &origin.syntax_error {
        // 措辞刻意是**条件式**的（「若……则」），不是断言：本函数无从知道被跳过的那几行
        // 里写的是哪些键——救回来的 Value 里，「被跳过的键」与「从未写过的键」完全同形。
        println!(
            // 「修好那一行」在「整个文件都不是 TOML」的情形下不成立（那时没有「那一行」），
            // 故用通用措辞。同一句要覆盖两种形态：跳过几行 vs 整份没加载。
            "\n⚠ 配置文件语法不合法：{err}\n\
             \x20 若本键正写在未能解析的行里，则它本次没有生效；上面的「来源」按救回的\n\
             \x20 内容判定，不受影响的键照常生效。修好语法即可完全恢复。\n\
             \x20 在此期间程序不会覆盖该文件（写回已被拦下），设置页保存时会先备份原件。"
        );
    } else if origin.degraded {
        // 这一条是「配置文件里白纸黑字写着，程序却在用别的值」的唯一解释。
        println!(
            "\n⚠ 该键所在的配置段本次解析失败、已整段回落出厂默认——上面各层的声明\n\
             \x20 本次都没有生效。日志里搜「解析失败」可定位到具体是哪个值的问题。"
        );
    }
}

/// 层名的中文标签。与日志里的 `覆盖生效[user][…]` 用同一批层名，只是这里给人读。
fn layer_label(layer: &str) -> String {
    let what = match layer {
        "default" => "代码默认",
        "data" => "出厂",
        "custom" => "定制版",
        "user" => "用户",
        other => other,
    };
    format!("{what}（{layer}）")
}

fn cmd_get(key: &str) -> i32 {
    if !is_known_key(key) {
        eprintln!("未登记的配置键: {key}");
        return 1;
    }
    match load_value(key) {
        Ok(v) => {
            println!("{}", format_value(&v));
            0
        }
        Err(e) => {
            eprintln!("读取失败: {e}");
            1
        }
    }
}

fn cmd_set(key: &str, raw: &str) -> i32 {
    let value = match parse_value(key, raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("无法设置 '{key}': {e}");
            return 1;
        }
    };
    if let Err(e) = validate(key, &value) {
        eprintln!("无法设置 '{key}': {e}");
        return 1;
    }
    apply_items(vec![(key.to_string(), value)])
}

fn cmd_export() -> i32 {
    match Config::load(Config::data_dir().as_deref()) {
        Ok(cfg) if cfg.degradation.is_degraded() => {
            // ⛔ 降级过就**拒绝导出**，宁可什么都不给。
            //
            // 导出产物的用途是备份和 `config export > config.toml` 回写，而降级后的配置里
            // 坏段已经被出厂值顶掉——导出去就是把这次数据丢失**固化**成用户的新配置，
            // 而且他从输出里完全看不出来。同 `preset_for_pruning` 取不到 preset 时退化为
            // 「不清理」：拿不到可信的全量就别动。
            // 语法故障先讲：它的修法（去改那一行）与段级降级（去改那个键的类型）不同，
            // 共用一句「段解析失败」会把用户支去翻一个语法上根本没问题的段。
            if let Some(u) = cfg.degradation.unparsable.first() {
                // 「只加载了可解析的部分」与「一个字都没加载」是两种情形，判据是
                // `is_salvaged()` 而**不是** `skipped_lines` 非空——啃到上限仍失败时
                // 两者同时成立（同一处判据本轮曾在四个地方各写错一遍）。
                eprintln!(
                    "拒绝导出：{}，导出的内容不是你的真实配置。",
                    if u.is_salvaged() {
                        "配置文件语法不合法，本次只加载了其中可解析的部分"
                    } else {
                        "配置文件语法不合法，本次一个键都没能加载"
                    }
                );
                eprintln!("  文件：{}", u.path.display());
                if !u.skipped_lines.is_empty() {
                    let verb = if u.is_salvaged() {
                        "已跳过"
                    } else {
                        "已尝试跳过（仍无法解析）"
                    };
                    // 紧凑形态：啃不动的文件会攒到 32 个行号。量词在 lines_phrase 里。
                    eprintln!("  {verb}：{}", u.lines_phrase());
                }
                if !u.error.is_empty() {
                    eprintln!("  首个错误：{}", u.error);
                }
                eprintln!("  请先修正该文件的语法，再重新导出。");
                return 1;
            }
            eprintln!(
                "拒绝导出：本次加载有配置段解析失败并回落了出厂默认值，导出的内容不是你的真实配置。"
            );
            if cfg.degradation.total_fallback {
                eprintln!("  受影响：整份配置（无法定位到具体段）");
            } else {
                eprintln!("  受影响的段：{}", cfg.degradation.sections.join(", "));
            }
            eprintln!(
                "  请先修正配置文件里这些段的坏键（日志中有 WARN 记录了具体错误），再重新导出。"
            );
            // 用 1（操作失败）而非 2：本 CLI 里 2 是**用法错误**（未知子命令、参数缺失），
            // 而这是「用法没问题，但拒绝执行」。
            1
        }
        Ok(cfg) => match toml::to_string_pretty(&cfg) {
            Ok(s) => {
                print!("{s}");
                0
            }
            Err(e) => {
                eprintln!("序列化失败: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("加载配置失败: {e}");
            1
        }
    }
}

/// `config check [--custom <目录>] [--data <目录>]`：体检定制版数据层。
///
/// 全程本地：不连 core、不读用户层 `%APPDATA%`、一个字节都不写盘。放在 `config` 子命令
/// 下正是因为这一族有**离线降级**——定制者拿到安装包解开就能跑，不必先把输入法装起来。
fn cmd_check(args: &[String]) -> i32 {
    let mut custom: Option<std::path::PathBuf> = None;
    let mut data: Option<std::path::PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let slot = match a.as_str() {
            "--custom" => &mut custom,
            "--data" => &mut data,
            other => {
                eprintln!("未知选项: {other}");
                return usage_err("check [--custom <目录>] [--data <目录>]");
            }
        };
        let Some(raw) = it.next() else {
            eprintln!("{a} 缺少目录参数");
            return usage_err("check [--custom <目录>] [--data <目录>]");
        };
        // `${APP_DIR}` 等内部目录变量与其它子命令同解析。
        match crate::cli_util::resolve_path(raw) {
            Ok(p) => *slot = Some(std::path::PathBuf::from(p)),
            Err(e) => {
                eprintln!("{e}");
                return 2;
            }
        }
    }

    // `--custom` 显式给出 ⇒ 体检的是**别人的定制包**，出厂对照必须来自同一个包。
    let explicit_custom = custom.is_some();
    let custom_dir = match custom.or_else(Config::custom_data_dir) {
        Some(d) => d,
        None => {
            eprintln!(
                "本机不是定制版（安装目录下没有可解析的 data_custom/custom.toml）。\n\
                 用 --custom <目录> 指定要体检的定制层目录。"
            );
            return 2;
        }
    };
    // ★ `--custom` 给了而 `--data` 省略时，**绝不能**回落到本机安装的 data 目录。
    //
    // 那会拿这台机器的出厂数据去对照别人的包：冗余键（「与出厂值相同」）、hide 目标是否
    // 存在、opencc 文件名比对三项检查全都会得出**与那个包不符**的结论，而抬头里印的
    // 出厂目录路径与 --custom 八竿子打不着，人一眼看不出结论是错的。
    //
    // 改为在 `--custom` 的**同级**找 `data/`：`data/` 与 `data_custom/` 必须同级是本功能的
    // 硬契约（`variant::install_root` 刻意不拆成两个注入点，理由正是「拆开后能构造出生产里
    // 根本不存在的形态」）。找不到就是 `None` —— 需要出厂对照的检查跳过并在抬头声明，
    // 拿不到可信对照就别下结论。
    let data_dir = data.or_else(|| {
        if explicit_custom {
            custom_dir
                .parent()
                .map(|root| root.join("data"))
                .filter(|d| d.is_dir())
        } else {
            Config::data_dir()
        }
    });

    let version = env!("WIND_APP_VERSION");
    let report = custom_check::check_layer(&custom_dir, data_dir.as_deref(), version);
    custom_check::render(&report, &custom_dir, data_dir.as_deref(), version);
    // 与 cmd_export 同一套：2 留给用法错误，1 是「用法没问题，但结果不通过」。
    // 警告不影响退出码——它们是「现在能用、下次升级会坏」，卡住打包流程弊大于利。
    i32::from(report.errors() > 0)
}

fn cmd_import(path: &str) -> i32 {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("读取 {path} 失败: {e}");
            return 1;
        }
    };
    let root: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("解析 TOML 失败: {e}");
            return 1;
        }
    };
    let entries = leaf_entries(&root);
    if entries.is_empty() {
        eprintln!("文件无可导入的配置项");
        return 1;
    }
    // 全量校验，任一项不合法即整体中止（不部分写入）。
    let mut errors = Vec::new();
    for (k, v) in &entries {
        if let Err(e) = validate(k, v) {
            errors.push(format!("  {k}: {e}"));
        }
    }
    if !errors.is_empty() {
        eprintln!(
            "导入中止，{} 项不合法:\n{}",
            errors.len(),
            errors.join("\n")
        );
        return 1;
    }
    apply_items(entries)
}

/// 把若干 `(key, value)` 写入配置：优先 RPC 让运行中的 core 即时热重载；
/// 连不上则离线直写用户配置文件。
fn apply_items(items: Vec<(String, toml::Value)>) -> i32 {
    let json_items: Vec<Value> = items
        .iter()
        .filter_map(|(k, v)| {
            serde_json::to_value(v)
                .ok()
                .map(|jv| json!({ "key": k, "value": jv }))
        })
        .collect();

    match wind_rpc::client::call(
        wind_config::variant::pipe_suffix(),
        "config.setItems",
        json!({ "items": json_items }),
    ) {
        Ok(res) => {
            let restart = res
                .get("needsRestart")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let applied = res
                .get("applied")
                .and_then(Value::as_u64)
                .unwrap_or(items.len() as u64);
            // 正常情况 CLI 已预校验，skipped 应为空；防御性呈现 core 的跳过项。
            let skipped = res
                .get("skipped")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for s in &skipped {
                let k = s.get("key").and_then(Value::as_str).unwrap_or("?");
                let reason = s.get("reason").and_then(Value::as_str).unwrap_or("");
                eprintln!("⚠ 跳过 {k}: {reason}");
            }
            let note = if restart {
                "（需重启 core 完全生效）"
            } else {
                "（已热重载）"
            };
            println!("✓ 已应用 {applied} 项{note}");
            // 全部被跳过（一个都没应用）视为失败。
            if applied == 0 && !skipped.is_empty() {
                1
            } else {
                0
            }
        }
        Err(_) => {
            // core 未运行：离线直写，下次启动生效。
            for (k, v) in &items {
                let parts: Vec<&str> = k.split('.').collect();
                if let Err(e) = Config::set_user_value(&parts, v.clone()) {
                    eprintln!("写入 {k} 失败: {e}");
                    return 1;
                }
            }
            println!("✓ 已写入 {} 项（core 未运行，下次启动生效）", items.len());
            0
        }
    }
}

/// 读取某键的当前值（四层合并后），转为 JSON。
fn load_value(key: &str) -> anyhow::Result<Value> {
    let cfg = Config::load(Config::data_dir().as_deref())?;
    let full = serde_json::to_value(cfg)?;
    let mut cur = &full;
    for part in key.split('.') {
        cur = cur
            .get(part)
            .ok_or_else(|| anyhow::anyhow!("配置缺少键 {key}"))?;
    }
    Ok(cur.clone())
}

/// 按注册表类型把命令行原始字符串解析为 TOML 值（下沉共享实现，cmdbar 同用）。
fn parse_value(key: &str, raw: &str) -> Result<toml::Value, String> {
    parse_str_value(key, raw)
}

/// 类型的可读标签。
fn type_label(ty: FieldType) -> String {
    match ty {
        FieldType::Bool => "bool".into(),
        FieldType::Int => "int".into(),
        FieldType::Float => "float".into(),
        FieldType::Str => "string".into(),
        FieldType::Enum(vals) => format!("enum({})", vals.join("|")),
        FieldType::StrList => "string[]".into(),
        FieldType::Map(_) => "map".into(),
        FieldType::StructList => "array".into(),
    }
}

/// 显示一个 JSON 值：字符串去引号，其余按紧凑 JSON。
fn format_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_value_typed_by_registry() {
        assert_eq!(
            parse_value("ui.candidate.per_page", "9").unwrap(),
            toml::Value::Integer(9)
        );
        assert_eq!(
            parse_value("ui.candidate.hide_window", "true").unwrap(),
            toml::Value::Boolean(true)
        );
        assert_eq!(
            parse_value("ui.candidate.hide_window", "off").unwrap(),
            toml::Value::Boolean(false)
        );
        assert_eq!(
            parse_value("ui.candidate.font_size", "18").unwrap(),
            toml::Value::Float(18.0)
        );
        assert_eq!(
            parse_value("ui.candidate.layout", "vertical").unwrap(),
            toml::Value::String("vertical".into())
        );
        // 字符串列表按逗号拆分
        let list = parse_value("schema.available", "wubi86, wubi86_pinyin").unwrap();
        assert_eq!(
            list,
            toml::Value::Array(vec![
                toml::Value::String("wubi86".into()),
                toml::Value::String("wubi86_pinyin".into()),
            ])
        );
    }

    #[test]
    fn parse_value_rejects_bad_input() {
        assert!(parse_value("ui.candidate.per_page", "seven").is_err());
        assert!(parse_value("ui.candidate.hide_window", "maybe").is_err());
        assert!(parse_value("no.such.key", "x").is_err());
    }

    #[test]
    fn parse_value_enum_passes_raw_then_validate_catches_range() {
        // parse 不校验枚举成员（交给 validate）；越界值先解析成字符串
        let v = parse_value("ui.candidate.layout", "diagonal").unwrap();
        assert_eq!(v, toml::Value::String("diagonal".into()));
        assert!(validate("ui.candidate.layout", &v).is_err());
    }

    #[test]
    fn format_value_unquotes_string() {
        assert_eq!(format_value(&json!("vertical")), "vertical");
        assert_eq!(format_value(&json!(7)), "7");
        assert_eq!(format_value(&json!(true)), "true");
    }
}
