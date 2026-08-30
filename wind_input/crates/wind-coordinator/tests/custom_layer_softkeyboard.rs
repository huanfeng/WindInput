//! 软键盘映射表的三层按面合并：`data < data_custom < %APPDATA%`。
//!
//! 软键盘**刻意不走** `Config::resolve_data_file`（那是「靠前的层存在就整份取代出厂」），
//! 而是按面合并——用户只想改一个键时不该失去其余的面。加了定制层之后，这个自成一套的
//! 加载路径必须同样认 `data_custom/`，否则定制者放进去的那份被**静默忽略**：程序一切
//! 正常、日志里连 WARN 都没有，正是本计划反复处理的那类故障。
//!
//! 本文件钉四条，都是「摘掉实现只表现为某个键出的字不对」、在候选面/菜单上不可观察的：
//!
//! | # | 断言 | 摘掉什么会红 |
//! |---|---|---|
//! | 1 | 三层各定义一部分面 ⇒ 合并结果含全部面 | 定制层没接进来 |
//! | 2 | 同名面同一个键：`data < custom < user` | **叠加顺序反了**（用户改的键被定制版盖回去） |
//! | 3 | 无用户层时定制层的面生效 | 定制层只在有用户层时才被读 |
//! | 4 | 定制层解析失败 ⇒ 跳过它，另两层照常 | 一层坏掉打回内置兜底表 |
//!
//! ⚠️ **第 2 条是层序方向的唯一守门**：夹具刻意让 `p_shared.q` 三层各写一个不同的值，
//! 且面的**出现序**也一并断言（`p_custom` 必须排在 `p_user` 前）——把 `.rev()` 摘掉、
//! 或把两层的叠加顺序对调，两处各红一次。只断言「custom 生效了」是不够的：顺序反了时
//! 定制层同样生效，只是把用户的覆盖盖掉了。
//!
//! 为什么必须是集成测试（独立进程）：`Config::custom_manifest()` 用 OnceLock 缓存，
//! 一个进程只能有一种层状态。本文件全程「清单在场」，三个子场景靠**换盘上的文件**切换
//! （加载每次构造都重来一遍，缓存的只有目录）。「清单不在场」那一态在
//! `custom_layer_softkeyboard_no_custom.rs`。
//!
//! ⚠️ 本用例**不依赖 `build_dev/data`**：软键盘完全脱离引擎（无方案、无码表），夹具
//! 全部自造，故不会静默跳过。

use std::path::{Path, PathBuf};
use wind_config::Config;
use wind_coordinator::Coordinator;

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

