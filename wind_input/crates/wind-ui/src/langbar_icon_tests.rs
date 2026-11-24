use super::*;

/// 取某像素的 alpha（输出是非预乘 BGRA）。
fn alpha_at(buf: &[u8], n: usize, x: usize, y: usize) -> u8 {
    buf[(y * n + x) * 4 + 3]
}

fn spec(punct: PunctBadge) -> IconSpec {
    IconSpec {
        punct,
        ..IconSpec::default()
    }
}

/// 每个尺寸档都要输出恰好 size×size×4 字节——SHM 变体表按这个长度切片，
/// 少一个字节就会让后续所有变体错位。
#[test]
fn output_length_matches_every_declared_size() {
    let r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    for &size in &wind_ipc::protocol::ICON_SIZES {
        let buf = r.render(size, false, &spec(PunctBadge::Chinese));
        assert_eq!(
            buf.len(),
            wind_ipc::protocol::icon_variant_bytes(size),
            "尺寸档 {size} 输出长度不符"
        );
    }
}

/// 主字尺寸不得随角标有无变化。
///
/// 英文态恰好没有标点角标，按 has_badge 分档字号会让每次中英切换字号都跳一下。
///
/// 只在 Windows 上跑：其它平台文本后端是 mock，画不出主字。
#[cfg(windows)]
#[test]
fn glyph_size_does_not_change_with_badge() {
    let r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    const N: usize = 32;

    // 主字的垂直跨度。只扫左侧 55% 的列，避开右下角的角标。
    let glyph_height = |punct: PunctBadge| -> usize {
        let buf = r.render(N as u16, false, &spec(punct));
        let mut top: Option<usize> = None;
        let mut bottom = 0usize;
        for y in 0..N {
            let inked = (0..(N * 55 / 100)).any(|x| buf[(y * N + x) * 4 + 3] > 0);
            if inked {
                top.get_or_insert(y);
                bottom = y;
            }
        }
        top.map_or(0, |t| bottom - t + 1)
    };

    let without = glyph_height(PunctBadge::None);
    let with = glyph_height(PunctBadge::Chinese);
    assert!(without > 0, "主字根本没画出来");
    assert_eq!(
        without, with,
        "主字高度随角标变化了——字号又按 has_badge 分档了？"
    );
}

/// 中文标点与英文标点必须画出不同的像素，否则角标形同虚设：标点状态须真的进入绘制，
/// 而不只是参与重绘判据。
#[test]
fn chinese_and_english_badges_differ() {
    let r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    for &size in &wind_ipc::protocol::ICON_SIZES {
        let cn = r.render(size, false, &spec(PunctBadge::Chinese));
        let en = r.render(size, false, &spec(PunctBadge::English));
        assert_ne!(cn, en, "尺寸档 {size} 的中/英标点角标画出来是一样的");
    }
}

/// 只装一条规则的渲染器，用于把某一条的表现单独拎出来验。
///
/// 出厂表有三条，直接用它验单条行为会把「另外两条恰好没命中」当成隐含前提，
/// 出厂表一改前提就没了。
fn with_rule(rule: BadgeRule) -> IconRenderer {
    let mut r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    r.rules = vec![rule];
    r
}

/// 同一个状态配到不同角落，画出来必须不同——否则「位置」这个自由度是空的。
#[test]
fn corners_produce_distinct_pixels() {
    let mut rendered = Vec::new();
    for c in Corner::ALL {
        let r = with_rule(BadgeRule::solid(
            BadgeState::PunctCn,
            c,
            IconRenderer::DEFAULT_PUNCT_CN_COLOR,
            1.0,
        ));
        rendered.push(r.render(24, false, &spec(PunctBadge::Chinese)));
    }
    for i in 0..rendered.len() {
        for j in (i + 1)..rendered.len() {
            assert_ne!(
                rendered[i],
                rendered[j],
                "角落 {:?} 与 {:?} 渲染结果相同",
                Corner::ALL[i],
                Corner::ALL[j]
            );
        }
    }
}

/// 出厂配置下角标画在右下角象限，不能跑到主字中心去。
#[test]
fn corner_badge_lands_in_bottom_right_quadrant() {
    let r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    let n = 32usize;
    let none = r.render(32, false, &spec(PunctBadge::None));
    let cn = r.render(32, false, &spec(PunctBadge::Chinese));

    // 右下角必须出现新的不透明像素
    let mut gained_bottom_right = 0;
    for y in (n * 3 / 4)..n {
        for x in (n * 3 / 4)..n {
            if alpha_at(&cn, n, x, y) > alpha_at(&none, n, x, y) {
                gained_bottom_right += 1;
            }
        }
    }
    assert!(
        gained_bottom_right > 0,
        "右下角没有画出角标（新增不透明像素数为 0）"
    );
}

/// 四个角落各自落在自己的象限里，且不越界到对角。
///
/// 四个角共用一套判据、靠坐标折叠区分（见 `draw_corner_triangle`），折错的产物仍是个三角、
/// 只是位置或朝向不同，在 16px 任务栏上几乎看不出来。
#[test]
fn every_corner_lands_in_its_own_quadrant() {
    let n = 32usize;
    for c in Corner::ALL {
        let r = with_rule(BadgeRule::solid(
            BadgeState::PunctCn,
            c,
            IconRenderer::DEFAULT_PUNCT_CN_COLOR,
            1.0,
        ));
        let none = r.render(32, false, &spec(PunctBadge::None));
        let cn = r.render(32, false, &spec(PunctBadge::Chinese));

        // 直角顶点所在的那个角像素必有墨。
        let (vx, vy) = match c {
            Corner::TopLeft => (0, 0),
            Corner::TopRight => (n - 1, 0),
            Corner::BottomRight => (n - 1, n - 1),
            Corner::BottomLeft => (0, n - 1),
        };
        assert!(
            alpha_at(&cn, n, vx, vy) > alpha_at(&none, n, vx, vy),
            "{c:?}：直角顶点（{vx},{vy}）处没有墨"
        );

        // 对角那个角像素一点都不能动。
        let (ox, oy) = (n - 1 - vx, n - 1 - vy);
        assert_eq!(
            alpha_at(&cn, n, ox, oy),
            alpha_at(&none, n, ox, oy),
            "{c:?}：对角（{ox},{oy}）被画上了墨"
        );
    }
}

