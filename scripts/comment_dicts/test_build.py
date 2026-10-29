"""build.py / ai_fill.py 的测试。无 pytest 依赖：`python3 -I -m unittest discover -s scripts/comment_dicts`，
装了 pytest 也能直接跑。"""

import contextlib
import gzip
import io
import importlib.util
import os
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, name + ".py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


build = load("build")
ai_fill = load("ai_fill")

ECDICT_CSV = (
    "word,phonetic,definition,translation,pos,collins,oxford,tag,bnc,frq,exchange,detail,audio\n"
    'abandon,x,,"vt. 放弃, 抛弃, 遗弃, 使屈从\\nn. 放任, 无拘束",,,,,0,0,,,\n'
    "Aaron,x,,[网络] 亚伦,,,,,0,0,,,\n"
    "zzz,x,,n. 不在词表,,,,,0,0,,,\n"
)
EN_DICT = "---\nname: en\n...\n# disabled\nabandon\tabandon\naaron\taaron\nnomatch\tnomatch\n"
ZH_DICT = "---\nname: x\ncolumns:\n  - code\n  - text\n  - weight\n...\na\t工\t9\nb\t你好\t1\nc\t生僻词\t1\nd\tabc\t1\n"
CEDICT = (
    "#! comment\n"
    "工 工 [gong1] /work/worker/skill/CL:個|个[ge4]/\n"
    "你好 你好 [ni3 hao3] /hello/hi/how are you?/\n"
    "旧 舊 [jiu4] /variant of 舊|旧/\n"
)


def write(d, name, text):
    p = os.path.join(d, name)
    with open(p, "w", encoding="utf-8") as f:
        f.write(text)
    return p


class BuildTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.d = self.tmp.name

    def test_en_zh_keeps_first_pos_two_senses(self):
        ec = write(self.d, "ecdict.csv", ECDICT_CSV)
        en = write(self.d, "en.dict.yaml", EN_DICT)
        entries, words = build.build_en_zh(ec, [en])
        self.assertEqual(entries["abandon"], "vt. 放弃；抛弃")
        self.assertEqual(entries["aaron"], "亚伦")  # 忽略大小写命中、无词性
        self.assertNotIn("nomatch", entries)
        self.assertNotIn("zzz", entries)
        self.assertEqual(words, ["abandon", "aaron", "nomatch"])

    def test_zh_en_cedict_wins_over_ai(self):
        cedict = build.load_cedict(write(self.d, "c.txt", CEDICT))
        ai = {"工": "n. AI 的义项", "生僻词": "n. rare word"}
        entries = build.build_zh_en(cedict, ai, ["工", "你好", "生僻词"])
        self.assertEqual(entries["工"], "work; worker")  # CEDICT 优先、跳过 CL
        self.assertEqual(entries["你好"], "hello; hi")
        self.assertEqual(entries["生僻词"], "n. rare word")

    def test_cedict_variant_only_entry_is_uncovered(self):
        cedict = build.load_cedict(write(self.d, "c.txt", CEDICT))
        self.assertNotIn("旧", cedict)

    def test_cedict_header_lines_become_comments(self):
        self.assertEqual(
            build.cedict_header(write(self.d, "c.txt", "#! date=2026-10-21\n#! time=1\n工 工 [gong1] /work/\n")),
            "# CC-CEDICT date=2026-10-21\n# CC-CEDICT time=1\n",
        )

    def test_cedict_gz(self):
        p = os.path.join(self.d, "c.txt.gz")
        with gzip.open(p, "wt", encoding="utf-8") as f:
            f.write(CEDICT)
        self.assertEqual(build.load_cedict(p)["你好"], "hello; hi")

    def test_missing_lists_uncovered_only(self):
        zh = write(self.d, "zh.dict.yaml", ZH_DICT)
        words = build.zh_words([zh])
        self.assertEqual(words, ["工", "你好", "生僻词"])  # 非 CJK 的 abc 不在内
        cedict = build.load_cedict(write(self.d, "c.txt", CEDICT))
        self.assertEqual(build.missing_words(cedict, {}, words), ["生僻词"])
        self.assertEqual(build.missing_words(cedict, {"生僻词": "x"}, words), [])

    def test_ai_tsv_blank_rows_skipped(self):
        ai = write(self.d, "ai.tsv", "# 来源\n词甲\tn. a\n词乙\t\n词丙\n\n词丁\tv. d\n")
        self.assertEqual(build.load_ai(ai), {"词甲": "n. a", "词丁": "v. d"})
        self.assertEqual(build.load_ai(os.path.join(self.d, "nope.tsv")), {})

    def test_blank_ai_row_still_missing(self):
        words = ["生僻词"]
        self.assertEqual(build.missing_words({}, build.load_ai(write(self.d, "ai.tsv", "生僻词\t\n")), words), words)


