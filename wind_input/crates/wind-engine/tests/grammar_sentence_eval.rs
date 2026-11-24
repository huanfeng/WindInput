//! 语法模型（bigram）的整句评测。
//!
//! 不能用 `pinyin_eval` 验收 bigram：它的 A/B/C 三类都是单个词的测试，不存在跨词转移；
//! 唯一涉及多词的 D 类是随机拼两个词造出来的，不是自然语言。要看 bigram 有没有用，
//! 必须拿真实的多词句子测。
//!
//! ## 用法
//!
//! ```text
//! cargo test -p wind-engine --test grammar_sentence_eval -- --ignored --nocapture
//! ```
//!
//! 默认对比 `weight=0`（基线）与 `weight=1.0`，可用 `WIND_GRAM_WEIGHT` 改后者。
//! 需要 `build_dev/data` 与 `build_dev/data/schemas/pinyin/grammar/*.gram`。

use std::path::{Path, PathBuf};

use wind_config::Config;
use wind_engine::EngineManager;

const TOP_N: usize = 10;

/// 真实多词句子。期望值是人工判定的「自然中文」，不是从词库合成的。
///
/// 选材原则：要么在设计文档 §1 被点名，要么是日常高频表达（用户真会这么打、且上下文能帮上忙）。
const CASES: &[(&str, &str)] = &[
    // —— 设计文档 §1 表格点名的定点 ——
    ("sixiang", "思想"),
    ("nihao", "你好"),
    // —— 双词搭配：上下文应能定夺 ——
    ("qihoutezheng", "气候特征"),
    ("zhengquangongsi", "证券公司"),
    ("xinlishang", "心理上"),
    ("dulizizhu", "独立自主"),
    ("jianyixiugai", "建议修改"),
    // —— 短句 ——
    ("woshizhongguoren", "我是中国人"),
    ("jintiantianqihenhao", "今天天气很好"),
    ("womenyiqiquchifan", "我们一起去吃饭"),
    ("xiexienidebangzhu", "谢谢你的帮助"),
    ("zhegewentihenzhongyao", "这个问题很重要"),
    ("qingwenxianzaijidian", "请问现在几点"),
    ("womenxuyaogengduoshijian", "我们需要更多时间"),
    ("tazhengzaikanshu", "他正在看书"),
    ("zhonghuarenmingongheguo", "中华人民共和国"),
    ("jisuanjikexue", "计算机科学"),
    ("rengongzhineng", "人工智能"),
    ("gongzuobaogao", "工作报告"),
    ("chifanlema", "吃饭了吗"),
    ("zhendehenbucuo", "真的很不错"),
    // —— 同音消歧：正是 bigram 该拿分的地方，靠上下文才分得出 ——
    ("mingtianzaijian", "明天再见"),            // 再见 / 在建
    ("shenghuozhongdexiaoshi", "生活中的小事"), // 小事 / 小时
    ("woyaoqushangban", "我要去上班"),
    ("tadeyisijiushi", "他的意思就是"),
    ("zheshiyigehaobanfa", "这是一个好办法"),
    // —— 更多日常整句：扩大样本以便看出基线本来就答错的那些 ——
    ("wobuzhidaozenmeban", "我不知道怎么办"),
    ("nizuotianqunalile", "你昨天去哪里了"),
    ("womingtianyaokaihui", "我明天要开会"),
    ("zhebenshuhenyouyisi", "这本书很有意思"),
    ("wodepengyoulaile", "我的朋友来了"),
    ("yijingwanchengle", "已经完成了"),
    ("xuyaoduoshaoqian", "需要多少钱"),
    ("qingnigaosuwo", "请你告诉我"),
    ("tashuodehendui", "他说得很对"),
    ("zheyangzuobutaihao", "这样做不太好"),
    ("womenzaiyiqigongzuo", "我们在一起工作"),
    ("zhegeshihouyinggai", "这个时候应该"),
    ("kanwandianyingyihou", "看完电影以后"),
    ("zuotianwanshangxiayu", "昨天晚上下雨"),
    ("womendeshijianbugou", "我们的时间不够"),
    ("zhegeshijianhenzhongyao", "这个事件很重要"),
    ("gongsideguidingshi", "公司的规定是"),
    ("qingtijiaoshenqing", "请提交申请"),
    ("xiawuliangdiankaishi", "下午两点开始"),
    ("tazaijiaxiuxi", "他在家休息"),
    ("womenyinggaizenmezuo", "我们应该怎么做"),
    ("zhejianshiqingbunan", "这件事情不难"),
    ("nihaishiyaoxiaoxin", "你还是要小心"),
    ("dajiadouzhidaole", "大家都知道了"),
    // ═══ 以下为扩充样本 ═══
    //
    // 50 条太小：一条样本翻转就是 ±2%，与 bigram 的真实效应量同级；200 条后单条权重降到 0.5%。
    // 选材仍须是词库打得出的常用表达，生僻词放进来只会制造恒定的失败噪声。

    // —— 「的/得/地」：最高频的同音三分，且只能靠上下文定夺 ——
    ("nishuodeduibudui", "你说得对不对"),
    ("tazuodefeichanghao", "他做得非常好"),
    ("wodexiangfashi", "我的想法是"),
    ("manmandezoulai", "慢慢地走来"),
    ("gaoxingdetiaoqilai", "高兴地跳起来"),
    ("tapaodehenkuai", "他跑得很快"),
    ("zhegeshiwodedongxi", "这个是我的东西"),
    ("renzhendeting", "认真地听"),
    // —— 「在/再」：同音且都高频 ——
    ("womingtianzaishuo", "我明天再说"),
    ("tazhengzaigongzuo", "他正在工作"),
    ("zaijianwodepengyou", "再见我的朋友"),
    ("womenzaijiadengni", "我们在家等你"),
    ("zaishuoyibian", "再说一遍"),
    ("xianzaizainali", "现在在哪里"),
    // —— 「那/哪」：疑问与指示的同音对立 ——
    ("younaxieyaoqiu", "有哪些要求"),
    ("nagerenshishui", "那个人是谁"),
    ("naliyoumaide", "哪里有卖的"),
    ("nashiwodeshu", "那是我的书"),
    ("nizainalishangban", "你在哪里上班"),
    // —— 「事/是/时」：三向同音，整句里最常见的错解 ——
    ("zheshijianshiqing", "这是件事情"),
    ("zhegeshiqinghenji", "这个事情很急"),
    ("shijieshangzuikuai", "世界上最快"),
    ("dangshiwobuzhidao", "当时我不知道"),
    ("youshijiaowo", "有事叫我"),
    ("zhenshibuhaoyisi", "真是不好意思"),
    ("gongzuoshijianjieshu", "工作时间结束"),
    // —— 「他/她/它」与人称 ——
    ("tashiwodetongshi", "他是我的同事"),
    ("tamenyijingzoule", "他们已经走了"),
    ("womenlianggehaopengyou", "我们两个好朋友"),
    // —— 日常高频整句 ——
    ("qingshaodengyixia", "请稍等一下"),
    ("womashanghuilai", "我马上回来"),
    ("jintianwohenmang", "今天我很忙"),
    ("mingtianyoushijianma", "明天有时间吗"),
    ("nizhendetaihaole", "你真的太好了"),
    ("meishibiekeqi", "没事别客气"),
    ("zhenduibuqiwolaiwanle", "真对不起我来晚了"),
    ("ganxienindebangzhu", "感谢您的帮助"),
    ("qidainindehuifu", "期待您的回复"),
    ("zhuninshentijiankang", "祝您身体健康"),
    ("womenmingtianjian", "我们明天见"),
    ("haojiubujianle", "好久不见了"),
    ("zuijinzenmeyang", "最近怎么样"),
    ("wozaikaihui", "我在开会"),
    ("bushiwodecuo", "不是我的错"),
    ("zheyangkeyima", "这样可以吗"),
    ("dangranmeiwenti", "当然没问题"),
    ("wozhidaolexiexie", "我知道了谢谢"),
    // —— 工作 / 事务类整句 ——
    ("wodezhuyaogongzuo", "我的主要工作"),
    ("zheshiwodezeren", "这是我的责任"),
    ("tigaogongzuoxiaolv", "提高工作效率"),
    ("jiejuezhegewenti", "解决这个问题"),
    ("anpaiyixiashijian", "安排一下时间"),
    ("qingniquerenyixia", "请你确认一下"),
    ("womenxuyaotaolun", "我们需要讨论"),
    ("zhegefanganbucuo", "这个方案不错"),
    ("mingtianjiaobaogao", "明天交报告"),
    ("huiyituidaoxiazhou", "会议推到下周"),
    ("qingchakanfujian", "请查看附件"),
    ("yijingfageinile", "已经发给你了"),
    ("wohuijixuguanzhu", "我会继续关注"),
    ("zhegerenwuwanchengle", "这个任务完成了"),
    ("xuyaonideyijian", "需要你的意见"),
    // —— 长句（8 字以上，跨词转移最多）——
    ("womenxuyaoyigexindefangan", "我们需要一个新的方案"),
    ("zhegexiangmuyijingwanchengle", "这个项目已经完成了"),
    ("tamenmingtianhuilaikaihui", "他们明天回来开会"),
    ("zhejianshiqingxuyaoshijian", "这件事情需要时间"),
    ("nikeyigaosuwodianhuama", "你可以告诉我电话吗"),
    ("womendeyijianbutaiyizhi", "我们的意见不太一致"),
    ("zhegewentibijiaofuza", "这个问题比较复杂"),
    ("tashuotayaoqubeijing", "他说他要去北京"),
    ("womenzaigongyuanjianmian", "我们在公园见面"),
    ("jintianwanshangyiqichifan", "今天晚上一起吃饭"),
    ("mingtianzaoshangbadianchufa", "明天早上八点出发"),
    ("wojuedezheyangbutaihao", "我觉得这样不太好"),
    ("nishifoukeyibangwoyixia", "你是否可以帮我一下"),
    ("womenyinggaizaikaolvkaolv", "我们应该再考虑考虑"),
    ("zhebenshuwoyijingkanwanle", "这本书我已经看完了"),
    ("tadegongzuotaimangle", "他的工作太忙了"),
    ("womenyaohaohaoxuexi", "我们要好好学习"),
    ("nishuodehenyoudaoli", "你说得很有道理"),
    ("tamenyijingchufale", "他们已经出发了"),
    ("womendejihuayaogaibian", "我们的计划要改变"),
    ("qingdajiazhuyianquan", "请大家注意安全"),
    ("zhegedifangwolaiguo", "这个地方我来过"),
    ("wobuzhidaotazainali", "我不知道他在哪里"),
    ("mingtiantianqihuizenmeyang", "明天天气会怎么样"),
    ("womenxiazhouzaijianmian", "我们下周再见面"),
];

