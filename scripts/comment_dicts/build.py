#!/usr/bin/env python3
"""中英释义注释词库生成：ECDICT / CC-CEDICT / AI 补齐 -> en_zh.txt、zh_en.txt。

输出每词一行 `词<TAB>词性. 义项`（CC-CEDICT 无词性，只有义项）。仅用标准库。
子命令：en-zh、zh-en、missing、report。用法见 `-h`。
"""

import argparse
import csv
import gzip
import re
import sys

CJK = re.compile(r"[㐀-鿿豈-﫿\U00020000-\U0002ffff]")
POS_LINE = re.compile(r"^([a-z]{1,6})\.\s*(.*)$")
LEADING_TAG = re.compile(r"^\[[^\]]*\]\s*")
OPEN_BRACKETS = "(（[〔<"
CLOSE_BRACKETS = ")）]〕>"
SENSE_SEPARATORS = ",，;；、"
# CC-CEDICT 里不是释义的条目：变体指向、参见、量词
CEDICT_NOT_A_GLOSS = re.compile(
    r"^(CL:|variant of|old variant of|erhua variant of|archaic variant of|"
    r"Japanese variant of|also written|see |abbr\. for|used in )"
)


def load_words(paths):
    """读 rime .dict.yaml 的词条文本（按 columns 取 text 列，默认第一列），保持首见顺序。"""
    words = {}
    for path in paths:
        with open(path, encoding="utf-8") as f:
            in_body = False
            text_col = 0
            columns = []
            in_columns = False
            for raw in f:
                line = raw.rstrip("\r\n")
                if not in_body:
                    stripped = line.strip()
                    if stripped == "...":
                        in_body = True
                        if "text" in columns:
                            text_col = columns.index("text")
                    elif stripped == "columns:":
                        in_columns = True
                    elif in_columns and stripped.startswith("- "):
                        columns.append(stripped[2:].strip())
                    elif in_columns and not stripped.startswith("#"):
                        in_columns = False
                    continue
                if not line.strip() or line.lstrip().startswith("#"):
                    continue
                cols = line.split("\t")
                if text_col < len(cols) and cols[text_col].strip():
                    words.setdefault(cols[text_col].strip(), None)
    return list(words)


def split_senses(text):
    """按顶层分隔符切义项（括号内的逗号不切）。"""
    senses, buf, depth = [], [], 0
    for ch in text:
        if ch in OPEN_BRACKETS:
            depth += 1
        elif ch in CLOSE_BRACKETS and depth > 0:
            depth -= 1
        if depth == 0 and ch in SENSE_SEPARATORS:
            senses.append("".join(buf).strip())
            buf = []
        else:
            buf.append(ch)
    senses.append("".join(buf).strip())
    return [s for s in senses if s]


def ecdict_entry(translation):
    """ECDICT translation 字段 -> `词性. 义项；义项`，取第一个有词性的行、前两个义项。"""
    lines = [l.strip() for l in translation.replace("\\n", "\n").split("\n") if l.strip()]
    pos, body = "", ""
    for l in lines:
        m = POS_LINE.match(l)
        if m and m.group(2):
            pos, body = m.group(1), m.group(2)
            break
    if not body:
        for l in lines:
            body = LEADING_TAG.sub("", l)
            if body:
                break
    senses = split_senses(body)[:2]
    if not senses:
        return ""
    meaning = "；".join(senses)
    return f"{pos}. {meaning}" if pos else meaning


def load_ecdict(path, wanted):
    """读 ECDICT csv，只保留词表里的词（精确匹配优先，其次忽略大小写）。返回 {词表词: 释义}。"""
    csv.field_size_limit(1 << 24)
    wanted_lower = {}
    for w in wanted:
        wanted_lower.setdefault(w.lower(), []).append(w)
    exact, lowered = {}, {}
    with open(path, encoding="utf-8", newline="") as f:
        for row in csv.DictReader(f):
            word = row["word"]
            if word.lower() not in wanted_lower:
                continue
            entry = ecdict_entry(row["translation"] or "")
            if not entry:
                continue
            exact.setdefault(word, entry)
            lowered.setdefault(word.lower(), entry)
    result = {}
    for w in wanted:
        entry = exact.get(w) or lowered.get(w.lower())
        if entry:
            result[w] = entry
    return result


def build_en_zh(ecdict_path, word_paths):
    words = load_words(word_paths)
    return load_ecdict(ecdict_path, words), words


def open_text(path):
    if str(path).endswith(".gz"):
        return gzip.open(path, "rt", encoding="utf-8")
    return open(path, encoding="utf-8")


CEDICT_LINE = re.compile(r"^(\S+) (\S+) \[[^\]]*\] /(.*)/\s*$")


def cedict_meaning(defs):
    good = [d.strip() for d in defs if d.strip() and not CEDICT_NOT_A_GLOSS.match(d.strip())]
    return "; ".join(good[:2])


def load_cedict(path, wanted=None):
    """读 CC-CEDICT，按简体（其次繁体）建索引；同词多条取第一条有释义的。"""
    by_simp, by_trad = {}, {}
    with open_text(path) as f:
        for line in f:
            if line.startswith("#"):
                continue
            m = CEDICT_LINE.match(line)
            if not m:
                continue
            trad, simp, defs = m.groups()
            if wanted is not None and simp not in wanted and trad not in wanted:
                continue
            meaning = cedict_meaning(defs.split("/"))
            if not meaning:
                continue
            by_simp.setdefault(simp, meaning)
            by_trad.setdefault(trad, meaning)
    for trad, meaning in by_trad.items():
        by_simp.setdefault(trad, meaning)
    return by_simp


