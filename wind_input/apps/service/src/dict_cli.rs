//! `wind_input dict ...` 命令行：用户词库（含临时词/词频/候选调整）按方案导入导出。
//!
//! 经 RPC 打给运行中的 core（仅在线）。文件读写在 CLI 侧完成，RPC 只传内容
//! 字符串——与设置页共用 `dict.export` / `dict.import` 同一契约（多段 wdict、
//! 引擎类型校验、Rime/TSV 自动识别）。文件路径经 `resolve_path` 解析，支持
//! `${APP_DIR}` / `${USER_DATA}` / `${LOCAL_DATA}` 内部目录变量。

use serde_json::{Value, json};

use crate::cli_util::{resolve_path, rpc_online};

/// 合法的段类型 key（与 wind-store `DictSection::key()` 驼峰一致）。
const SECTION_KEYS: &[&str] = &["userWords", "tempWords", "freq", "shadow"];

/// 子命令入口。`args` 为 `dict` 之后的参数。返回进程退出码。
pub fn run(args: &[String]) -> i32 {
    let r = match args.first().map(String::as_str) {
        Some("export") => match (args.get(1), args.get(2)) {
            (Some(id), Some(file)) => cmd_export(id, file, &args[3..]),
            _ => return usage_err("export <方案id> <文件> [--sections a,b,...]"),
        },
        Some("import") => match (args.get(1), args.get(2)) {
            (Some(id), Some(file)) => cmd_import(id, file, &args[3..]),
            _ => return usage_err("import <方案id> <文件> [--replace] [--sections a,b,...]"),
        },
        // 离线子命令：读数据目录的源文件，**不经 RPC、不要求服务在线**。
        // 受众是打包方案的作者，他们跑这条命令时通常没在跑输入法。
        Some("weight-check") => cmd_weight_check(&args[1..]),
        Some("help") | Some("--help") | Some("-h") | None => {
            print_usage();
            return 0;
        }
        Some(other) => {
            eprintln!("未知子命令: {other}");
            print_usage();
            return 2;
        }
    };
    match r {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn print_usage() {
    eprintln!(
        "用法: wind_input dict <命令>   （需要输入法服务在线）\n\
         \n\
         命令:\n  \
         export <方案id> <文件> [--sections a,b]   导出词库数据到文件（缺省按引擎默认段）\n  \
         import <方案id> <文件> [--replace] [--sections a,b]\n                                            \
         从文件导入（缺省合并；格式自动识别 WindDict/Rime/TSV）\n\
         \n\
         weight-check [--data <目录>]              按方案体检词库权重值域（离线，无需服务）\n\
         \n\
         段类型: userWords(用户词库) tempWords(临时词库) freq(词频) shadow(候选调整)"
    );
}

fn usage_err(form: &str) -> i32 {
    eprintln!("用法: wind_input dict {form}");
    2
}

/// 解析 `--sections a,b` 与 `--replace` 旗标；未知旗标或未知段名报错。
fn parse_flags(rest: &[String]) -> anyhow::Result<(Option<Vec<String>>, bool)> {
    let mut sections: Option<Vec<String>> = None;
    let mut replace = false;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--replace" => replace = true,
            "--sections" => {
                let raw = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--sections 缺少参数（逗号分隔的段名）"))?;
                let keys: Vec<String> = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect();
                for k in &keys {
                    if !SECTION_KEYS.contains(&k.as_str()) {
                        anyhow::bail!("未知段类型: {k}（可选: {}）", SECTION_KEYS.join(" / "));
                    }
                }
                if keys.is_empty() {
                    anyhow::bail!("--sections 参数为空");
                }
                sections = Some(keys);
            }
            other => anyhow::bail!("未知参数: {other}"),
        }
    }
    Ok((sections, replace))
}

fn cmd_export(id: &str, file: &str, rest: &[String]) -> anyhow::Result<i32> {
    let (sections, replace) = parse_flags(rest)?;
    if replace {
        anyhow::bail!("export 不支持 --replace");
    }
    let mut params = json!({ "schemaId": id });
    if let Some(s) = &sections {
        params["sections"] = json!(s);
    }
    let v = rpc_online("dict.export", params)?;
    let content = v
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("dict.export 返回了意外形状"))?;
    let path = resolve_path(file)?;
    std::fs::write(&path, content).map_err(|e| anyhow::anyhow!("写入 {path} 失败: {e}"))?;
    println!("✓ 已导出 {id} 词库数据到 {path}（{} 字节）", content.len());
    Ok(0)
}