/// 扩充第二批：数量词、时间、固定搭配、易被字级模型打散的词组。
///
/// 与 [`CASES`] 分开只是为了可读，实际评测时两者合并（见 `all_cases`）。
const CASES_EXT: &[(&str, &str)] = &[
    // —— 数量词 / 时间：量词与数字最易被同音字冲掉 ——
    ("mingtianxiawusandian", "明天下午三点"),
    ("yigongsanbaikuaiqian", "一共三百块钱"),
    ("dengleyigexiaoshi", "等了一个小时"),
    ("zoulesanshifenzhong", "走了三十分钟"),
    ("maileliangjinpingguo", "买了两斤苹果"),
    ("huafeiliangtianshijian", "花费两天时间"),
    ("yigongyoushigeren", "一共有十个人"),
    ("diyicikanjian", "第一次看见"),
    ("liangnianqiandeshiqing", "两年前的事情"),
    ("sangeyuehou", "三个月后"),
    ("shangwujiudiankaishi", "上午九点开始"),
    ("wanshangbadianjieshu", "晚上八点结束"),
    ("yitianyici", "一天一次"),
    ("bannianyihoucaizhidao", "半年以后才知道"),
    ("jitianneihuifu", "几天内回复"),
    // —— 固定搭配 / 四字词：整体性强，最能看出模型会不会打散词组 ——
    ("shishiqiushi", "实事求是"),
    ("yirujiwang", "一如既往"),
    ("quanliyifu", "全力以赴"),
    ("renzhenfuze", "认真负责"),
    ("zonghekaolv", "综合考虑"),
    ("jiaqiangguanli", "加强管理"),
    ("tigaozhiliang", "提高质量"),
    ("jiejuefangshi", "解决方式"),
    ("chongfenzhunbei", "充分准备"),
    ("jijiyingdui", "积极应对"),
    ("baochilianxi", "保持联系"),
    ("zhuyianquan", "注意安全"),
    ("gongtongnuli", "共同努力"),
    ("xiangguanbumen", "相关部门"),
    ("jutitiaozheng", "具体调整"),
    // —— 易被字级模型打散的多字词组（bgc 的已知弱项）——
    ("shehuizhuyi", "社会主义"),
    ("jingjifazhan", "经济发展"),
    ("kejichuangxin", "科技创新"),
    ("jiaoyugaige", "教育改革"),
    ("huanjingbaohu", "环境保护"),
    ("yiliaobaozhang", "医疗保障"),
    ("wenhuachuancheng", "文化传承"),
    ("guojiazhengce", "国家政策"),
    ("chengshijianshe", "城市建设"),
    ("nongyeshengchan", "农业生产"),
    ("jinrongshichang", "金融市场"),
    ("qiyeguanli", "企业管理"),
    ("renlicaiyuan", "人力资源"),
    ("shujufenxi", "数据分析"),
    ("wangluoanquan", "网络安全"),
    // —— 口语 / 语气结尾 ——
    ("nikeyibangwokankanma", "你可以帮我看看吗"),
    ("zhegezenmeyongya", "这个怎么用呀"),
    ("womenzoubaxianzai", "我们走吧现在"),
    ("haodejiuzheyangba", "好的就这样吧"),
    ("nizhendequedingma", "你真的确定吗"),
    ("shibushizhende", "是不是真的"),
    ("keyishishima", "可以试试吗"),
    ("yaobuyaoyiqiqu", "要不要一起去"),
    ("zenmehuizheyang", "怎么会这样"),
    ("weishenmebugaosuwo", "为什么不告诉我"),
];