def load_ai(path):
    """读 zh_en_ai.tsv：跳过 # 注释与空释义行。"""
    result = {}
    if not path:
        return result
    try:
        f = open(path, encoding="utf-8")
    except FileNotFoundError:
        return result
    with f:
        for line in f:
            line = line.rstrip("\r\n")
            if not line or line.startswith("#"):
                continue
            cols = line.split("\t")
            if len(cols) < 2 or not cols[0].strip() or not cols[1].strip():
                continue
            result.setdefault(cols[0].strip(), cols[1].strip())
    return result


def build_zh_en(cedict, ai, words):
    """CEDICT 优先，其次 AI；只保留词表里的词。"""
    result = {}
    for w in words:
        entry = cedict.get(w) or ai.get(w)
        if entry:
            result[w] = entry
    return result


def missing_words(cedict, ai, words):
    return [w for w in words if CJK.search(w) and w not in cedict and w not in ai]


def render(entries):
    return "".join(f"{w}\t{entries[w]}\n" for w in sorted(entries))


def write_entries(entries, out, header=""):
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        f.write(header)
        f.write(render(entries))


def cedict_header(path):
    """CC-CEDICT 文件头的 `#!` 行（许可、日期、时间戳），写进产物注释行以便追溯快照。"""
    lines = []
    with open_text(path) as f:
        for line in f:
            if not line.startswith("#"):
                break
            if line.startswith("#!"):
                lines.append("# CC-CEDICT " + line[2:].strip() + "\n")
    return "".join(lines)


def pct(n, total):
    return f"{100.0 * n / total:.2f}%" if total else "n/a"


def size_lines(label, entries, total):
    data = render(entries).encode("utf-8")
    gz = len(gzip.compress(data, 9))
    return [
        f"[{label}]",
        f"  条目数: {len(entries)} / 词表 {total}  覆盖率: {pct(len(entries), total)}",
        f"  文件字节数: {len(data)}  gzip 字节数: {gz}",
    ]


def zh_words(paths):
    return [w for w in load_words(paths) if CJK.search(w)]


def cmd_en_zh(a):
    entries, words = build_en_zh(a.ecdict, a.words)
    write_entries(entries, a.out)
    print(f"en_zh: {len(entries)}/{len(words)} -> {a.out}", file=sys.stderr)


def cmd_zh_en(a):
    words = zh_words(a.words)
    cedict = load_cedict(a.cedict, set(words))
    entries = build_zh_en(cedict, load_ai(a.ai), words)
    write_entries(entries, a.out, cedict_header(a.cedict))
    print(f"zh_en: {len(entries)}/{len(words)} -> {a.out}", file=sys.stderr)


def cmd_missing(a):
    words = zh_words(a.words)
    cedict = load_cedict(a.cedict, set(words))
    out = open(a.out, "w", encoding="utf-8", newline="\n") if a.out else sys.stdout
    for w in missing_words(cedict, load_ai(a.ai), words):
        out.write(w + "\n")
    if a.out:
        out.close()


def cmd_report(a):
    en_entries, en_words = build_en_zh(a.ecdict, a.en_words)
    words = zh_words(a.zh_words)
    cedict = load_cedict(a.cedict, set(words))
    ai = load_ai(a.ai)
    cedict_only = build_zh_en(cedict, {}, words)
    lines = size_lines("en_zh (ECDICT)", en_entries, len(en_words))
    lines += size_lines("zh_en (仅 CC-CEDICT)", cedict_only, len(words))
    missing = missing_words(cedict, ai, words)
    lines.append(f"[缺口] CEDICT 与 AI 都没有的中文词: {len(missing)}（需 AI 补齐，占词表 {pct(len(missing), len(words))}）")
    if ai:
        lines += size_lines("zh_en (CEDICT + AI)", build_zh_en(cedict, ai, words), len(words))
    print("\n".join(lines))


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("en-zh", help="ECDICT 按英文词表裁剪")
    s.add_argument("--ecdict", required=True)
    s.add_argument("--words", nargs="+", required=True, help="en.dict.yaml en_ext.dict.yaml")
    s.add_argument("--out", required=True)
    s.set_defaults(fn=cmd_en_zh)

    s = sub.add_parser("zh-en", help="CC-CEDICT + AI 按中文词表裁剪")
    s.add_argument("--cedict", required=True)
    s.add_argument("--ai", help="zh_en_ai.tsv")
    s.add_argument("--words", nargs="+", required=True)
    s.add_argument("--out", required=True)
    s.set_defaults(fn=cmd_zh_en)

    s = sub.add_parser("missing", help="列出 CEDICT 与 AI 都没覆盖的中文词")
    s.add_argument("--cedict", required=True)
    s.add_argument("--ai")
    s.add_argument("--words", nargs="+", required=True)
    s.add_argument("--out", help="缺省输出到 stdout")
    s.set_defaults(fn=cmd_missing)

    s = sub.add_parser("report", help="条目数、覆盖率、体积")
    s.add_argument("--ecdict", required=True)
    s.add_argument("--cedict", required=True)
    s.add_argument("--ai")
    s.add_argument("--en-words", nargs="+", required=True)
    s.add_argument("--zh-words", nargs="+", required=True)
    s.set_defaults(fn=cmd_report)

    a = p.parse_args(argv)
    a.fn(a)


if __name__ == "__main__":
    main()