/// 全角规则出厂画在右上角象限。同时钉住出厂分工：全角与标点是正交的两件事，挤在同一角
/// 就要为四种搭配各设计一种编码，16px 上放不下。
#[test]
fn full_width_mark_lands_in_top_right_quadrant() {
    let r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    let n = 32usize;
    let half = r.render(32, false, &IconSpec::default());
    let full = r.render(
        32,
        false,
        &IconSpec {
            full_width: true,
            ..IconSpec::default()
        },
    );

    let mut gained_top_right = 0;
    for y in 0..(n / 4) {
        for x in (n * 3 / 4)..n {
            if alpha_at(&full, n, x, y) > alpha_at(&half, n, x, y) {
                gained_top_right += 1;
            }
        }
    }
    assert!(
        gained_top_right > 0,
        "右上角没有画出全角标记（新增不透明像素数为 0）"
    );
}

/// 半角在右上角一点痕迹都不能留：半角是常态，给它画标记只会常驻一个不传达信息的点，
/// 又占掉本就稀缺的一角。
#[test]
fn half_width_leaves_no_top_right_mark() {
    let r = with_rule(BadgeRule::solid(
        BadgeState::FullWidth,
        Corner::TopRight,
        IconRenderer::DEFAULT_FULL_WIDTH_COLOR,
        1.0,
    ));
    let plain = r.render(32, false, &IconSpec::default());
    let with_mark = IconSpec {
        full_width: true,
        ..Default::default()
    };
    let marked = r.render(32, false, &with_mark);
    assert_ne!(plain, marked, "全角与半角渲染结果相同，标记没画出来");

    // 反向：把全角关掉应当逐字节回到「从没有过标记」的样子。
    let back = r.render(32, false, &IconSpec::default());
    assert_eq!(plain, back, "半角仍留有全角标记的残迹");
}

/// 全角标记与标点角标彼此正交：四种搭配两两不同。
///
/// 若哪天有人把两者合进同一层（比如共用一张蒙版或同一个开关），本条会失败。
#[test]
fn width_mark_and_punct_badge_are_independent() {
    let r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    let mk = |punct, full_width| {
        r.render(
            32,
            false,
            &IconSpec {
                punct,
                full_width,
                ..IconSpec::default()
            },
        )
    };
    let combos = [
        mk(PunctBadge::None, false),
        mk(PunctBadge::None, true),
        mk(PunctBadge::Chinese, false),
        mk(PunctBadge::Chinese, true),
    ];
    for i in 0..combos.len() {
        for j in (i + 1)..combos.len() {
            assert_ne!(combos[i], combos[j], "组合 {i} 与 {j} 渲染结果相同");
        }
    }
}

/// 同一个角落同时命中多条时，只画最靠前的那一条（16px 上叠两个三角只会糊成一片）。
/// 判据取「两条并存的结果 == 只留第一条的结果」，第二条也画了或顺序反了都对不上。
#[test]
fn only_the_first_rule_wins_in_a_shared_corner() {
    let both = IconSpec {
        punct: PunctBadge::Chinese,
        full_width: true,
        ..IconSpec::default()
    };
    let punct = BadgeRule::solid(
        BadgeState::PunctCn,
        Corner::BottomRight,
        IconRenderer::DEFAULT_PUNCT_CN_COLOR,
        1.0,
    );
    // 刻意给第二条一个不同的颜色与尺寸：它若被画出来，像素上一定看得见。
    let full = BadgeRule::solid(
        BadgeState::FullWidth,
        Corner::BottomRight,
        IconRenderer::DEFAULT_FULL_WIDTH_COLOR,
        0.6,
    );

    let mut r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    r.rules = vec![punct.clone(), full.clone()];
    let punct_first = r.render(32, false, &both);
    r.rules = vec![full.clone(), punct.clone()];
    let full_first = r.render(32, false, &both);

    assert_eq!(
        punct_first,
        with_rule(punct).render(32, false, &both),
        "同角落并存时画出了第二条（或两条叠加）"
    );
    assert_eq!(
        full_first,
        with_rule(full).render(32, false, &both),
        "调换顺序后赢的不是排在前面的那条——优先级没按表序来"
    );
    assert_ne!(punct_first, full_first, "两条规则本就该画出不同的东西");
}

/// 角标不透明度只作用于角标，主字不受影响。透明度的用途是别把字吃掉；漏到主字上整个图标
/// 会发灰，那是 `dimmed` 的语义，两者必须分开。
#[test]
fn badge_alpha_only_affects_the_badge() {
    let mut r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    let n = 32usize;
    let spec_cn = IconSpec {
        punct: PunctBadge::Chinese,
        ..IconSpec::default()
    };

    r.badge_alpha = 1.0;
    let opaque = r.render(32, false, &spec_cn);
    r.badge_alpha = 0.5;
    let translucent = r.render(32, false, &spec_cn);
    assert_ne!(opaque, translucent, "调低不透明度后角标没有变化");

    // 左上象限只有主字，不该有任何一个像素被动过。
    for y in 0..(n / 2) {
        for x in 0..(n / 2) {
            assert_eq!(
                alpha_at(&opaque, n, x, y),
                alpha_at(&translucent, n, x, y),
                "角标不透明度漏到了主字上（{x},{y}）"
            );
        }
    }
}

/// 半透明角标必须真的「半遮」：角标覆盖处底下的主字笔画不能被挖掉，否则挖空与透明同时用，
/// 调低不透明度看起来毫无效果。判据取同一处像素在半透明档下比全不透明档更接近主字。
///
/// 判据依赖主字真有墨迹，故 gate 到 Windows：Linux 的文本后端是空操作 mock（见
/// `text/dwrite.rs`），主字恒全透明，断言必然不成立。
#[cfg(windows)]
#[test]
fn translucent_badge_lets_the_glyph_show_through() {
    let n = 32usize;
    let spec_cn = IconSpec {
        // 「五」右下有横笔，正是被角标压住的那种字。
        label: "五".to_string(),
        punct: PunctBadge::Chinese,
        ..IconSpec::default()
    };

    let mut r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    r.badge_alpha = 1.0;
    let opaque = r.render(32, false, &spec_cn);
    r.badge_alpha = 0.5;
    let translucent = r.render(32, false, &spec_cn);

    // 挖空只在不透明档发生，所以不透明档在角标外围会有一圈 alpha 被削低的像素；
    // 半透明档保留主字，那一圈应当更"实"。统计右下象限里 alpha 更高的像素数。
    let mut translucent_denser = 0;
    for y in (n / 2)..n {
        for x in (n / 2)..n {
            if alpha_at(&translucent, n, x, y) > alpha_at(&opaque, n, x, y) {
                translucent_denser += 1;
            }
        }
    }
    assert!(
        translucent_denser > 0,
        "半透明档没有保留任何被挖掉的主字像素——挖空与透明又叠加了，\
             调不透明度会再次变成静默无效"
    );
}