/// 两批用例合并后的全集。分成两个常量只是为了可读，评测一律走这里。
fn all_cases() -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    CASES.iter().chain(CASES_EXT.iter())
}

fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    p.join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
        .then_some(p)
}

fn manager(dir: &Path, weight: f64) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string()];
    cfg.schema.active = "pinyin".to_string();
    cfg.schema.pinyin.grammar.weight = weight;
    // 换模型：bgc（字级 2-gram）与 bgw（词级）行为差别很大，标定时要能一键切换。
    if let Ok(m) = std::env::var("WIND_GRAM_MODEL") {
        cfg.schema.pinyin.grammar.model = m;
    }
    EngineManager::new(&cfg, Some(dir))
}

#[test]
#[ignore = "整句评测：依赖 build_dev 真实词库与 .gram 模型。用 --ignored 显式运行"]
fn grammar_sentence_report() {
    let Some(dir) = data_dir() else {
        eprintln!("!!! 跳过：build_dev 拼音词库不存在");
        return;
    };
    let gram = dir.join("schemas/pinyin/grammar");
    if !gram.exists() {
        eprintln!(
            "!!! 跳过：找不到 {}。\n\
             !!! 获取：curl -L -o zh-hans-bgc.gram \
             https://github.com/lotem/rime-octagram-data/raw/hans/zh-hans-t-essay-bgc.gram",
            gram.display()
        );
        return;
    }

    let weight = std::env::var("WIND_GRAM_WEIGHT")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1.0);

    let base = manager(&dir, 0.0);
    let with = manager(&dir, weight);

    let (mut base_hit, mut with_hit) = (0usize, 0usize);
    let mut changed: Vec<(&str, &str, String, String)> = Vec::new();

    for &(input, expect) in all_cases() {
        let b = base.convert_with("pinyin", input, TOP_N).candidates;
        let w = with.convert_with("pinyin", input, TOP_N).candidates;
        let bt = b.first().map(|c| c.text.clone()).unwrap_or_default();
        let wt = w.first().map(|c| c.text.clone()).unwrap_or_default();
        if bt == expect {
            base_hit += 1;
        }
        if wt == expect {
            with_hit += 1;
        }
        if bt != wt {
            changed.push((input, expect, bt, wt));
        }
    }

    let n = all_cases().count();
    println!("\n=== 整句评测 (weight={weight}) ===");
    println!("样本 {n}");
    println!(
        "基线   top-1 命中 {base_hit}/{n} = {:.1}%",
        base_hit as f64 * 100.0 / n as f64
    );
    println!(
        "接模型 top-1 命中 {with_hit}/{n} = {:.1}%   ({:+})",
        with_hit as f64 * 100.0 / n as f64,
        with_hit as i64 - base_hit as i64
    );

    println!("\n--- 首选发生变化的样本 ({}) ---", changed.len());
    for (input, expect, bt, wt) in &changed {
        // 标注这次改动是修好了、弄坏了、还是两边都不对
        let tag = match (bt == expect, wt == expect) {
            (false, true) => "修好",
            (true, false) => "弄坏",
            _ => "都错",
        };
        println!("  [{tag}] {input:<26} 期望 {expect:<14} 基线 {bt:<14} 新 {wt}");
    }

    println!("\n--- 两边一致的样本里仍未命中的 ---");
    for &(input, expect) in all_cases() {
        let b = base.convert_with("pinyin", input, TOP_N).candidates;
        let bt = b.first().map(|c| c.text.clone()).unwrap_or_default();
        let w = with.convert_with("pinyin", input, TOP_N).candidates;
        let wt = w.first().map(|c| c.text.clone()).unwrap_or_default();
        if bt == wt && bt != expect {
            println!("  {input:<26} 期望 {expect:<14} 实得 {bt}");
        }
    }
}