def rime(rows, columns=None):
    head = "---\nname: t\n"
    if columns:
        head += "columns:\n" + "".join(f"  - {c}\n" for c in columns)
    return head + "...\n" + "".join("\t".join(map(str, r)) + "\n" for r in rows)


class SelectionTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.d = self.tmp.name
        # 通用词库不写 columns：默认 text/code/weight
        self.base = write(self.d, "base.dict.yaml", rime([
            ("低频词", "a", 99), ("边界词", "b", 100), ("高频词", "c", 5000),
            ("已覆盖", "d", 9000), ("单", "e", 9000), ("abc", "f", 9000),
            ("重复词", "g", 300), ("重复词", "g2", 800), ("跨库词", "h", 400),
        ]))
        self.ext = write(self.d, "ext.dict.yaml", rime([("跨库词", "h", 900), ("扩展词", "i", 150)]))
        self.cat = write(self.d, "name.dict.yaml", rime([("专名甲", "j", 99999), ("专名乙", "k", 99999)]))
        self.wubi = write(self.d, "wubi.dict.yaml", rime([
            ("aa", "五笔甲", 50), ("ab", "五笔乙", 70), ("ac", "五笔丙", 60), ("ad", "高频词", 99999),
            ("ae", "专名甲", 99999), ("af", "已覆盖", 99999), ("ag", "字", 99999), ("ah", "跨库词", 99999),
        ], ["code", "text", "weight"]))
        self.cedict = {"已覆盖": "covered"}

    def select(self, **kw):
        args = dict(cedict=self.cedict, ai={}, general=[self.base, self.ext], wubi=[self.wubi], category=[self.cat])
        args.update(kw)
        return build.select_missing(**args)

    def words(self, sel, group=None):
        return [w for g, w in sel if group is None or g == group]

    def test_frequency_threshold_is_inclusive_and_configurable(self):
        sel = self.select()
        self.assertIn("边界词", self.words(sel))
        self.assertNotIn("低频词", self.words(sel))
        self.assertIn("低频词", self.words(self.select(min_freq=50)))
        self.assertNotIn("边界词", self.words(self.select(min_freq=101)))

    def test_weight_of_repeated_word_is_the_max(self):
        self.assertIn("重复词", self.words(self.select(min_freq=800)))

    def test_covered_single_char_and_non_cjk_excluded(self):
        sel = self.words(self.select(min_freq=0))
        for w in ("已覆盖", "单", "abc"):
            self.assertNotIn(w, sel)

    def test_category_dicts_never_supplemented_even_via_wubi(self):
        sel = self.words(self.select())
        self.assertNotIn("专名甲", sel)
        self.assertNotIn("专名乙", sel)

    def test_wubi_takes_top_n_by_weight_of_words_unknown_to_pinyin(self):
        sel = self.select(wubi_top=2)
        self.assertEqual(self.words(sel, "wubi"), ["五笔乙", "五笔丙"])
        self.assertEqual(self.words(self.select(), "wubi"), ["五笔乙", "五笔丙", "五笔甲"])

    def test_wubi_threshold_is_not_applied_to_wubi(self):
        self.assertIn("五笔甲", self.words(self.select(min_freq=10**6)))

    def test_cross_source_dedup_keeps_first_group(self):
        sel = self.select()
        self.assertEqual(len(self.words(sel)), len(set(self.words(sel))))
        self.assertEqual(self.words(sel, "base").count("跨库词"), 1)
        self.assertNotIn("跨库词", self.words(sel, "ext"))
        self.assertEqual(self.words(sel, "ext"), ["扩展词"])

    def test_ai_covered_words_not_selected_again(self):
        sel = self.words(self.select(ai={"高频词": "n. x"}))
        self.assertNotIn("高频词", sel)

    def test_plan_summary_counts_groups_and_batches(self):
        sel = self.select()
        text = "\n".join(build.plan_summary(sel, 2))
        self.assertIn("base", text)
        self.assertIn("wubi", text)
        n = len(sel)
        self.assertIn(f"{n}", text)
        self.assertIn(f"{(n + 1) // 2} 批", text)

    def test_plan_command_writes_word_list_and_prints_summary(self):
        cedict = write(self.d, "c.txt", "已覆盖 已覆盖 [x] /covered/\n")
        out = os.path.join(self.d, "plan.txt")
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            build.main(["plan", "--cedict", cedict, "--general", self.base, self.ext,
                        "--wubi", self.wubi, "--category", self.cat, "--out", out])
        with open(out, encoding="utf-8") as f:
            lines = f.read().split()
        self.assertEqual(lines, self.words(self.select()))
        self.assertIn("批", buf.getvalue())