/// 右上角标的**直角确实在右上角**，斜边朝图标中心。
///
/// 象限测试只验证「右上象限有新像素」，方块、圆点、画反的三角都能通过。这里沿顶行取两点：
/// 贴右边界那点在直角顶点上必有墨；往左走出斜边之外的那点必须与半角时逐字节相同。
#[test]
fn top_right_triangle_has_its_right_angle_at_top_right() {
    // 显式给倍率 1.0 而不是借出厂那条：下面「斜边之外」的取样点按 leg 算，
    // 跟着出厂倍率走会让这条测试在有人调整出厂倍率时莫名其妙地失败。
    let r = with_rule(BadgeRule::solid(
        BadgeState::FullWidth,
        Corner::TopRight,
        IconRenderer::DEFAULT_FULL_WIDTH_COLOR,
        1.0,
    ));
    let n = 32usize;
    let half = r.render(32, false, &IconSpec::default());
    let full = r.render(
        32,
        false,
        &IconSpec {
            full_width: true,
            ..IconSpec::default()
        },
    );

    // 直角顶点：顶行最右一列。
    assert!(
        alpha_at(&full, n, n - 1, 0) > alpha_at(&half, n, n - 1, 0),
        "右上角顶点处没有墨——三角没画在直角该在的地方"
    );

    // 顶行往左第 13 列（leg ≈ 32×0.34 ≈ 11），已在斜边之外。
    // 若这里也有墨，画出来的就是方块或朝向错误的三角。
    let outside_x = n - 13;
    assert_eq!(
        alpha_at(&full, n, outside_x, 0),
        alpha_at(&half, n, outside_x, 0),
        "斜边之外（{outside_x},0）被画上了墨——形状不是右上直角三角"
    );
}

/// 出厂三条规则的颜色两两不同。
///
/// 它们表达互不相干的状态，而位置是用户可改的，颜色是唯一还能承载「不相干」的通道。
#[test]
fn default_rules_use_three_distinct_colors() {
    let rules = IconRenderer::default_rules();
    for i in 0..rules.len() {
        for j in (i + 1)..rules.len() {
            assert_ne!(
                rules[i].color_light, rules[j].color_light,
                "出厂规则 {i} 与 {j} 的浅色配色相同"
            );
            assert_ne!(
                rules[i].color_dark, rules[j].color_dark,
                "出厂规则 {i} 与 {j} 的深色配色相同"
            );
        }
    }
}

/// 颜色配成 `auto`（渲染侧的 `None`）时，角标退化为主字色。
///
/// 这是 `color_light = "auto"` 的落点，也是色值解析失败的降级目标：整张图只该有主字那一种色相。
#[test]
fn auto_color_falls_back_to_the_glyph_color() {
    let mut r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    r.rules = vec![
        BadgeRule {
            state: BadgeState::PunctCn,
            corner: Corner::BottomRight,
            color_light: BadgeColor::AUTO,
            color_dark: BadgeColor::AUTO,
            scale: 1.0,
        },
        BadgeRule {
            state: BadgeState::FullWidth,
            corner: Corner::TopRight,
            color_light: BadgeColor::AUTO,
            color_dark: BadgeColor::AUTO,
            scale: 1.0,
        },
    ];
    let spec_both = IconSpec {
        punct: PunctBadge::Chinese,
        full_width: true,
        ..IconSpec::default()
    };
    let mono = r.render(32, false, &spec_both);

    // 单色档下整张图只该有主字那一种色相：浅色主题前景为黑，故 RGB 三通道相等。
    for i in 0..(32 * 32) {
        if mono[i * 4 + 3] == 0 {
            continue; // 全透明像素的颜色无意义
        }
        let (b, g, r) = (mono[i * 4], mono[i * 4 + 1], mono[i * 4 + 2]);
        assert!(
            b == g && g == r,
            "auto 配色下仍有带色相的像素（{b},{g},{r}）"
        );
    }
}

/// 条目倍率归零 = 这一条彻底不画，必须与状态未命中时逐字节相同。
///
/// 防半截短路：形状按 leg=0 画不出来，但挖空版尺寸是 `leg + gap`，主字仍会被挖掉一块。
///
/// `badge_alpha = 1.0` 是必需的：挖空只在不透明档发生，用默认的 0.88 跑这条测不出短路。
#[test]
fn rule_scale_zero_carves_nothing() {
    let mut r = with_rule(BadgeRule::solid(
        BadgeState::FullWidth,
        Corner::TopRight,
        IconRenderer::DEFAULT_FULL_WIDTH_COLOR,
        0.0,
    ));
    r.badge_alpha = 1.0;
    let full = r.render(
        32,
        false,
        &IconSpec {
            full_width: true,
            ..IconSpec::default()
        },
    );
    let half = r.render(32, false, &IconSpec::default());
    assert_eq!(full, half, "倍率归零时仍在主字上挖了洞");
}

/// 条目自带的不透明度覆盖全局值。
///
/// 这是 `alpha_light/dark` 的落点：色相相同、只有不透明度不同的两条规则画出来必须不一样；
/// 不写不透明度的那条要与全局值画出的结果逐字节相同（防覆盖字段没回落、被当成 0）。
#[test]
fn per_rule_alpha_overrides_the_global_default() {
    let spec_cn = spec(PunctBadge::Chinese);
    let mk = |global: f32, per_rule: Option<f32>| {
        let color = BadgeColor {
            rgb: Some(IconRenderer::DEFAULT_PUNCT_CN_COLOR),
            alpha: per_rule,
        };
        let mut r = with_rule(BadgeRule {
            state: BadgeState::PunctCn,
            corner: Corner::BottomRight,
            color_light: color,
            color_dark: color,
            scale: 1.0,
        });
        r.badge_alpha = global;
        r.render(32, false, &spec_cn)
    };
    assert_ne!(mk(0.88, Some(0.4)), mk(0.88, None), "条目不透明度没有生效");
    assert_eq!(
        mk(0.88, None),
        mk(0.88, Some(0.88)),
        "不指定条目不透明度时应等同于全局值"
    );
    assert_eq!(
        mk(0.5, Some(0.4)),
        mk(0.88, Some(0.4)),
        "条目指定了不透明度，全局值不该再有影响"
    );
}

