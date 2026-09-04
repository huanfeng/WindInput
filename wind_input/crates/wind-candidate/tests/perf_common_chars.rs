//! 常用性判定的热路径成本对照。
//!
//! `#[ignore]`：这是量成本的，不是判对错的，不进常规 CI。手动跑：
//! ```text
//! cargo test --release -p wind-candidate --test perf_common_chars -- --ignored --nocapture
//! ```
//!
//! `is_string_common` 对**每个候选**都要跑一遍，字素簇分割比逐 char 贵，故用
//! `has_multi_char_keys` 闸门把成本挡在「用户真的登记过多码位覆盖」之后。这里量的就是
//! 那道闸门两侧的差距。
use std::time::Instant;
use wind_candidate::CommonChars;

fn bench(
    label: &str,
    cc: &CommonChars,
    cs: &wind_candidate::CharsetRegistry,
    texts: &[&str],
    rounds: usize,
) {
    // 预热，避免首轮的分支预测/缓存冷启动混进读数。
    let mut sink = 0usize;
    for t in texts {
        sink += cc.is_string_common(t, cs) as usize;
    }
    let t0 = Instant::now();
    for _ in 0..rounds {
        for t in texts {
            sink += cc.is_string_common(t, cs) as usize;
        }
    }
    let el = t0.elapsed();
    let n = rounds * texts.len();
    println!(
        "{label:<28} {n} 次 {:>8.2?}  单次 {:>6.1} ns   (sink={sink})",
        el,
        el.as_nanos() as f64 / n as f64
    );
}

#[test]
#[ignore = "性能对照，手动跑：-- --ignored --nocapture"]
fn bench_is_string_common_hot_path() {
    // 一屏候选的典型形态：单字、二字词、长词、英文、带 emoji。
    let texts = [
        "我",
        "的",
        "东西",
        "输入法",
        "计算机",
        "hello",
        "iPhone",
        "一家人",
        "中华人民共和国",
        "\u{26BD}\u{FE0F}",
    ];
    let base: Vec<char> = "我的东西输入法计算机一家人中华民共和国".chars().collect();
    let rounds = 200_000;

    // 三组只在覆盖上不同，registry 是同一份——本测试量的是闸门两侧的差距，
    // registry 若跟着变，读数里就混进了别的东西。
    let (cs, _) = wind_candidate::CharsetRegistry::compile(vec![wind_candidate::ClassSpec {
        key: "common_han".into(),
        members: base.iter().map(|c| c.to_string()).collect(),
        scope: Some(wind_candidate::Scope::Han),
        default_common: Some(true),
        outside_common: Some(false),
        ..Default::default()
    }]);

    let plain = CommonChars::from_base(base.clone());
    bench("无覆盖（闸门关）", &plain, &cs, &texts, rounds);

    let mut single = CommonChars::from_base(base.clone());
    single.set_overrides(
        (0..200u32).map(|i| (char::from_u32(0x4E00 + i).unwrap().to_string(), i % 2 == 0)),
    );
    bench("200 条单码位覆盖（闸门关）", &single, &cs, &texts, rounds);

    let mut multi = CommonChars::from_base(base);
    let mut v: Vec<(String, bool)> = (0..200u32)
        .map(|i| (char::from_u32(0x4E00 + i).unwrap().to_string(), i % 2 == 0))
        .collect();
    // 一条多码位覆盖即打开闸门——这是最坏情况，也是唯一会付字素簇分割代价的情况。
    v.push(("\u{26BD}\u{FE0F}".to_string(), false));
    multi.set_overrides(v);
    bench("+1 条多码位覆盖（闸门开）", &multi, &cs, &texts, rounds);
}