fn cmd_import(id: &str, file: &str, rest: &[String]) -> anyhow::Result<i32> {
    let (sections, replace) = parse_flags(rest)?;
    let path = resolve_path(file)?;
    let content =
        std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("读取 {path} 失败: {e}"))?;
    let mut params = json!({ "schemaId": id, "content": content });
    if replace {
        params["strategy"] = json!("replace");
    }
    if let Some(s) = &sections {
        params["sections"] = json!(s);
    }
    let v = rpc_online("dict.import", params)?;
    print_import_report(&v);
    Ok(0)
}

/// 打印 `{sections:[{key, added/updated/unchanged | imported, skipped}]}` 逐段结果。
fn print_import_report(v: &Value) {
    let Some(secs) = v.get("sections").and_then(Value::as_array) else {
        println!("✓ 导入完成");
        return;
    };
    // core 只处理「所选 ∩ 文件所含」的段：交集为空时若不提示，会零输出静默成功。
    if secs.is_empty() {
        println!("⚠ 文件不含所选段类型，未导入任何数据");
        return;
    }
    fn label(k: &str) -> &str {
        match k {
            "userWords" => "用户词库",
            "tempWords" => "临时词库",
            "freq" => "词频",
            "shadow" => "候选调整",
            other => other,
        }
    }
    for s in secs {
        let key = s.get("key").and_then(Value::as_str).unwrap_or("?");
        let skipped = s.get("skipped").and_then(Value::as_u64).unwrap_or(0);
        let mut parts = Vec::new();
        for (field, name) in [
            ("added", "新增"),
            ("updated", "更新"),
            ("unchanged", "不变"),
            ("imported", "导入"),
        ] {
            if let Some(n) = s.get(field).and_then(Value::as_u64) {
                parts.push(format!("{name} {n}"));
            }
        }
        if skipped > 0 {
            parts.push(format!("跳过 {skipped}"));
        }
        println!("✓ {}: {}", label(key), parts.join(" · "));
    }
}