/// 大样本整句评测：用例来自文件（`scripts/lm/sentence_eval_prep.py` 从 Tatoeba 生成，每行
/// `类别<TAB>拼音<TAB>期望`），不再靠手写——192 条手写用例上结论曾随抽样翻转（设计文档 §8.1）。
///
/// ```text
/// WIND_SENTENCE_EVAL=sentence_eval.tsv [WIND_GRAM_MODEL=… WIND_GRAM_WEIGHT=0.5] \
///   [WIND_SENTENCE_EVAL_DUMP=misses.tsv] \
///   cargo test --release -p wind-engine --test grammar_sentence_eval sentence_eval_file -- --ignored --nocapture
/// ```
///
/// 只设文件时只量基线（`weight = 0`，不需要模型）；设了 `WIND_GRAM_MODEL` 就并排量接模型的。
/// 位次按界面序（见 [`ui_candidates`]）。按类别出：首选整句命中、宽松命中（他/她/它不分——光看
/// 拼音本就分不出）、前三含整句、首选的字准确率（1 − 编辑距离 / 期望字数）、单次转换的平均与 p99 耗时。
/// `DUMP` 写出任一边首选不对的条目，供逐条看错在哪。
#[test]
#[ignore = "整句评测：依赖 build_dev 真实词库与评测文件。用 --ignored 显式运行"]
fn sentence_eval_file() {
    let Ok(file) = std::env::var("WIND_SENTENCE_EVAL") else {
        eprintln!("!!! 跳过：未设 WIND_SENTENCE_EVAL");
        return;
    };
    let Some(dir) = data_dir() else {
        eprintln!("!!! 跳过：build_dev 拼音词库不存在");
        return;
    };
    let text = std::fs::read_to_string(&file).expect("读评测文件");
    let cases: Vec<(&str, &str, &str)> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            Some((it.next()?, it.next()?, it.next()?))
        })
        .collect();

    let mut runs = vec![("基线".to_string(), manager(&dir, 0.0))];
    if std::env::var("WIND_GRAM_MODEL").is_ok() {
        let weight = std::env::var("WIND_GRAM_WEIGHT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.5);
        runs.push((format!("模型 w={weight}"), manager(&dir, weight)));
    }

    let mut classes: Vec<&str> = cases.iter().map(|c| c.0).collect();
    classes.sort_unstable();
    classes.dedup();

    // 每条用例在每个 run 下的首选
    let mut firsts: Vec<Vec<String>> = vec![Vec::new(); runs.len()];
    println!("\n类别\t配置\t条数\t首选整句\t宽松\t前三含\t字准确率\t平均 ms\tp99 ms");
    for (ri, (name, mgr)) in runs.iter().enumerate() {
        for class in &classes {
            let (mut n, mut top1, mut loose, mut top3) = (0, 0, 0, 0);
            let (mut chars, mut char_ok) = (0usize, 0.0f64);
            let mut times = Vec::new();
            for &(_, input, expect) in cases.iter().filter(|c| c.0 == *class) {
                let t = std::time::Instant::now();
                let cands = ui_candidates(mgr, input);
                times.push(t.elapsed().as_secs_f64() * 1000.0);
                let first = cands.first().map(|c| c.text.clone()).unwrap_or_default();
                n += 1;
                top1 += usize::from(first == expect);
                loose += usize::from(loosen(&first) == loosen(expect));
                top3 += usize::from(cands.iter().take(3).any(|c| c.text == expect));
                let len = expect.chars().count();
                chars += len;
                char_ok += len.saturating_sub(edit_distance(&first, expect)) as f64;
                firsts[ri].push(first);
            }
            times.sort_by(f64::total_cmp);
            let pct = |k: usize| k as f64 * 100.0 / n.max(1) as f64;
            println!(
                "{class}\t{name}\t{n}\t{:.1}%\t{:.1}%\t{:.1}%\t{:.1}%\t{:.2}\t{:.2}",
                pct(top1),
                pct(loose),
                pct(top3),
                char_ok * 100.0 / chars.max(1) as f64,
                times.iter().sum::<f64>() / n.max(1) as f64,
                times[(times.len() * 99 / 100).min(times.len().saturating_sub(1))],
            );
        }
    }

    if let Ok(dump) = std::env::var("WIND_SENTENCE_EVAL_DUMP") {
        // firsts 按「类别 × 用例」的顺序填的，这里按同一顺序对回用例
        let ordered: Vec<&(&str, &str, &str)> = classes
            .iter()
            .flat_map(|cl| cases.iter().filter(move |c| c.0 == *cl))
            .collect();
        let mut out = String::from("类别\t拼音\t期望");
        for (name, _) in &runs {
            out.push('\t');
            out.push_str(name);
        }
        out.push('\n');
        for (i, (class, input, expect)) in ordered.iter().enumerate() {
            if firsts.iter().all(|f| f[i] == *expect) {
                continue;
            }
            out.push_str(&format!("{class}\t{input}\t{expect}"));
            for f in &firsts {
                out.push('\t');
                out.push_str(&f[i]);
            }
            out.push('\n');
        }
        std::fs::write(&dump, out).expect("写 DUMP");
        println!("未命中 → {dump}");
    }
}