class MergePriorityTest(unittest.TestCase):
    def test_zh_en_refined_beats_cedict_beats_ai(self):
        out = build.build_zh_en(
            {"甲": "raw-c", "乙": "raw-c2"}, {"甲": "ai", "乙": "ai", "丙": "ai"}, ["甲", "乙", "丙", "丁"],
            refined={"甲": "n. refined"},
        )
        self.assertEqual(out, {"甲": "n. refined", "乙": "raw-c2", "丙": "ai"})

    def test_zh_en_without_refined_unchanged(self):
        self.assertEqual(build.build_zh_en({"甲": "c"}, {"甲": "a"}, ["甲"]), {"甲": "c"})

    def test_en_zh_refined_overrides_raw(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        ec = write(d.name, "ecdict.csv", ECDICT_CSV)
        en = write(d.name, "en.dict.yaml", EN_DICT)
        entries, _ = build.build_en_zh(ec, [en], refined={"abandon": "v. 放弃", "nomatch": "n. 无"})
        self.assertEqual(entries["abandon"], "v. 放弃")
        self.assertEqual(entries["aaron"], "亚伦")
        self.assertEqual(entries["nomatch"], "n. 无")

    def test_zh_en_command_uses_refined_file(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        zh = write(d.name, "zh.dict.yaml", ZH_DICT)
        cedict = write(d.name, "c.txt", CEDICT)
        ref = write(d.name, "ref.tsv", "# 来源\n工\tn. work\n")
        out = os.path.join(d.name, "zh_en.txt")
        build.main(["zh-en", "--cedict", cedict, "--refined", ref, "--words", zh, "--out", out])
        with open(out, encoding="utf-8") as f:
            text = f.read()
        self.assertIn("工\tn. work\n", text)
        self.assertIn("你好\thello; hi\n", text)


class SnapshotTest(unittest.TestCase):
    """入库的原始释义快照（snapshot → --raw）与直接读上游，产物须逐字节相同。"""

    def test_raw_snapshot_reproduces_upstream_build(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        # 上游释义里本身带 Tab（ECDICT 的 WinXP 就有），快照不能截掉后半截。
        ec = write(d.name, "ecdict.csv", ECDICT_CSV + 'winxp,x,,"abbr. 操作系统（\tWindows XP）",,,,,0,0,,,\n')
        en = write(d.name, "en.dict.yaml", EN_DICT + "winxp\twinxp\n")
        zh = write(d.name, "zh.dict.yaml", ZH_DICT)
        cedict = write(d.name, "c.txt", CEDICT)
        en_ref = write(d.name, "en_ref.tsv", "abandon\tv. 放弃\n")
        zh_ref = write(d.name, "zh_ref.tsv", "工\tn. work\n你好\t\n")
        ai = write(d.name, "ai.tsv", "生僻词\tn. rare word\n")
        p = lambda n: os.path.join(d.name, n)
        build.main(["snapshot", "--ecdict", ec, "--cedict", cedict, "--en-words", en, "--zh-words", zh,
                    "--en-refined", en_ref, "--zh-refined", zh_ref,
                    "--en-out", p("en_raw.tsv"), "--zh-out", p("zh_raw.tsv")])
        build.main(["en-zh", "--ecdict", ec, "--words", en, "--refined", en_ref, "--out", p("en_up.txt")])
        build.main(["en-zh", "--raw", p("en_raw.tsv"), "--words", en, "--refined", en_ref, "--out", p("en_raw.txt")])
        build.main(["zh-en", "--cedict", cedict, "--ai", ai, "--refined", zh_ref, "--words", zh, "--out", p("zh_up.txt")])
        build.main(["zh-en", "--raw", p("zh_raw.tsv"), "--ai", ai, "--refined", zh_ref, "--words", zh,
                    "--out", p("zh_raw.txt")])
        read = lambda n: open(p(n), encoding="utf-8").read()
        self.assertEqual(read("en_up.txt"), read("en_raw.txt"))
        self.assertEqual(read("zh_up.txt"), read("zh_raw.txt"))
        self.assertIn("winxp\tabbr. 操作系统（\tWindows XP）\n", read("en_raw.txt"))
        # 重整版有非空释义的词不进快照；空释义（你好）照旧回退原始释义。
        self.assertNotIn("abandon", read("en_raw.tsv"))
        self.assertNotIn("工\t", read("zh_raw.tsv"))
        self.assertIn("你好\thello; hi\n", read("zh_raw.tsv"))

    def test_upstream_and_raw_are_mutually_exclusive(self):
        with self.assertRaises(SystemExit):
            build.main(["zh-en", "--words", "x", "--out", "y"])


class ExportFullTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.d = self.tmp.name

    def test_en_zh_full_keeps_all_pos_lines_and_senses(self):
        ec = write(self.d, "ecdict.csv", ECDICT_CSV)
        en = write(self.d, "en.dict.yaml", EN_DICT)
        out = os.path.join(self.d, "full.tsv")
        build.main(["export-full", "--direction", "en-zh", "--ecdict", ec, "--words", en, "--out", out])
        with open(out, encoding="utf-8") as f:
            rows = dict(l.rstrip("\n").split("\t") for l in f if not l.startswith("#"))
        self.assertIn("n. 放任", rows["abandon"])  # 非首个词性行
        self.assertIn("使屈从", rows["abandon"])  # 第 3 个义项
        self.assertNotIn("\n", rows["abandon"])
        self.assertIn("亚伦", rows["aaron"])
        self.assertNotIn("nomatch", rows)

    def test_zh_en_full_merges_all_entries_and_filters_non_gloss(self):
        cedict = write(self.d, "c.txt", "#! x\n"
            "行 行 [hang2] /row/line/CL:個|个[ge4]/\n"
            "行 行 [xing2] /to walk/to go/OK/variant of 衍/\n"
            "旧 舊 [jiu4] /variant of 舊|旧/\n")
        zh = write(self.d, "zh.dict.yaml", "---\nname: x\n...\n行\ta\t1\n旧\tb\t1\n")
        out = os.path.join(self.d, "full.tsv")
        build.main(["export-full", "--direction", "zh-en", "--cedict", cedict, "--words", zh, "--out", out])
        with open(out, encoding="utf-8") as f:
            rows = dict(l.rstrip("\n").split("\t") for l in f if not l.startswith("#"))
        self.assertEqual(rows["行"], "row; line | to walk; to go; OK")
        self.assertNotIn("旧", rows)


PINYIN_MAP = (
    "# 汉字拼音反查表\n"
    "U+90ED: guō  # 郭\n"
    "U+8299: fú,fù  # 芙\n"
    "U+672C: běn  # 本\n"
    "U+6EAA: xī  # 溪\n"
    "U+5FB7: dé  # 德\n"
    "U+8D6B: hè  # 赫\n"
    "U+4E9A: yà  # 亚\n"
    "U+5415: lǚ  # 吕\n"
    "U+94F6: yín  # 银\n"
    "U+884C: xíng,háng  # 行\n"
)


class RomanizationTest(unittest.TestCase):
    def setUp(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        self.pm = build.load_pinyin_map(write(d.name, "pinyin_map.txt", PINYIN_MAP))

    def drops(self, word, meaning):
        return build.is_pinyin_romanization(word, meaning, self.pm)

    def test_names_written_as_pinyin_are_dropped(self):
        self.assertTrue(self.drops("郭芙", "n. Guo Fu"))
        self.assertTrue(self.drops("本溪", "n. Benxi"))

    def test_separators_case_and_tone_marks_ignored(self):
        self.assertTrue(self.drops("郭芙", "GUO-FU"))
        self.assertTrue(self.drops("郭芙", "n. Guō Fù"))
        self.assertTrue(self.drops("吕", "n. Lu"))
        self.assertTrue(self.drops("吕", "Lü"))

    def test_polyphone_any_reading_matches(self):
        self.assertTrue(self.drops("银行", "n. Yinhang"))
        self.assertTrue(self.drops("银行", "n. Yinxing"))

    def test_real_translations_kept(self):
        self.assertFalse(self.drops("德赫亚", "n. De Gea"))
        self.assertFalse(self.drops("银行", "n. bank"))
        self.assertFalse(self.drops("郭芙", ""))

    def test_unknown_char_never_matches(self):
        self.assertFalse(self.drops("郭X", "n. Guo X"))

    def test_load_ai_with_pinyin_drops_romanized_rows(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        ai = write(d.name, "ai.tsv", "郭芙\tn. Guo Fu\n银行\tn. bank\n")
        self.assertEqual(build.load_ai(ai, self.pm), {"银行": "n. bank"})
        self.assertEqual(len(build.load_ai(ai)), 2)

    def test_lint_ai_reports_count_and_rows(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        ai = write(d.name, "ai.tsv", "# c\n郭芙\tn. Guo Fu\n银行\tn. bank\n本溪\tn. Benxi\n")
        pm = write(d.name, "pinyin_map.txt", PINYIN_MAP)
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            build.main(["lint-ai", ai, "--pinyin", pm])
        out = buf.getvalue()
        self.assertIn("2/3", out)
        self.assertIn("郭芙\tn. Guo Fu", out)
        self.assertNotIn("银行", out)

    def test_zh_en_command_applies_filter_to_ai_and_refined(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        zh = write(d.name, "zh.dict.yaml", "---\nname: x\n...\n郭芙\ta\t1\n银行\tb\t1\n")
        cedict = write(d.name, "c.txt", "#! x\n")
        ai = write(d.name, "ai.tsv", "郭芙\tn. Guo Fu\n银行\tn. bank\n")
        ref = write(d.name, "ref.tsv", "郭芙\tn. Guo Fu\n")
        pm = write(d.name, "pinyin_map.txt", PINYIN_MAP)
        out = os.path.join(d.name, "o.txt")
        build.main(["zh-en", "--cedict", cedict, "--ai", ai, "--refined", ref, "--pinyin", pm, "--words", zh, "--out", out])
        with open(out, encoding="utf-8") as f:
            text = f.read()
        self.assertNotIn("郭芙", text)
        self.assertIn("银行\tn. bank", text)


class ParseTest(unittest.TestCase):
    def test_strict_pos_format_accepted(self):
        got, bad = ai_fill.parse_response("银行\tn. bank\n运行\tv. run; operate", ["银行", "运行"])
        self.assertEqual(got, [("银行", "n. bank"), ("运行", "v. run; operate")])
        self.assertEqual(bad, [])

    def test_blank_meaning_is_an_answer(self):
        got, bad = ai_fill.parse_response("生僻词\t", ["生僻词"])
        self.assertEqual(got, [("生僻词", "")])
        self.assertEqual(bad, [])

    def test_illegal_lines_rejected_not_recorded(self):
        text = "甲\tbank\n乙\tn.bank\n丙\tnoun. bank\n丁\tn. 好; n. 坏 extra\u0000\n戊\tn. ok"
        got, bad = ai_fill.parse_response(text, ["甲", "乙", "丙", "戊"])
        self.assertEqual(got, [("戊", "n. ok")])
        self.assertEqual([w for w, _ in bad], ["甲", "乙", "丙"])

    def test_unanswered_words_neither_accepted_nor_rejected(self):
        got, bad = ai_fill.parse_response("```\n甲\tn. a\n```\n多余\tn. x", ["甲", "乙"])
        self.assertEqual(got, [("甲", "n. a")])
        self.assertEqual(bad, [])

    def test_first_answer_wins(self):
        got, _ = ai_fill.parse_response("甲\tn. a\n甲\tn. b", ["甲"])
        self.assertEqual(got, [("甲", "n. a")])


class PromptTest(unittest.TestCase):
    def test_fill_prompt_demands_common_sense_with_pos_and_blank(self):
        p = ai_fill.build_prompt("fill", "zh-en", [("银行", "")])
        for needle in ("词性. 义项", "银行", "最常用", "空"):
            self.assertIn(needle, p)
        self.assertIn("不给例句", p)

    def test_refine_prompt_includes_existing_meanings(self):
        p = ai_fill.build_prompt("refine", "zh-en", [("银行", "bank; CL:家")])
        self.assertIn("银行\tbank; CL:家", p)
        self.assertIn("现有", p)

    def test_refine_prompt_en_zh_asks_chinese(self):
        p = ai_fill.build_prompt("refine", "en-zh", [("bank", "银行; 岸")])
        self.assertIn("中文", p)


class PosSetTest(unittest.TestCase):
    def test_prompt_pos_list_equals_parser_set(self):
        import re
        for mode in ("fill", "refine"):
            p = ai_fill.build_prompt(mode, "zh-en", [("银行", "bank")])
            self.assertIn("只能用下列词性", p)
            line = next(l for l in p.splitlines() if "只能用下列词性" in l)
            listed = set(re.findall(r"([a-z]+)\.", line.split("只能用下列词性", 1)[1]))
            self.assertEqual(listed, set(ai_fill.POS_TAGS))

    def test_refine_prompt_mentions_many_pos_and_senses(self):
        p = ai_fill.build_prompt("refine", "zh-en", [("行", "x")])
        self.assertIn("多个词性", p)
        self.assertIn("多个义项", p)


class RunTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.d = self.tmp.name

    def test_batches(self):
        self.assertEqual(list(ai_fill.batches(list("abcde"), 2)), [["a", "b"], ["c", "d"], ["e"]])

    def test_read_items_fill_and_refine(self):
        p = write(self.d, "in.tsv", "# c\n甲\n乙\tn. b\n\n")
        self.assertEqual(ai_fill.read_items(p, "fill"), [("甲", ""), ("乙", "")])
        self.assertEqual(ai_fill.read_items(p, "refine"), [("乙", "n. b")])

    def test_run_resumes_and_never_repeats_words(self):
        items = [(w, "") for w in "甲乙丙丁"]
        out = os.path.join(self.d, "ai.tsv")
        calls = []

        def fake(batch):
            calls.append([w for w, _ in batch])
            return "\n".join(f"{w}\tn. {w}-en" for w, _ in batch)

        be = ai_fill.CallBackend(fake)
        ai_fill.run(items, out, be, batch_size=2, limit=2, today="2026-01-01")
        self.assertEqual(calls, [["甲", "乙"]])
        ai_fill.run(items, out, be, batch_size=2, today="2026-01-01")
        self.assertEqual(calls, [["甲", "乙"], ["丙", "丁"]])
        with open(out, encoding="utf-8") as f:
            self.assertEqual(f.read().count("生成日期"), 1)
        self.assertEqual(build.load_ai(out), {w: f"n. {w}-en" for w in "甲乙丙丁"})

    def test_blank_answers_recorded_so_not_retried(self):
        items = [("甲", ""), ("乙", "")]
        out = os.path.join(self.d, "ai.tsv")
        ai_fill.run(items, out, ai_fill.CallBackend(lambda b: "甲\tn. a\n乙\t"), batch_size=10, today="2026-01-01")
        calls = []
        ai_fill.run(items, out, ai_fill.CallBackend(lambda b: calls.append(b) or ""), batch_size=10, today="2026-01-01")
        self.assertEqual(calls, [])

    def test_rejected_and_unanswered_words_are_retried_next_run(self):
        items = [("甲", ""), ("乙", ""), ("丙", "")]
        out = os.path.join(self.d, "ai.tsv")
        stats = ai_fill.run(items, out, ai_fill.CallBackend(lambda b: "甲\tn. a\n乙\tbad"), batch_size=10, today="2026-01-01")
        self.assertEqual((stats["written"], stats["rejected"]), (1, 1))
        seen = []
        ai_fill.run(items, out, ai_fill.CallBackend(lambda b: seen.append([w for w, _ in b]) or ""), batch_size=10, today="2026-01-01")
        self.assertEqual(seen, [["乙", "丙"]])

    def test_header_describes_mode(self):
        out = os.path.join(self.d, "r.tsv")
        ai_fill.run([("银行", "bank")], out, ai_fill.CallBackend(lambda b: "银行\tn. bank"),
                    mode="refine", direction="zh-en", today="2026-01-01")
        with open(out, encoding="utf-8") as f:
            head = f.readline() + f.readline()
        self.assertIn("重整", head)
        self.assertIn("claude-sonnet-5-5", head)


class FileBackendGuardTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.bdir = os.path.join(self.tmp.name, "b")
        self.out = os.path.join(self.tmp.name, "o.tsv")

    def go(self, items, **kw):
        return ai_fill.run(items, self.out, ai_fill.FileBackend(self.bdir), today="2026-01-01", **kw)

    def prompt(self):
        with open(os.path.join(self.bdir, "PROMPT.txt"), encoding="utf-8") as f:
            return f.read()

    def test_prompt_file_uses_real_mode_and_direction(self):
        self.go([("bank", "银行; 岸")], mode="refine", direction="en-zh")
        self.assertIn("中文", self.prompt())
        self.assertIn("n. 银行", self.prompt())

    def test_different_mode_or_direction_is_refused(self):
        items = [("银行", "bank")]
        self.go(items, mode="refine", direction="zh-en")
        with self.assertRaises(SystemExit) as cm:
            self.go(items, mode="refine", direction="en-zh")
        self.assertIn("哈希不同", str(cm.exception))
        with self.assertRaises(SystemExit):
            self.go([("银行", "")], mode="fill", direction="zh-en")

    def test_old_format_prompt_warns_and_rewrites_header(self):
        os.makedirs(self.bdir)
        with open(os.path.join(self.bdir, "PROMPT.txt"), "w", encoding="utf-8") as f:
            f.write("为下列中文词给出……\n")
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            self.go([("甲", "")])
        self.assertIn("旧格式", err.getvalue())
        self.assertTrue(self.prompt().startswith("# 输入哈希: "))
        self.go([("甲", "")])  # 现在是新格式，同输入可重跑


class PromptPolicyTest(unittest.TestCase):
    def test_both_prompts_carry_lexical_and_proper_noun_rules(self):
        for mode in ("fill", "refine"):
            p = ai_fill.build_prompt(mode, "zh-en", [("街霸", "x")])
            for needle in ("视觉效果", "言之有理", "就坐在", "你不用担心", "最好的办法就是", "讲一下", "街霸", "Street Fighter", "绝代双骄", "拼音"):
                self.assertIn(needle, p)


class ExtraRuleTest(unittest.TestCase):
    def test_extra_rule_appended_as_last_bullet_in_both_modes(self):
        for mode in ("fill", "refine"):
            p = ai_fill.build_prompt(mode, "zh-en", [("银行", "bank")], extra_rule="成语不给义项")
            rules = p.split("\n\n")[0]
            self.assertTrue(rules.rstrip().endswith("- 成语不给义项"))
            self.assertNotIn("成语不给义项", ai_fill.build_prompt(mode, "zh-en", [("银行", "bank")]))

    def test_different_extra_rule_changes_hash_and_is_refused(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        bdir, out = os.path.join(d.name, "b"), os.path.join(d.name, "o.tsv")
        run = lambda rule: ai_fill.run([("甲", "")], out, ai_fill.FileBackend(bdir), extra_rule=rule, today="2026-01-01")
        run("规则一")
        with open(os.path.join(bdir, "PROMPT.txt"), encoding="utf-8") as f:
            self.assertIn("- 规则一", f.read())
        run("规则一")
        with self.assertRaises(SystemExit):
            run("规则二")
        with self.assertRaises(SystemExit):
            run(None)


class FileBackendTest(unittest.TestCase):
    def test_refuses_batches_dir_from_a_different_input(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        bdir = os.path.join(d.name, "b")
        out = os.path.join(d.name, "o.tsv")
        ai_fill.run([("甲", "")], out, ai_fill.FileBackend(bdir), today="2026-01-01")
        ai_fill.run([("甲", "")], out, ai_fill.FileBackend(bdir), today="2026-01-01")  # 同输入可重跑
        with self.assertRaises(SystemExit) as cm:
            ai_fill.run([("甲", ""), ("乙", "")], out, ai_fill.FileBackend(bdir), today="2026-01-01")
        self.assertIn("清空", str(cm.exception))

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.d = self.tmp.name
        self.bdir = os.path.join(self.d, "batches")
        self.out = os.path.join(self.d, "ai.tsv")
        self.items = [(w, "") for w in "甲乙丙丁戊"]

    def run_files(self, **kw):
        return ai_fill.run(self.items, self.out, ai_fill.FileBackend(self.bdir), batch_size=2, today="2026-01-01", **kw)

    def read(self, name):
        with open(os.path.join(self.bdir, name), encoding="utf-8") as f:
            return f.read()

    def test_writes_numbered_input_batches_and_prompt(self):
        stats = self.run_files()
        self.assertEqual(sorted(os.listdir(self.bdir)), ["0000.in.tsv", "0001.in.tsv", "0002.in.tsv", "PROMPT.txt"])
        self.assertEqual(self.read("0000.in.tsv"), "甲\n乙\n")
        self.assertEqual(stats["written"], 0)
        self.assertEqual(stats["pending_batches"], 3)
        self.assertIn("词性. 义项", self.read("PROMPT.txt"))

    def test_refine_batches_carry_existing_meanings(self):
        be = ai_fill.FileBackend(self.bdir)
        ai_fill.run([("银行", "bank")], self.out, be, mode="refine", direction="zh-en", today="2026-01-01")
        self.assertEqual(self.read("0000.in.tsv"), "银行\tbank\n")

    def test_merges_out_files_and_resumes(self):
        self.run_files()
        with open(os.path.join(self.bdir, "0000.out.tsv"), "w", encoding="utf-8") as f:
            f.write("甲\tn. a\n乙\tn. b\n")
        with open(os.path.join(self.bdir, "0002.out.tsv"), "w", encoding="utf-8") as f:
            f.write("戊\tn. e\n")
        stats = self.run_files()
        self.assertEqual(stats["written"], 3)
        self.assertEqual(stats["pending_batches"], 1)
        self.assertEqual(build.load_ai(self.out), {"甲": "n. a", "乙": "n. b", "戊": "n. e"})
        # 再跑一遍：已合并的不重复，补上 0001
        with open(os.path.join(self.bdir, "0001.out.tsv"), "w", encoding="utf-8") as f:
            f.write("丙\tn. c\n丁\tn. d\n")
        stats = self.run_files()
        self.assertEqual(stats["written"], 2)
        self.assertEqual(stats["pending_batches"], 0)
        self.assertEqual(self.run_files()["written"], 0)
        self.assertEqual(len(build.load_ai(self.out)), 5)

    def test_batch_numbers_stay_stable_after_merging(self):
        self.run_files()
        with open(os.path.join(self.bdir, "0000.out.tsv"), "w", encoding="utf-8") as f:
            f.write("甲\tn. a\n乙\tn. b\n")
        self.run_files()
        self.assertEqual(self.read("0001.in.tsv"), "丙\n丁\n")
        self.assertFalse(os.path.exists(os.path.join(self.bdir, "0003.in.tsv")))

    def test_invalid_out_rows_rejected_and_stay_pending(self):
        self.run_files()
        with open(os.path.join(self.bdir, "0000.out.tsv"), "w", encoding="utf-8") as f:
            f.write("甲\tn. a\n乙\tnonsense\n")
        stats = self.run_files()
        self.assertEqual((stats["written"], stats["rejected"]), (1, 1))
        self.assertEqual(build.load_ai(self.out), {"甲": "n. a"})


if __name__ == "__main__":
    unittest.main()