/// `dict weight-check`：离线体检各**方案**的词库权重值域。
///
/// ## 为什么必须是离线的独立命令，而不是启动时告警
///
/// 解析期告警（`ParseStats::log_weight_range`）只在**解析 yaml** 时触发，而词库一旦建了
/// `.wdat` 缓存就直接 mmap、不再解析。于是「老词库 + 新版本」这个最需要报警的组合
/// **一次也不会响** —— 实测：首次加载报一次，删掉缓存才会再报。
///
/// 本命令直接读源文件、无视缓存，是权威的那条路径。且受众是**方案作者**（终端用户看到
/// 告警也改不了词库），他们需要的是「打包前随手查一次」。
///
/// ## ⚠️ 统计按**方案**聚合，不按词库
///
/// 归一化是方案级的（一个映射函数施加到全部词库，保序 ⇒ 库间关系不变，见
/// `Schema::weight_spec`），故参数必须取**全方案合并后**的分布。中位数/分位数无法由各库
/// 的摘要合并得出，必须并原始值再算——这正是 `scan_weight_values` 存在的理由。
///
/// 上锚点建议 **p99 而非 max**：虎码方案级 max=1e11（12 条脏数据）而 p99=343,880，
/// 相差 30 万倍，用 max 会让量程被那 12 条吃掉。
fn cmd_weight_check(args: &[String]) -> anyhow::Result<i32> {
    let explicit_data = args.iter().position(|a| a == "--data");
    let data_dir = match explicit_data {
        Some(i) => std::path::PathBuf::from(
            args.get(i + 1)
                .ok_or_else(|| anyhow::anyhow!("--data 后缺少目录"))?,
        ),
        None => wind_config::Config::data_dir()
            .ok_or_else(|| anyhow::anyhow!("找不到数据目录，请用 --data <目录> 指定"))?,
    };
    // 扫描层序：不带 `--data` 时按程序真实的层序（user > custom > data），否则**只看
    // 指定的那个目录**——`--data` 的语义是「体检这个目录」（常指向构建产物），把
    // %APPDATA% 与 data_custom 混进来会让结果对不上用户指的那份数据。
    //
    // 不加层序的后果：定制版里只存在于 `data_custom` 的方案，`dict weight-check` 一个都
    // 看不见，而它恰恰是给定制者查词库权重用的工具。
    let schemas_dirs: Vec<std::path::PathBuf> = if explicit_data.is_some() {
        vec![data_dir.join("schemas")]
    } else {
        wind_config::Config::resource_layers_with(Some(&data_dir))
            .into_iter()
            .map(|d| d.join("schemas"))
            .collect()
    };
    if !schemas_dirs.iter().any(|d| d.is_dir()) {
        anyhow::bail!("{} 不是有效的数据目录（缺 schemas/）", data_dir.display());
    }
    println!("数据目录: {}", data_dir.display());
    for d in schemas_dirs.iter().skip(1).filter(|d| d.is_dir()) {
        println!("附加层  : {}", d.display());
    }
    println!("约定值域: 0 ~ {}\n", wind_dict::WEIGHT_RANGE_MAX);

    // 各层合并，同名方案由**靠前的层**胜出（与 `EngineManager::resolve_schema_file` 同序）。
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for dir in &schemas_dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for p in rd.filter_map(|e| e.ok().map(|e| e.path())) {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
            let Some(name) = name.filter(|n| n.ends_with(".schema.toml")) else {
                continue;
            };
            if seen.insert(name) {
                files.push(p);
            }
        }
    }
    // 按**文件名**排（不是全路径）：多层合并后按路径排会先按层分组，同一份报告里
    // 方案顺序随「它住在哪一层」跳动，读起来对不上。
    files.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()));

    let mut bad = 0usize;
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let Ok(schema) = toml::from_str::<wind_config::Schema>(&text) else {
            continue;
        };
        let sid = f
            .file_name()
            .map(|n| n.to_string_lossy().replace(".schema.toml", ""))
            .unwrap_or_default();
        let dicts: Vec<_> = schema
            .dictionaries
            .iter()
            .filter(|d| !d.path.is_empty())
            .collect();
        if dicts.is_empty() {
            continue;
        }

        // 全方案合并取值——参数必须按方案算，见函数文档。
        let mut all: Vec<i32> = Vec::new();
        let mut zero = 0usize;
        let mut files_n = 0usize;
        let mut missing: Vec<&str> = Vec::new();
        for d in &dicts {
            // 词库同样逐层找：定制层换掉的词库要按它实际生效的那一份体检。
            // （这里只认 `.dict.yaml` 源，wdat-only 的库读不出权重值，记 missing——
            //  与改造前对 data 层的行为一致。）
            let Some(path) = schemas_dirs
                .iter()
                .map(|dir| dir.join(&d.path))
                .find(|p| p.is_file())
            else {
                missing.push(&d.path);
                continue;
            };
            match wind_dict::codetable::scan_weight_values(&path) {
                Ok((mut ws, z)) => {
                    all.append(&mut ws);
                    zero += z;
                    files_n += 1;
                }
                Err(e) => println!("  ?  {} （{e}）", d.path),
            }
        }
        let sc = wind_dict::codetable::weight_scan_of(&mut all, zero);
        let configured = schema.weight_spec.is_some();

        print!("方案 {sid}  （{files_n} 个词库");
        if !missing.is_empty() {
            print!("，{} 个文件缺失", missing.len());
        }
        println!("）");

        if sc.weighted == 0 {
            // 整库无权重是**有意设计**（退化为文件顺序），不算问题。
            println!(
                "  -  全部词库无权重列（{} 条）：按文件顺序排，无需归一化",
                sc.zero
            );
        } else if sc.is_compliant() {
            println!(
                "  ok {} 条带权重  中位={} p99={} 最大={}",
                sc.weighted, sc.median, sc.p99, sc.max
            );
            if configured {
                println!("     ⚠ 已配 [weight_spec] 但本方案权重本就守约——通常不必配");
            }
        } else {
            bad += 1;
            println!(
                "  !! {} 条带权重  中位={} p99={} 最大={}  超范围 {} 条（{:.1}%）",
                sc.weighted,
                sc.median,
                sc.p99,
                sc.max,
                sc.over_range,
                sc.over_pct()
            );
            if configured {
                println!("     已配 [weight_spec] —— 归一化生效，此处仅报源数据分布");
            } else {
                println!("     建议在方案文件顶层追加（上锚点取 p99，离群值 clamp）：");
                println!("       [weight_spec]");
                println!("       median = {}", sc.median);
                println!("       max = {}", sc.p99);
                println!("       mode = \"log\"");
            }
        }
        println!();
    }
    if bad == 0 {
        println!("全部方案的词库权重均在约定值域内。");
    } else {
        println!("{bad} 个方案超出约定值域，跨来源排序（短语 vs 码表）会失真。");
        println!("⚠️ 拼音方案即使超范围通常也不该配——其权重刻意在另一条轴，且引擎侧有按");
        println!(
            "   原始权重标定的绝对阈值（COMPLETION_FAR_WEIGHT_FLOOR 等），归一化会让它们失效。"
        );
    }
    Ok(0)
}
