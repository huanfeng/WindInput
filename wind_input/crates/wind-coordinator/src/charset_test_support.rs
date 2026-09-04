//! 字符类相关测试的共用夹具。
//!
//! # 为什么测试要显式准备 `charsets/`
//!
//! `exclude_blocks` / `include_blocks` 里能写的名字，绝大多数是**内置区块类**（代码
//! 提供，夹具不必操心）。唯独 `emoji` 不是——它的成员由 `data/charsets/emoji.yaml`
//! 那份按 UTS #51 生成的精确字表给出（旧的「五个块并集」口径两个方向都不准，见
//! `docs/design/charset-classification.md` §5.5）。
//!
//! ⇒ 夹具缺这个目录时，`emoji` 解析不出来、那一行配置被当成「未识别」跳过，测试会以
//! **「开关没生效」**的形态失败——与真实故障长得一模一样，排查要多绕一大圈。

use std::path::{Path, PathBuf};

/// 把仓里真实的 `data/charsets/` 原样复制到 `base_dir` 下。
///
/// ⚠️ 找不到源目录就 **panic 而不是静默跳过**，理由见模块头。
pub(crate) fn copy_factory_charsets(base_dir: &Path) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("定位不到仓库根");
    let src = repo.join("data").join("charsets");
    assert!(src.is_dir(), "找不到出厂字符类目录 {}", src.display());
    let dst = base_dir.join("charsets");
    std::fs::create_dir_all(&dst).unwrap();
    for e in std::fs::read_dir(&src).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), dst.join(e.file_name())).unwrap();
    }
}

/// 建一个只装了出厂字符类的临时数据目录，供 `Coordinator::new_headless` 当 `data_dir`。
///
/// `tag` 要在同一个测试二进制内唯一——测试并行跑，同名目录会互相删。
pub(crate) fn charsets_only_data_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_charsets_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    copy_factory_charsets(&dir);
    dir
}