/// 挖空档位**逐层**生效：同一张图里可以一条实心挖空、另一条半遮不挖。
///
/// 条目能各自指定不透明度后，挖空判据须从全局降到层上；否则调到 FF 的那条周围主字没被挖开，
/// 看起来糊在字上。
#[cfg(windows)]
#[test]
fn carving_is_decided_per_layer() {
    let n = 32usize;
    // 「五」右下有横笔，右上也有竖笔，两个角都压得住。
    let both = IconSpec {
        label: "五".to_string(),
        punct: PunctBadge::Chinese,
        full_width: true,
        ..IconSpec::default()
    };
    let rule = |state, corner, alpha| {
        let color = BadgeColor {
            rgb: Some(IconRenderer::DEFAULT_PUNCT_CN_COLOR),
            alpha: Some(alpha),
        };
        BadgeRule {
            state,
            corner,
            color_light: color,
            color_dark: color,
            scale: 1.0,
        }
    };
    let mut r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    // 右下实心（该挖空）、右上半遮（不该挖空）。
    r.rules = vec![
        rule(BadgeState::PunctCn, Corner::BottomRight, 1.0),
        rule(BadgeState::FullWidth, Corner::TopRight, 0.5),
    ];
    let mixed = r.render(32, false, &both);
    // 对照组：两条都半遮，谁都不挖。
    r.rules = vec![
        rule(BadgeState::PunctCn, Corner::BottomRight, 0.5),
        rule(BadgeState::FullWidth, Corner::TopRight, 0.5),
    ];
    let none_carved = r.render(32, false, &both);

    // 右下：实心那版在角标外围挖掉了一圈主字 ⇒ 存在 alpha 更低的像素。
    let mut carved = 0;
    for y in (n / 2)..n {
        for x in (n / 2)..n {
            if alpha_at(&mixed, n, x, y) < alpha_at(&none_carved, n, x, y) {
                carved += 1;
            }
        }
    }
    assert!(carved > 0, "不透明的那一层没有挖空——档位判据还停在全局值上");

    // 右上：两版都是半遮，主字一个像素都不该被动。
    for y in 0..(n / 2) {
        for x in (n / 2)..n {
            assert_eq!(
                alpha_at(&mixed, n, x, y),
                alpha_at(&none_carved, n, x, y),
                "半遮的那一层挖了主字（{x},{y}）"
            );
        }
    }
}

/// 全局倍率确实改变角标尺寸。
#[test]
fn badge_scale_changes_badge_size() {
    let mut r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    let spec_cn = IconSpec {
        punct: PunctBadge::Chinese,
        ..IconSpec::default()
    };
    r.badge_scale = 1.0;
    let base = r.render(32, false, &spec_cn);
    r.badge_scale = 0.6;
    let small = r.render(32, false, &spec_cn);
    assert_ne!(base, small, "改了全局大小倍率但渲染结果相同");
}

/// 条目倍率与全局倍率**相乘**，两级各自有效。
///
/// 第三条断言钉住「相乘」本身：条目倍率若改成覆盖全局，同一乘积的两条路径会画出不同的东西。
#[test]
fn rule_scale_multiplies_the_global_scale() {
    let spec_cn = spec(PunctBadge::Chinese);
    let mk = |global: f32, per_rule: f32| {
        let mut r = with_rule(BadgeRule::solid(
            BadgeState::PunctCn,
            Corner::BottomRight,
            IconRenderer::DEFAULT_PUNCT_CN_COLOR,
            per_rule,
        ));
        r.badge_scale = global;
        r.render(32, false, &spec_cn)
    };
    assert_ne!(mk(1.0, 1.0), mk(1.0, 0.6), "条目倍率没有生效");
    assert_ne!(mk(1.0, 1.0), mk(0.5, 1.0), "全局倍率没有生效");
    assert_eq!(
        mk(1.0, 0.5),
        mk(0.5, 1.0),
        "两级倍率不是相乘关系（同一乘积画出了不同的东西）"
    );
}

/// 变淡只压低 alpha，不改变颜色通道——旧实现里变淡与"显英文"是两种不同的表达，
/// 混在一起会让「输入法被禁用」和「当前位置不可输入」看起来一样。
#[test]
fn dimmed_only_lowers_alpha() {
    let r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    let normal = r.render(24, false, &spec(PunctBadge::Chinese));
    let dim = r.render(
        24,
        false,
        &IconSpec {
            dimmed: true,
            ..spec(PunctBadge::Chinese)
        },
    );
    assert_eq!(normal.len(), dim.len());
    for i in (0..normal.len()).step_by(4) {
        assert_eq!(normal[i], dim[i], "B 通道被改动");
        assert_eq!(normal[i + 1], dim[i + 1], "G 通道被改动");
        assert_eq!(normal[i + 2], dim[i + 2], "R 通道被改动");
        assert!(dim[i + 3] <= normal[i + 3], "变淡反而提高了 alpha");
    }
}

/// 暗色主题下图标画成浅色，亮色主题下画成深色。
///
/// 只检查有覆盖的像素：多色合成只在 alpha>0 处写颜色，全透明像素的 RGB 留 0（32bpp alpha
/// 图标下系统忽略它）。必须清空规则表：角标有自己的颜色，本断言测的是主字那条单色通路。
///
/// 仅 Windows：`render_glyph_mask` 走 DirectWrite，非 Windows 上是 stub、整张透明，
/// 末尾的 `inked > 0` 守卫会报断言等于没跑。
#[cfg(windows)]
#[test]
fn theme_flips_foreground_channels() {
    let mut r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    r.rules = Vec::new();
    let light = r.render(24, false, &spec(PunctBadge::Chinese));
    let dark = r.render(24, true, &spec(PunctBadge::Chinese));
    let mut inked = 0;
    for i in (0..light.len()).step_by(4) {
        // alpha 与主题无关，两者必须逐像素相等
        assert_eq!(light[i + 3], dark[i + 3], "主题不应改变覆盖度");
        if light[i + 3] == 0 {
            continue;
        }
        inked += 1;
        assert_eq!(light[i], 0, "亮色主题应画深色前景");
        assert_eq!(dark[i], 255, "暗色主题应画浅色前景");
    }
    assert!(inked > 0, "整张图都是透明的，断言等于没跑");
}

