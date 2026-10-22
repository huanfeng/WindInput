"""build.py / ai_fill.py 的测试。无 pytest 依赖：`python3 -I -m unittest discover -s scripts/comment_dicts`，
装了 pytest 也能直接跑。"""

import gzip
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


class AiFillTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.d = self.tmp.name

    def test_batches(self):
        self.assertEqual(list(ai_fill.batches(list("abcde"), 2)), [["a", "b"], ["c", "d"], ["e"]])

    def test_parse_response_blank_for_unanswered_and_fenced(self):
        text = "```\n甲\tn. a\n乙\t\n多余\tn. x\n```"
        self.assertEqual(
            ai_fill.parse_response(text, ["甲", "乙", "丙"]),
            [("甲", "n. a"), ("乙", ""), ("丙", "")],
        )

    def test_run_resumes_and_never_repeats_words(self):
        missing = write(self.d, "missing.txt", "甲\n乙\n丙\n丁\n")
        out = os.path.join(self.d, "ai.tsv")
        calls = []

        def fake(batch):
            calls.append(list(batch))
            return "\n".join(f"{w}\tn. {w}-en" for w in batch)

        ai_fill.run(missing, out, batch_size=2, limit=2, call=fake, today="2026-01-01")
        self.assertEqual(calls, [["甲", "乙"]])
        ai_fill.run(missing, out, batch_size=2, limit=None, call=fake, today="2026-01-01")
        self.assertEqual(calls, [["甲", "乙"], ["丙", "丁"]])
        with open(out, encoding="utf-8") as f:
            text = f.read()
        self.assertEqual(text.count("生成日期"), 1)
        self.assertEqual(build.load_ai(out), {w: f"n. {w}-en" for w in "甲乙丙丁"})

    def test_blank_answers_are_recorded_so_not_retried(self):
        missing = write(self.d, "missing.txt", "甲\n乙\n")
        out = os.path.join(self.d, "ai.tsv")
        ai_fill.run(missing, out, batch_size=10, limit=None, call=lambda b: "甲\tn. a", today="2026-01-01")
        calls = []
        ai_fill.run(missing, out, batch_size=10, limit=None, call=lambda b: calls.append(b) or "", today="2026-01-01")
        self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
