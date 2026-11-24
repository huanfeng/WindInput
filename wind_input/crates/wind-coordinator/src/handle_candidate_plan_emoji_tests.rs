//! emoji 插入规划：选宿主、查表、去重、截断。
use super::plan_emoji_insertions;
use wind_candidate::Candidate;

fn c(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        ..Default::default()
    }
}
fn prefix(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        is_prefix: true,
        ..Default::default()
    }
}
/// 词表：只认「开心」「动物」两个词。
fn table(t: &str) -> Option<String> {
    match t {
        "开心" => Some("😄 😊 🙂 😁".into()),
        "动物" => Some("🐶 🐱".into()),
        _ => None,
    }
}

#[test]
fn extends_first_host_only_by_default() {
    let v = [c("开心"), c("动物")];
    let got = plan_emoji_insertions(&v, true, 1, 3, 2, table);
    assert_eq!(got.len(), 1, "max_hosts=1 只扩首选");
    assert_eq!(got[0].0, 0);
}

/// ★ `max_hosts` 的语义是「只看列表前 N 条」，**不是**「找够 N 个合格宿主」。
///
/// 差别在这里：首条查不到 emoji 时，前者什么都不产出，后者会往下找到第二条。
/// 前者可预测（首选没有就是没有），后者会让 emoji 飘忽地出现在不同位置。
#[test]
fn max_hosts_is_a_window_not_a_quota() {
    let v = [c("无表词"), c("开心")];
    let got = plan_emoji_insertions(&v, true, 1, 3, 2, table);
    assert!(got.is_empty(), "窗口内首条没命中就不产出，不该顺延到第二条");
    // 窗口放大到 2 才轮得到「开心」。
    let got = plan_emoji_insertions(&v, true, 2, 3, 2, table);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0, 1);
}

#[test]
fn truncates_to_max_per_word() {
    let v = [c("开心")];
    let got = plan_emoji_insertions(&v, true, 1, 2, 2, table);
    assert_eq!(got[0].1, vec!["😄".to_string(), "😊".to_string()]);
}

#[test]
fn skips_short_words() {
    let v = [c("开心")];
    assert!(
        plan_emoji_insertions(&v, true, 1, 3, 3, table).is_empty(),
        "min_chars=3 时两字词不触发"
    );
}

/// `exact` 档跳过前缀补全；`all` 档放行。
#[test]
fn exact_scope_skips_prefix_candidates() {
    let v = [prefix("开心")];
    assert!(plan_emoji_insertions(&v, true, 1, 3, 2, table).is_empty());
    assert_eq!(plan_emoji_insertions(&v, false, 1, 3, 2, table).len(), 1);
}

/// 词库里本来就有那个 emoji 时不再重复给一条。
#[test]
fn does_not_duplicate_existing_candidate() {
    let v = [c("开心"), c("😄")];
    let got = plan_emoji_insertions(&v, true, 1, 3, 2, table);
    assert_eq!(
        got[0].1,
        vec!["😊".to_string(), "🙂".to_string(), "😁".to_string()],
        "已在候选里的 😄 应被跳过"
    );
}

/// 同一符号带 / 不带 U+FE0F 只算一个：表内重复只留先到者的原形，已有候选也挡得住。
/// 回归：上游表开分类后「奖项」出 `🏅️ 🏅 🎖️`，三个配额两个是同一枚奖牌。
#[test]
fn dedups_variation_selector_forms() {
    let fe0f = |t: &str| match t {
        "奖项" => Some("🏅\u{FE0F} 🏅 🎖\u{FE0F} 🥇".to_string()),
        "打勾" => Some("✅\u{FE0F} ✔\u{FE0F}".to_string()),
        _ => None,
    };
    let got = plan_emoji_insertions(&[c("奖项")], true, 1, 3, 2, fe0f);
    assert_eq!(
        got[0].1,
        ["🏅\u{FE0F}", "🎖\u{FE0F}", "🥇"],
        "裸 🏅 是前一条的另一种写法，跳过；保留先到的带 FE0F 形态"
    );
    let got = plan_emoji_insertions(&[c("打勾"), c("✅")], true, 1, 3, 2, fe0f);
    assert_eq!(got[0].1, ["✔\u{FE0F}"], "候选里已有 ✅，表里的 ✅️ 不再给");
}

/// 两个宿主命中同一个 emoji 时只给一条。
#[test]
fn dedups_across_hosts() {
    let dup = |t: &str| match t {
        "甲词" | "乙词" => Some("🌟".to_string()),
        _ => None,
    };
    let v = [c("甲词"), c("乙词")];
    let got = plan_emoji_insertions(&v, true, 2, 3, 2, dup);
    assert_eq!(got.len(), 1, "第二个宿主的重复 emoji 被去掉后不产出空条目");
    assert_eq!(got[0].0, 0);
}

/// 已经是 emoji 扩展的候选不再被当作宿主（防二次扩展）。
#[test]
fn skips_existing_emoji_candidates() {
    let mut e = c("开心");
    e.is_emoji_suggestion = true;
    let v = [e];
    assert!(plan_emoji_insertions(&v, true, 1, 3, 2, table).is_empty());
}