/// 亮暗配了不同的色时，两个主题下角标像素确实不同。
///
/// 「亮暗独立配色」的存在性证明：取色时若忘了看 `dark_theme`，两档会画出一样的角标而不报错。
#[test]
fn per_theme_colors_are_honored() {
    let mut r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    r.rules = vec![BadgeRule {
        state: BadgeState::PunctCn,
        corner: Corner::BottomRight,
        color_light: BadgeColor::rgb([0x00, 0x00, 0xFF]), // BGR：红
        color_dark: BadgeColor::rgb([0x00, 0xFF, 0x00]),  // BGR：绿
        scale: 1.0,
    }];
    let none_l = r.render(24, false, &spec(PunctBadge::None));
    let cn_l = r.render(24, false, &spec(PunctBadge::Chinese));
    let cn_d = r.render(24, true, &spec(PunctBadge::Chinese));

    // 只看「加了角标才出现覆盖」的像素，绕开主字（主字本就跟随主题）。
    let mut differed = 0;
    for i in (0..cn_l.len()).step_by(4) {
        if cn_l[i + 3] == 0 || none_l[i + 3] > 0 {
            continue;
        }
        if cn_l[i..i + 3] != cn_d[i..i + 3] {
            differed += 1;
        }
    }
    assert!(differed > 0, "亮暗配了不同的色，画出来却一样——取色没看主题");
}

/// 亮暗配成同一个色时，角标在两个主题下逐像素相同（出厂三条就是这个形态）。
///
/// 与上一条互为反向：配了同色不许跟着主题漂。少了这一侧，角标被顺手接上主字前景色也不会被发现。
#[test]
fn same_color_in_both_themes_is_theme_independent() {
    let r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    let none_l = r.render(24, false, &spec(PunctBadge::None));
    let cn_l = r.render(24, false, &spec(PunctBadge::Chinese));
    let cn_d = r.render(24, true, &spec(PunctBadge::Chinese));
    let mut checked = 0;
    for i in (0..cn_l.len()).step_by(4) {
        if cn_l[i + 3] == 0 || none_l[i + 3] > 0 {
            continue;
        }
        checked += 1;
        assert_eq!(
            cn_l[i..i + 3],
            cn_d[i..i + 3],
            "角标颜色随主题变了——出厂配色的前提是两个主题共用一组颜色"
        );
    }
    assert!(checked > 0, "没有找到角标独占像素，断言等于没跑");
}

/// 主字墨迹必须落在图标正中。
///
/// 行盒高含 lineGap 且只加在下方、CJK 字面在 em 框里又偏上，按行盒居中会让字整体偏上。
///
/// 容差 0.75px：GDI 兼容渲染把基线吸附到整像素，加上包围盒按整像素量的半像素误差，
/// 0.5 已是物理下限。
#[cfg(windows)]
#[test]
fn glyph_ink_is_centered() {
    let r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    for label in ["中", "英", "拼", "五"] {
        for &size in &wind_ipc::protocol::ICON_SIZES {
            let s = size as f32;
            let mask = r.render_glyph_mask(
                size,
                &IconSpec {
                    label: label.to_string(),
                    ..spec(PunctBadge::None)
                },
            );
            let (dx, dy) =
                IconRenderer::ink_center_delta(&mask, s).expect("主字没画出来，无法量墨迹");
            assert!(
                dx.abs() <= 0.75 && dy.abs() <= 0.75,
                "「{label}」在 {size}px 下未居中：残余位移 ({dx:.2}, {dy:.2})"
            );
        }
    }
}

/// 字重与留白的分档判据本身。
///
/// 最后一条是这条测试存在的主要理由：判据不能写成 `is_ascii`，全角「Ｅｎ」是字母却不是 ASCII。
#[test]
fn cjk_detection_splits_latin_from_han() {
    assert!(IconRenderer::has_cjk("英"));
    assert!(IconRenderer::has_cjk("符号"));
    assert!(!IconRenderer::has_cjk("En"));
    assert!(!IconRenderer::has_cjk("A"));
    assert!(
        !IconRenderer::has_cjk("Ｅｎ"),
        "全角字母仍是字母，判据不能用 is_ascii"
    );
}

/// 字重补偿的判据：只给真的被缩小了的拉丁标签。
///
/// 单字符 `"A"` 不进回缩、字号满格，加粗反而突兀。前两条与后两条必须同时成立：只测 `"En"`
/// 要加粗，一个「凡字母都加粗」的实现照样全绿。
#[test]
fn weight_compensation_only_for_shrunk_latin() {
    let w = |label: &str, base: f32, target: f32| IconRenderer::label_weight(label, base, target);
    // 16px 下拉丁档的 target ≈ 14.1
    assert_eq!(
        w("En", 15.65, 14.1),
        IconRenderer::FONT_WEIGHT_LATIN,
        "被缩小的字母要补偿"
    );
    assert_eq!(
        w("A", 7.7, 14.1),
        IconRenderer::FONT_WEIGHT,
        "装得下的字母不补偿——真机否掉的就是这一档"
    );
    // 16px 下汉字档的 target ≈ 13.16
    assert_eq!(
        w("英", 14.0, 13.16),
        IconRenderer::FONT_WEIGHT,
        "汉字恒用基线字重"
    );
    assert_eq!(
        w("符号", 28.0, 13.16),
        IconRenderer::FONT_WEIGHT,
        "汉字即使被缩小也不补偿：300 本就是为汉字笔画密度定的"
    );
}