/// 宽松比较用：第三人称代词归一成「他」。
fn loosen(s: &str) -> String {
    s.chars()
        .map(|c| {
            if matches!(c, '她' | '它' | '牠') {
                '他'
            } else {
                c
            }
        })
        .collect()
}

/// 界面序候选：与协调器同一个比较器排序、按文本去重（同 `pinyin_eval.rs::ui_candidates`）。
///
/// 引擎序不是用户看到的顺序：协调器以消费长度为首键整体重排，只吃掉前缀的整句在引擎序里能压住
/// 覆盖整串的混合整句（`tayizhizaixx` 下「他一直在学习」引擎序第 10、界面第 1）。
fn ui_candidates(mgr: &EngineManager, input: &str) -> Vec<wind_candidate::Candidate> {
    const UI_LIMIT: usize = 300;
    let mut c = mgr.convert_with("pinyin", input, UI_LIMIT).candidates;
    c.sort_by(|a, b| wind_candidate::candidate_display_order(a, b, false, false, input));
    let mut seen = std::collections::HashSet::new();
    c.retain(|x| seen.insert(x.text.clone()));
    c
}

/// 按字的编辑距离（插入、删除、替换各算 1）。
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// 逐条看候选：`WIND_PROBE=tayizhizaixx,wxqhuabingchang` 打印每条的前 `TOP_N` 名
/// （文本、编码、消费长度）。设了 `WIND_GRAM_MODEL` 就带模型（权重取 `WIND_GRAM_WEIGHT`，默认 0.5）。
#[test]
#[ignore = "诊断工具：依赖 build_dev 真实词库。用 --ignored 显式运行"]
fn sentence_probe() {
    let Ok(inputs) = std::env::var("WIND_PROBE") else {
        eprintln!("!!! 跳过：未设 WIND_PROBE");
        return;
    };
    let Some(dir) = data_dir() else {
        eprintln!("!!! 跳过：build_dev 拼音词库不存在");
        return;
    };
    let weight = if std::env::var("WIND_GRAM_MODEL").is_ok() {
        std::env::var("WIND_GRAM_WEIGHT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.5)
    } else {
        0.0
    };
    let mgr = manager(&dir, weight);
    for input in inputs.split(',').filter(|s| !s.is_empty()) {
        println!("== {input}");
        for c in ui_candidates(&mgr, input).into_iter().take(TOP_N) {
            println!("  {}\t{}\t消费 {}", c.text, c.code, c.consumed_length);
        }
    }
}
