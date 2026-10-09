//! 观测：在真实用户库上量一次「关库重开」（`drop_page_cache`）要多久。
//!
//! 回收发生在回收线程上，但它持 db 锁：若恰逢按键，`with_db` 要陪等这一下。本机 zfs 实测约
//! 7 ms（几乎全在关库刷盘）；Windows 盘与杀软下另量。用法（Windows 靶机上跑交叉编出的测试 exe）：
//!
//! ```text
//! set WIND_DB=C:\path\to\copy-of-userdata.redb
//! drop_page_cache_timing-xxxx.exe --ignored --nocapture
//! ```
//!
//! 只读一份**副本**：库在服务手里开着，直接开会撞文件锁。

use std::time::Instant;
use wind_store::Store;

#[test]
#[ignore = "观测，须设 WIND_DB 指向用户库副本"]
fn time_drop_page_cache_on_a_real_store() {
    let Ok(path) = std::env::var("WIND_DB") else {
        println!("未设 WIND_DB，跳过");
        return;
    };
    let t = Instant::now();
    let s = Store::open(std::path::Path::new(&path)).expect("开库");
    println!("开库 {:?}", t.elapsed());
    for round in 1..=5 {
        let t = Instant::now();
        let n = s
            .search_user_words_prefix("pinyin", "", 0)
            .expect("全表扫")
            .len();
        let scan = t.elapsed();
        let t = Instant::now();
        s.drop_page_cache().expect("回收");
        let drop = t.elapsed();
        let t = Instant::now();
        let _ = s
            .search_user_words_prefix("pinyin", "ni", 20)
            .expect("冷查询");
        let cold = t.elapsed();
        println!(
            "第 {round} 轮：全表扫 {n} 条 {scan:?}；关库重开 {drop:?}；随后一次窄查询（冷） {cold:?}"
        );
    }
}