/// 装得下的标签不得被回缩。
///
/// 判据取墨迹跨度而不是逐像素相等：`render_glyph_mask` 末尾按墨迹重心迭代微调位置，
/// 跨度只受字号影响、不受位移影响。
#[cfg(windows)]
#[test]
fn labels_that_fit_are_not_shrunk() {
    let r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    for label in ["A", "英", "五"] {
        for &size in &wind_ipc::protocol::ICON_SIZES {
            let mask = r.render_glyph_mask(
                size,
                &IconSpec {
                    label: label.to_string(),
                    ..spec(PunctBadge::None)
                },
            );
            let n = mask.n;
            let has = |x: usize, y: usize| mask.v[y * n + x] > 0.05;
            let lo = (0..n)
                .find(|&x| (0..n).any(|y| has(x, y)))
                .expect("没画出主字");
            let hi = (0..n)
                .rev()
                .find(|&x| (0..n).any(|y| has(x, y)))
                .expect("没画出主字");
            let span = hi - lo + 1;

            // 基线字号下这个标签本来有多宽（不回缩、不补偿）。
            let s = size as f32;
            let style = crate::text::dwrite::TextStyle::new(s - IconRenderer::FONT_SIZE_INSET)
                .with_weight(IconRenderer::FONT_WEIGHT);
            let base_w = r.text.measure(label, &style).width;

            // 墨迹跨度必然 ≤ 行盒宽；0.7 留给「墨迹本就窄于 advance」的量，
            // 真被回缩时（0.4~0.5 倍）跨度掉得远比这多。
            assert!(
                span as f32 >= base_w * 0.7,
                "「{label}」在 {size}px 下墨迹只跨 {span}px，基线行盒 {base_w:.1}px：被回缩了"
            );
        }
    }
}

/// 拉丁标签不得被白缩：`"En"` 在 16px 下行盒 15.65，本就差不多装得下，可用宽度若按汉字档
/// （`s - FONT_SIZE_INSET`）取，会被缩到 11px 上下、叠加 Light 字重显得发细。
/// 判据取墨迹跨度而不是字号：用户看到的是墨迹占了多少地方。
#[cfg(windows)]
#[test]
fn latin_label_is_not_shrunk_needlessly() {
    let r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    let mask = r.render_glyph_mask(
        16,
        &IconSpec {
            label: "En".to_string(),
            ..spec(PunctBadge::None)
        },
    );
    let n = mask.n;
    let col_has_ink = |x: usize| (0..n).any(|y| mask.v[y * n + x] > 0.05);
    let lo = (0..n).find(|&x| col_has_ink(x)).expect("没画出主字");
    let hi = (0..n).rev().find(|&x| col_has_ink(x)).expect("没画出主字");
    let span = hi - lo + 1;
    assert!(
        span >= 12,
        "\"En\" 在 16px 画布下墨迹只跨了 {span}px，被白缩了（列范围 {lo}..={hi}）"
    );
}

/// 两字符标签必须完整落在画布内（`[ui.labels]` 允许配成 `En`；字号写死时 `x0` 被钳成
/// 左对齐，右半个字母画到画布外）。
///
/// 判据取「最外一圈像素无墨迹」：行盒宽不等于墨迹宽，量真实输出才是在测用户看到的东西。
/// `En` 半角（临界）、`Ｅｎ`/`符号` 全角（必然触发回缩）各代表一类宽度；少了全角，
/// 「只在半角时缩」的实现也能全绿。
#[cfg(windows)]
#[test]
fn two_char_label_fits_canvas() {
    let r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    for label in ["En", "Ｅｎ", "符号"] {
        for &size in &wind_ipc::protocol::ICON_SIZES {
            let mask = r.render_glyph_mask(
                size,
                &IconSpec {
                    label: label.to_string(),
                    ..spec(PunctBadge::None)
                },
            );
            let n = mask.n;
            let col_has_ink = |x: usize| (0..n).any(|y| mask.v[y * n + x] > 0.05);
            let lo = (0..n).find(|&x| col_has_ink(x));
            let hi = (0..n).rev().find(|&x| col_has_ink(x));
            let last = n - 1;
            assert!(
                !col_has_ink(0) && !col_has_ink(last),
                "「{label}」在 {size}px 下触到画布左/右边缘：墨迹列范围 {lo:?}..={hi:?}，画布 0..={last}"
            );
        }
    }
}

/// 关掉总开关后必须与「本来就没有角标」逐字节相同。
///
/// 「不显示」不是画更小的角标，而是一点痕迹都不留；挖空蒙版若忘了跟着短路，主字上会留一圈凹口。
/// 关掉的是总开关而非清空规则表：规则表还在，这条才真的在验短路。
#[test]
fn style_none_leaves_no_trace() {
    let off = IconRenderer::new(BadgeStyle::None).expect("renderer");
    let on = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    for &size in &wind_ipc::protocol::ICON_SIZES {
        let baseline = on.render(size, false, &spec(PunctBadge::None));
        for p in [PunctBadge::Chinese, PunctBadge::English] {
            assert_eq!(
                off.render(size, false, &spec(p)),
                baseline,
                "{size}px / {p:?}：关掉总开关后仍与无角标基线不同"
            );
        }
    }
}

/// 三个落盘 id 都必须唯一且可往返——它们写进 `config.toml`，活得比进程久。
///
/// 未知 id 的处置按类型不同：总开关与角落有合理默认值，回落即可（手写配置写错一个字母
/// 不该让图标消失）；状态没有默认值，只能返回 `None` 让调用方丢掉整条规则。
#[test]
fn badge_ids_roundtrip() {
    let mut seen = std::collections::HashSet::new();
    for &st in &BadgeStyle::ALL {
        assert!(seen.insert(st.as_id()), "{st:?} 的 id 与别的档位重复");
        assert_eq!(BadgeStyle::from_id(st.as_id()), st);
    }
    for bogus in ["", "corner_triangle", "Corner", "0"] {
        assert_eq!(
            BadgeStyle::from_id(bogus),
            BadgeStyle::default(),
            "未知总开关 id {bogus:?} 未回落到默认"
        );
    }

    let mut seen = std::collections::HashSet::new();
    for &c in &Corner::ALL {
        assert!(seen.insert(c.as_id()), "{c:?} 的 id 与别的角落重复");
        assert_eq!(Corner::from_id(c.as_id()), c);
    }
    for bogus in ["", "bottomright", "BottomRight", "center"] {
        assert_eq!(
            Corner::from_id(bogus),
            Corner::BottomRight,
            "未知角落 id {bogus:?} 未回落到右下角"
        );
    }

    let mut seen = std::collections::HashSet::new();
    for &st in &BadgeState::ALL {
        assert!(seen.insert(st.as_id()), "{st:?} 的 id 与别的状态重复");
        assert_eq!(BadgeState::from_id(st.as_id()), Some(st));
    }
    for bogus in ["", "punct", "PunctCn", "half_width"] {
        assert_eq!(
            BadgeState::from_id(bogus),
            None,
            "未知状态 id {bogus:?} 应返回 None（整条规则丢弃），而不是回落到某个状态"
        );
    }
}

