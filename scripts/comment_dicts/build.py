#!/usr/bin/env python3
"""中英释义注释词库生成：ECDICT / CC-CEDICT / AI 补齐 -> en_zh.txt、zh_en.txt。

输出每词一行 `词<TAB>词性. 义项`（CC-CEDICT 无词性，只有义项）。仅用标准库。
子命令：en-zh、zh-en、export-full、missing、plan、report。用法见 `-h`。
"""

import argparse
import csv
import gzip
import os
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


def read_rows(path):
    """读 rime .dict.yaml，逐行产出 (词, 权重或 None)。按 columns 取 text/weight 列；
    无 columns 时按 rime 默认 text、code、weight。"""
    with open(path, encoding="utf-8") as f:
        in_body = False
        text_col, weight_col = 0, 2
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
                    if columns:
                        weight_col = columns.index("weight") if "weight" in columns else None
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
            if text_col >= len(cols) or not cols[text_col].strip():
                continue
            weight = None
            if weight_col is not None and weight_col < len(cols):
                try:
                    weight = float(cols[weight_col])
                except ValueError:
                    pass
            yield cols[text_col].strip(), weight


def load_words(paths):
    """词条文本，保持首见顺序。"""
    words = {}
    for path in paths:
        for text, _ in read_rows(path):
            words.setdefault(text, None)
    return list(words)


def load_weighted(paths):
    """{词: 最大权重}，保持首见顺序；无权重记 0。同词多音取最大权重。"""
    result = {}
    for path in paths:
        for text, weight in read_rows(path):
            w = weight or 0.0
            if text not in result or w > result[text]:
                result[text] = w
    return result


def group_name(path):
    name = os.path.basename(str(path))
    return name[: -len(".dict.yaml")] if name.endswith(".dict.yaml") else os.path.splitext(name)[0]


def select_missing(cedict, ai, general, wubi, category=(), min_freq=100, wubi_top=10000):
    """待补词表 [(来源组, 词)]。
    通用词库（general）：CEDICT 未覆盖、权重 >= min_freq；
    五笔（wubi）：拼音词库（general+category）与 CEDICT 都没有，按自身权重降序取前 wubi_top。
    分类词库只用于排除；单字、非中文词不补；已有 AI 释义的不再补；跨来源去重，先到先得。"""

    def eligible(w):
        return len(w) >= 2 and CJK.search(w) and w not in cedict and not ai.get(w)

    seen, selected = set(), []
    pinyin_known = set()
    for path in general:
        group = group_name(path)
        for w, weight in load_weighted([path]).items():
            pinyin_known.add(w)
            if weight >= min_freq and eligible(w) and w not in seen:
                seen.add(w)
                selected.append((group, w))
    pinyin_known.update(load_words(list(category)))
    pool = []
    for path in wubi:
        group = group_name(path)
        for w, weight in load_weighted([path]).items():
            if w not in pinyin_known and eligible(w) and w not in seen:
                pool.append((weight, group, w))
    pool.sort(key=lambda x: -x[0])  # 稳定排序，同权重保持文件序
    for _, group, w in pool[:wubi_top]:
        if w not in seen:
            seen.add(w)
            selected.append((group, w))
    return selected


def plan_summary(selected, batch_size):
    counts = {}
    for group, _ in selected:
        counts[group] = counts.get(group, 0) + 1
    lines = [f"[待补] {group}: {n}" for group, n in counts.items()]
    batches = (len(selected) + batch_size - 1) // batch_size
    lines.append(f"[合计] {len(selected)} 词，每批 {batch_size}，预计 {batches} 批")
    return lines


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


def ecdict_full(translation):
    """ECDICT translation 全部词性行、全部义项，行间用 ` | ` 连成一行（给重整用，不裁剪）。"""
    lines = [l.strip() for l in translation.replace("\\n", "\n").split("\n") if l.strip()]
    return " | ".join(lines)


def load_ecdict(path, wanted, entry_fn=ecdict_entry):
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
            entry = entry_fn(row["translation"] or "")
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


def build_en_zh(ecdict_path, word_paths, refined=None):
    """refined（重整后的源数据）优先于 ECDICT 原始取义；只保留词表里的词。"""
    words = load_words(word_paths)
    entries = load_ecdict(ecdict_path, words)
    for w in words:
        if refined and refined.get(w):
            entries[w] = refined[w]
    return entries, words


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


def load_cedict_full(path, wanted):
    """CC-CEDICT 同词所有条目的释义（滤掉变体/参见/量词），条目间 ` | `，条目内 `; `；简体优先、其次繁体。"""
    by_simp, by_trad = {}, {}
    with open_text(path) as f:
        for line in f:
            m = CEDICT_LINE.match(line) if not line.startswith("#") else None
            if not m:
                continue
            trad, simp, defs = m.groups()
            if simp not in wanted and trad not in wanted:
                continue
            good = [d.strip() for d in defs.split("/") if d.strip() and not CEDICT_NOT_A_GLOSS.match(d.strip())]
            if not good:
                continue
            by_simp.setdefault(simp, []).append("; ".join(good))
            by_trad.setdefault(trad, []).append("; ".join(good))
    for trad, entries in by_trad.items():
        by_simp.setdefault(trad, entries)
    return {w: " | ".join(dict.fromkeys(e)) for w, e in by_simp.items()}