/// 出厂画布：一个独有面 + 一个三层都要动的共享面。
const DATA_SK: &str = "\
[[pages]]
id = \"p_data\"
name = \"出厂独有\"
keys = { q = \"D_ONLY\" }

[[pages]]
id = \"p_shared\"
name = \"共享面\"
keys = { q = \"D_Q\", w = \"D_W\", e = \"D_E\" }
";

/// 定制层：只改共享面的两个键 + 加一个自己的面（**不整份复制**，正是要支持的用法）。
const CUSTOM_SK: &str = "\
[[pages]]
id = \"p_shared\"
keys = { q = \"C_Q\", w = \"C_W\" }

[[pages]]
id = \"p_custom\"
name = \"定制独有\"
keys = { q = \"C_ONLY\" }
";

/// 用户层：只改共享面的一个键 + 加一个自己的面。
const USER_SK: &str = "\
[[pages]]
id = \"p_shared\"
keys = { q = \"U_Q\" }

[[pages]]
id = \"p_user\"
name = \"用户独有\"
keys = { q = \"U_ONLY\" }
";

fn page_ids(coord: &Coordinator) -> Vec<String> {
    coord
        .debug_softkeyboard()
        .pages()
        .iter()
        .map(|p| p.id.clone())
        .collect()
}

/// 共享面上某个键的基础层输出。面不存在时给出可辨认的失败信息。
fn shared_out(coord: &Coordinator, slot: &str) -> String {
    coord
        .debug_softkeyboard()
        .page("p_shared")
        .unwrap_or_else(|| panic!("共享面 p_shared 必须存在（出厂画布铺底）"))
        .output(slot, false)
        .unwrap_or_else(|| panic!("共享面的键位 {slot} 应有映射"))
        .to_string()
}

#[test]
fn softkeyboard_merges_three_layers_lowest_to_highest() {
    // ⚠️ 目录名带 pid：多 worktree / 多会话并行跑测试时固定名会互删夹具。
    let tmp = std::env::temp_dir().join(format!(
        "wind_coord_custom_softkeyboard-{}",
        std::process::id()
    ));
    let root = tmp.join("install");
    let data = root.join("data");
    let custom = root.join("data_custom");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();

    // 清单必须在**任何** OnceLock 初始化之前就位。本用例只用它把定制层「打开」，
    // 不做减法。
    write_at(
        &custom,
        "custom.toml",
        "[custom]\nid = \"sk-edition\"\nversion = \"1.0\"\n",
    );

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_DATADIR_CONF", &conf);
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }
    assert_eq!(
        Config::user_config_dir(),
        Some(user.clone()),
        "前置条件：用户目录须已重定向，否则本测试会读写真实 %APPDATA%"
    );
    assert_eq!(
        Config::custom_data_dir(),
        Some(custom.clone()),
        "前置条件：清单在场时 custom 层必须启用"
    );

    let data_sk = write_at(&data, wind_softkeyboard::FILE_NAME, DATA_SK);
    let custom_sk = write_at(&custom, wind_softkeyboard::FILE_NAME, CUSTOM_SK);
    let user_sk = write_at(&user, wind_softkeyboard::FILE_NAME, USER_SK);

    // ── 1. 三层齐全 ───────────────────────────────────────────────────────────
    let coord = Coordinator::new_headless(Config::default(), Some(&data));
    assert_eq!(
        page_ids(&coord),
        vec!["p_data", "p_shared", "p_custom", "p_user"],
        "三层各自的面必须**全部**在场（整份取代的话只剩最靠前那层的面），\
         且出现序即叠加序——定制层的面排在用户层之前"
    );
    assert_eq!(
        shared_out(&coord, "q"),
        "U_Q",
        "★ 层序方向：三层都写了这个键，赢家必须是用户层。\
         拿到 C_Q = 叠加顺序反了（定制版把用户改的键盖回去）；拿到 D_Q = 两层都没叠上"
    );
    assert_eq!(
        shared_out(&coord, "w"),
        "C_W",
        "用户层没碰这个键 ⇒ 定制层的值生效（定制层没接进来的话是 D_W）"
    );
    assert_eq!(
        shared_out(&coord, "e"),
        "D_E",
        "两个覆盖层都没碰的键保持出厂值——按面合并不是整份取代"
    );
    assert_eq!(
        coord
            .debug_softkeyboard()
            .page("p_shared")
            .map(|p| p.name.as_str()),
        Some("共享面"),
        "覆盖层只打补丁、没写 name ⇒ 保留出厂面名（合并语义没被改动）"
    );
    drop(coord);

    // ── 2. 只有 data + custom（无用户层）────────────────────────────────────
    std::fs::remove_file(&user_sk).unwrap();
    let coord = Coordinator::new_headless(Config::default(), Some(&data));
    assert_eq!(
        page_ids(&coord),
        vec!["p_data", "p_shared", "p_custom"],
        "没有用户层时定制层照常叠加（定制层若只在有用户层时才读，这里会少一个面）"
    );
    assert_eq!(
        shared_out(&coord, "q"),
        "C_Q",
        "用户层缺席 ⇒ 定制层是最高层"
    );
    assert_eq!(shared_out(&coord, "w"), "C_W");
    drop(coord);

    // ── 3. 定制层解析失败 ⇒ 跳过它，另两层照常 ───────────────────────────────
    std::fs::write(&user_sk, USER_SK).unwrap();
    std::fs::write(&custom_sk, "[[pages]]\nid = \"p_custom\"\nkeys = { q =").unwrap();
    let coord = Coordinator::new_headless(Config::default(), Some(&data));
    assert_eq!(
        page_ids(&coord),
        vec!["p_data", "p_shared", "p_user"],
        "坏掉的定制层被整层跳过，出厂与用户层的面**一个不少**——\
         回落内置兜底表的话这里一个自造面都不会有"
    );
    assert_eq!(
        shared_out(&coord, "q"),
        "U_Q",
        "定制层坏掉不影响用户层的覆盖"
    );
    assert_eq!(
        shared_out(&coord, "w"),
        "D_W",
        "定制层坏掉 ⇒ 它改的那个键回到出厂值（前一步这里是 C_W，故本条不恒真）"
    );
    drop(coord);

    assert_eq!(
        std::fs::read_to_string(&data_sk).unwrap(),
        DATA_SK,
        "★ 程序永不写安装层：出厂那份必须一个字节都没变（不变量 3）"
    );
    assert!(
        custom_sk.is_file(),
        "★ 程序永不写 data_custom：解析失败也不得把它删掉或“修好”（不变量 3）"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}