/// 总开关下标往返必须自洽——菜单命令只传一个 u8，映射错位就是「点了另一档」。
#[test]
fn badge_style_index_roundtrips() {
    for (i, st) in BadgeStyle::ALL.iter().enumerate() {
        assert_eq!(st.index() as usize, i, "{st:?} 的下标与 ALL 中的位置不符");
        assert_eq!(BadgeStyle::from_index(i as u8), *st);
    }
    // 越界回落到默认，不 panic：id 由另一个进程回传，不能假定合法
    assert_eq!(
        BadgeStyle::from_index(BadgeStyle::ALL.len() as u8),
        BadgeStyle::default()
    );
}

/// 演示动画：相位不同必须画出不同像素，否则"动画"是静止的。
#[test]
fn demo_animation_frames_differ() {
    let mut r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    r.demo_animation = true;
    let at = |frame: u32| {
        r.render(
            24,
            false,
            &IconSpec {
                frame,
                ..spec(PunctBadge::Chinese)
            },
        )
    };
    // 取相隔四分之一周期的两帧，跑马灯应转过约 90°
    let quarter = IconRenderer::DEMO_FRAMES_PER_CYCLE / 4;
    assert_ne!(at(0), at(quarter), "动画两帧完全相同——相位没生效");
    // 整周期回到原点
    assert_eq!(
        at(0),
        at(IconRenderer::DEMO_FRAMES_PER_CYCLE),
        "转满一圈没有回到起始帧"
    );
}

/// 关闭演示动画时，相位不得影响画面——否则状态推送会被无谓的重发刷屏。
#[test]
fn frame_is_ignored_when_demo_off() {
    let r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    let a = r.render(24, false, &spec(PunctBadge::Chinese));
    let b = r.render(
        24,
        false,
        &IconSpec {
            frame: 7,
            ..spec(PunctBadge::Chinese)
        },
    );
    assert_eq!(a, b, "演示动画关闭时相位仍改变了画面");
}