def load_ai(path):
    """读 词<TAB>释义 的源数据 tsv（zh_en_ai / *_refined）：跳过 # 注释与空释义行。"""
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


def build_zh_en(cedict, ai, words, refined=None):
    """优先级 refined > CEDICT 原始 > AI；只保留词表里的词。"""
    refined = refined or {}
    result = {}
    for w in words:
        entry = refined.get(w) or cedict.get(w) or ai.get(w)
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
    entries, words = build_en_zh(a.ecdict, a.words, load_ai(a.refined))
    write_entries(entries, a.out)
    print(f"en_zh: {len(entries)}/{len(words)} -> {a.out}", file=sys.stderr)


def cmd_zh_en(a):
    words = zh_words(a.words)
    cedict = load_cedict(a.cedict, set(words))
    entries = build_zh_en(cedict, load_ai(a.ai), words, load_ai(a.refined))
    write_entries(entries, a.out, cedict_header(a.cedict))
    print(f"zh_en: {len(entries)}/{len(words)} -> {a.out}", file=sys.stderr)


def cmd_export_full(a):
    """导出未裁剪的全部义项，供 ai_fill.py --mode refine 作输入。词表范围同 en-zh / zh-en。"""
    if a.direction == "en-zh":
        words = load_words(a.words)
        entries = load_ecdict(a.ecdict, words, ecdict_full)
        header = ""
    else:
        words = zh_words(a.words)
        full = load_cedict_full(a.cedict, set(words))
        entries = {w: full[w] for w in words if w in full}
        header = cedict_header(a.cedict)
    write_entries(entries, a.out, header)
    print(f"export-full {a.direction}: {len(entries)}/{len(words)} -> {a.out}", file=sys.stderr)


def selection(a):
    general = list(a.general)
    cedict = load_cedict(a.cedict, set(load_words(general + list(a.wubi) + list(a.category))))
    return select_missing(cedict, load_ai(a.ai), general, a.wubi, a.category, a.min_freq, a.wubi_top)


def write_list(selected, out):
    f = open(out, "w", encoding="utf-8", newline="\n") if out else sys.stdout
    for _, w in selected:
        f.write(w + "\n")
    if out:
        f.close()


def cmd_missing(a):
    write_list(selection(a), a.out)


def cmd_plan(a):
    selected = selection(a)
    write_list(selected, a.out)
    print("\n".join(plan_summary(selected, a.batch_size)))


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
    s.add_argument("--refined", help="en_zh_refined.tsv（重整后的源数据，优先于 ECDICT 原始取义）")
    s.add_argument("--out", required=True)
    s.set_defaults(fn=cmd_en_zh)

    s = sub.add_parser("zh-en", help="CC-CEDICT + AI 按中文词表裁剪")
    s.add_argument("--cedict", required=True)
    s.add_argument("--ai", help="zh_en_ai.tsv")
    s.add_argument("--refined", help="zh_en_cedict_refined.tsv（优先于 CEDICT 原始取义）")
    s.add_argument("--words", nargs="+", required=True)
    s.add_argument("--out", required=True)
    s.set_defaults(fn=cmd_zh_en)

    s = sub.add_parser("export-full", help="导出未裁剪的全部义项（ai_fill.py refine 的输入）")
    s.add_argument("--direction", choices=["en-zh", "zh-en"], required=True)
    s.add_argument("--ecdict", help="en-zh 需要")
    s.add_argument("--cedict", help="zh-en 需要")
    s.add_argument("--words", nargs="+", required=True, help="同 en-zh / zh-en 的词表")
    s.add_argument("--out", required=True)
    s.set_defaults(fn=cmd_export_full)

    def selection_args(s):
        s.add_argument("--cedict", required=True)
        s.add_argument("--ai")
        s.add_argument("--general", nargs="+", required=True, help="通用词库 base/ext：CEDICT 缺口且词频 >= --min-freq")
        s.add_argument("--wubi", nargs="*", default=[], help="五笔词库：拼音与 CEDICT 都没有的，按权重取前 --wubi-top")
        s.add_argument("--category", nargs="*", default=[], help="分类词库（含 idiom）：不补，只用于排除")
        s.add_argument("--min-freq", type=int, default=100)
        s.add_argument("--wubi-top", type=int, default=10000)
        s.add_argument("--out", help="待补词表，每行一词；missing 缺省输出到 stdout")

    s = sub.add_parser("missing", help="列出待补词（选词口径同 plan）")
    selection_args(s)
    s.set_defaults(fn=cmd_missing)

    s = sub.add_parser("plan", help="待补词表：按来源计数、预计批数，并写出词表")
    selection_args(s)
    s.add_argument("--batch-size", type=int, default=200)
    s.set_defaults(fn=cmd_plan)

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