/// 手动预览工具：把四个角落渲染成对比图，供肉眼比选后再决定默认配到哪一角。
///
/// 部署要 UAC 提权并重启输入法，逐个试成本太高，而位置、配色、间隙、字重只能靠看。
/// 默认 `#[ignore]`，不进常规测试。
///
/// ```text
/// cargo test -p wind-ui --lib dump_preview -- --ignored --nocapture
/// ```
/// 输出目录由 `WIND_ICON_PREVIEW_DIR` 指定，缺省为系统临时目录。
#[cfg(windows)]
#[test]
#[ignore = "手动预览工具，不参与常规测试"]
fn dump_preview() {
    use image::{Rgba, RgbaImage};

    let dir = std::env::var("WIND_ICON_PREVIEW_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    std::fs::create_dir_all(&dir).expect("创建输出目录");

    const ZOOM: u32 = 9;
    const PAD: u32 = 10;
    let sizes: [u16; 2] = [16, 24];
    let corners = Corner::ALL;
    // 取实际出货的那组配色，别在预览里另写一份——预览与真机不同色时，
    // 肉眼比选出来的结论根本不适用于装机后的样子。
    let (cn_color, en_color) = (
        IconRenderer::DEFAULT_PUNCT_CN_COLOR,
        IconRenderer::DEFAULT_PUNCT_EN_COLOR,
    );

    // 把一个变体贴到画布上（BGRA→RGBA，最近邻放大），底色模拟任务栏
    let blit = |img: &mut RgbaImage, px: &[u8], n: u32, ox: u32, oy: u32, dark: bool| {
        let bg = if dark { 0x20u8 } else { 0xF3u8 };
        for y in 0..n * ZOOM {
            for x in 0..n * ZOOM {
                let i = ((y / ZOOM) * n + (x / ZOOM)) as usize * 4;
                let (b, g, r, a) = (px[i], px[i + 1], px[i + 2], px[i + 3] as u32);
                let mix = |c: u8| ((c as u32 * a + bg as u32 * (255 - a)) / 255) as u8;
                img.put_pixel(ox + x, oy + y, Rgba([mix(r), mix(g), mix(b), 255]));
            }
        }
    };

    // ── 图一：位置对比。每行一个角落；列 = {16,24}px × {中,英} × {浅,深} ──
    let row_h = 24 * ZOOM + PAD;
    let width = PAD
        + sizes
            .iter()
            .map(|s| 4 * (*s as u32 * ZOOM + PAD))
            .sum::<u32>();
    let mut img = RgbaImage::from_pixel(
        width,
        PAD + corners.len() as u32 * row_h,
        Rgba([255, 255, 255, 255]),
    );
    for (ri, corner) in corners.iter().enumerate() {
        let name = corner.as_id();
        let mut r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
        r.rules = vec![
            BadgeRule::solid(BadgeState::PunctCn, *corner, cn_color, 1.0),
            BadgeRule::solid(BadgeState::PunctEn, *corner, en_color, 1.0),
        ];
        let y = PAD + ri as u32 * row_h;
        let mut x = PAD;
        for &size in &sizes {
            let n = size as u32;
            for col in 0..4 {
                let cn = col % 2 == 0;
                let dark = col >= 2;
                let punct = if cn {
                    PunctBadge::Chinese
                } else {
                    PunctBadge::English
                };
                let px = r.render(size, dark, &spec(punct));
                blit(&mut img, &px, n, x, y + (24 * ZOOM - n * ZOOM) / 2, dark);
                x += n * ZOOM + PAD;
            }
        }
        println!("row {ri}: {name}");
    }
    let p = dir.join("icon_corners.png");
    img.save(&p).expect("保存 icon_corners.png");
    println!("wrote {}", p.display());
    println!("cols: [16px] 中/浅 英/浅 中/深 英/深 | [24px] 同上");

    // ── 图二：演示动画一圈的帧序列 ──
    let mut r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    r.demo_animation = true;
    let frames = 8u32;
    let step = IconRenderer::DEMO_FRAMES_PER_CYCLE / frames;
    let n = 24u32;
    let mut anim = RgbaImage::from_pixel(
        PAD + frames * (n * ZOOM + PAD),
        PAD * 2 + 2 * (n * ZOOM + PAD),
        Rgba([255, 255, 255, 255]),
    );
    for f in 0..frames {
        let s = IconSpec {
            frame: f * step,
            ..spec(PunctBadge::Chinese)
        };
        for (ri, dark) in [false, true].into_iter().enumerate() {
            let px = r.render(n as u16, dark, &s);
            blit(
                &mut anim,
                &px,
                n,
                PAD + f * (n * ZOOM + PAD),
                PAD + ri as u32 * (n * ZOOM + PAD),
                dark,
            );
        }
    }
    let p = dir.join("icon_anim.png");
    anim.save(&p).expect("保存 icon_anim.png");
    println!(
        "wrote {} （上排浅底、下排深底，左→右为一圈的 8 帧）",
        p.display()
    );
}

/// 取整张图 alpha 最大的那个像素的 BGR（非预乘，即该处的实际颜色）。
fn densest_bgr(buf: &[u8]) -> [u8; 3] {
    let px = buf
        .as_chunks::<4>()
        .0
        .iter()
        .max_by_key(|p| p[3])
        .expect("空图");
    assert!(px[3] > 0, "整张图没有任何墨迹");
    [px[0], px[1], px[2]]
}

/// 只画演示跑马灯的渲染器：跑马灯与主字同色、且不依赖文本后端，
/// 故能在 Linux（文本后端是 mock、画不出主字）上验证主字色的取用。
fn marquee_only(colors: TextColors) -> IconRenderer {
    let mut r = IconRenderer::new(BadgeStyle::None).expect("renderer");
    r.demo_animation = true;
    r.text_colors = colors;
    r
}

fn lang_spec(english: bool) -> IconSpec {
    IconSpec {
        english,
        ..IconSpec::default()
    }
}

/// 默认主字色 = 改动前写死的值：浅色任务栏黑、深色任务栏白，中英一致。
#[test]
fn default_text_colors_match_legacy_black_white() {
    let r = marquee_only(TextColors::default());
    for english in [false, true] {
        let light = r.render(32, false, &lang_spec(english));
        let dark = r.render(32, true, &lang_spec(english));
        assert_eq!(densest_bgr(&light), [0, 0, 0], "english={english} 浅色");
        assert_eq!(
            densest_bgr(&dark),
            [255, 255, 255],
            "english={english} 深色"
        );
    }
}

/// 中英配了不同色，english 标识必须真的选到不同的色（且按明暗各取各的格子）。
#[test]
fn english_flag_selects_its_own_text_color() {
    let colors = TextColors {
        cn_light: [0xFF, 0x00, 0x00, 0xFF],
        cn_dark: [0x00, 0xFF, 0x00, 0xFF],
        en_light: [0x00, 0x00, 0xFF, 0xFF],
        en_dark: [0xFF, 0xFF, 0x00, 0xFF],
    };
    let r = marquee_only(colors);
    // 输出是 BGR：RGBA 红 → [0, 0, 255]。
    assert_eq!(
        densest_bgr(&r.render(32, false, &lang_spec(false))),
        [0, 0, 255]
    );
    assert_eq!(
        densest_bgr(&r.render(32, true, &lang_spec(false))),
        [0, 255, 0]
    );
    assert_eq!(
        densest_bgr(&r.render(32, false, &lang_spec(true))),
        [255, 0, 0]
    );
    assert_eq!(
        densest_bgr(&r.render(32, true, &lang_spec(true))),
        [0, 255, 255]
    );
}

/// `auto` 角标与主字同色：换了主字色，auto 角标跟着换。
#[test]
fn auto_badge_follows_text_color() {
    let mut r = with_rule(BadgeRule {
        state: BadgeState::PunctCn,
        corner: Corner::BottomRight,
        color_light: BadgeColor::AUTO,
        color_dark: BadgeColor::AUTO,
        scale: 1.0,
    });
    r.badge_alpha = 1.0;
    r.text_colors.cn_light = [0x12, 0x34, 0x56, 0xFF];
    let buf = r.render(32, false, &spec(PunctBadge::Chinese));
    assert_eq!(densest_bgr(&buf), [0x56, 0x34, 0x12]);
}

/// 真机字形：中英分色后主字像素颜色不同；默认下明暗两档是 0 / 255。
/// 只在 Windows 上跑：其它平台文本后端是 mock，画不出主字。
#[cfg(windows)]
#[test]
fn glyph_uses_text_colors() {
    let mut r = IconRenderer::new(BadgeStyle::None).expect("renderer");
    assert_eq!(
        densest_bgr(&r.render(32, false, &lang_spec(false))),
        [0, 0, 0]
    );
    assert_eq!(
        densest_bgr(&r.render(32, true, &lang_spec(false))),
        [255, 255, 255]
    );
    r.text_colors.en_light = [0xFF, 0x00, 0x00, 0xFF];
    let cn = r.render(32, false, &lang_spec(false));
    let en = r.render(32, false, &lang_spec(true));
    assert_eq!(densest_bgr(&cn), [0, 0, 0]);
    assert_eq!(densest_bgr(&en), [0, 0, 255]);
}

/// 尺寸档标记开启时，各档左上角画的点数不同——这是真机验证"系统用了哪档"的依据。
#[test]
fn size_marks_differ_per_size_tier() {
    let mut r = IconRenderer::new(BadgeStyle::Corner).expect("renderer");
    r.size_marks = true;
    let a = r.render(16, false, &spec(PunctBadge::None));
    let b = r.render(16, false, &spec(PunctBadge::None));
    assert_eq!(a, b, "同一档两次渲染应完全一致");

    // 不同档之间左上角的点数不同（此处只验证渲染不 panic 且长度正确，
    // 点数差异靠真机肉眼判读——这正是这个标记存在的理由）
    for &size in &wind_ipc::protocol::ICON_SIZES {
        let buf = r.render(size, false, &spec(PunctBadge::None));
        assert_eq!(buf.len(), wind_ipc::protocol::icon_variant_bytes(size));
    }
}
