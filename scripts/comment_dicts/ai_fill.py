#!/usr/bin/env python3
"""生成中英释义源数据：补齐（fill）与重整（refine），可断点续跑。

- fill：给 CC-CEDICT 没覆盖的中文词生成英文释义。输入每行一词（`build.py plan/missing` 的输出）。
- refine：输入未裁剪的全部义项 `词<TAB>全部义项`（`build.py export-full` 的输出，别用已裁剪的 en_zh/zh_en），让 AI 从现有义项里
  挑最常用的一两个并补词性。
两种模式输出同一格式 `词<TAB>词性. 义项`；没有可靠释义的词输出空释义（记空行，下次不再重试）。
格式不合法的行拒收、不落盘，下次重跑会重试。

后端：
- api：调 Claude API（需环境变量 ANTHROPIC_API_KEY）。
- files：离线批次。把待处理批次写成 `<dir>/NNNN.in.tsv`（并附 PROMPT.txt），
  由人工或会话内 agent 产出同名 `NNNN.out.tsv`，再次运行即合并。批号按完整输入顺序固定，
  合并后不会漂移；重复运行是幂等的。
"""

import argparse
import hashlib
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
from datetime import date

MODEL = "claude-sonnet-5-5"
BATCH_SIZE = 200
API_URL = "https://api.anthropic.com/v1/messages"

# 严格格式：词性. 义项（点号后一个空格，词性只写一个）
POS_TAGS = ("n", "v", "vt", "vi", "adj", "adv", "prep", "conj", "pron", "num", "mw", "int", "art", "aux", "det", "abbr")
VALID_MEANING = re.compile(r"^(?:" + "|".join(POS_TAGS) + r")\. \S[^\t\r\n\x00]*$")

POS_LIST = " ".join(t + "." for t in POS_TAGS)
FORMAT_RULES = f"""- 每词只给最常用的一两个义项，两个义项之间用「{{sep}}」隔开；不给例句，不叠加冷僻义项，尽量短
- 格式严格为「词性. 义项」，只能用下列词性之一且只写一个：{POS_LIST}，点号后一个空格（如 {{example}}）
- 没有可靠释义的词（生僻词、专名、不成词的片段）只输出「词<TAB>」，释义留空，不要硬凑
- 只输出结果行：每词一行，词与输入完全一致、顺序一致；不要解释，不要编号，不要代码块"""

# 取词口径（维护者已定）：只给词汇和固定说法释义；短语碎片留空；专名只给通行译名，不拼音化
POLICY_ZH_EN = """- 只给词汇和固定说法释义（如 视觉效果、药物治疗、言之有理）；短语碎片一律留空：词加虚词、代词加动词、半截句子都算碎片（如 就坐在、你不用担心、最好的办法就是、讲一下、在这样）
- 专名（人名、作品名、品牌）只在有通行英文名时给出（如 街霸 -> Street Fighter）；没有就留空；绝不音译成拼音（如 绝代双骄 不能写成 Juedai Shuangjiao）"""

EXAMPLES = {"zh-en": ("; ", "`n. bank`、`v. run; operate`"), "en-zh": ("；", "`n. 银行`、`v. 跑；运转`")}

FILL_PROMPT = """为下列中文词给出最常用的英文释义，供输入法候选旁的注释使用。
每词输出一行：词<TAB>词性. 义项
{rules}

词表：
{lines}
"""

REFINE_PROMPT = """下列每行是「词<TAB>现有释义」。请从现有释义里挑出最常用的一两个义项，并补上词性，供输入法候选旁的注释使用。
现有释义可能含多个词性、很多个义项（词性行之间用 ` | ` 隔开）；请判断哪个词性和义项最常用，只挑一两个。
义项只能来自现有释义（可精简、可调整措辞，不要新增别的义项）；释义语言为{lang}。
每词输出一行：词<TAB>词性. 义项
{rules}

条目：
{lines}
"""

LANG = {"zh-en": "英文", "en-zh": "中文"}


def build_prompt(mode, direction, items, extra_rule=None):
    sep, example = EXAMPLES[direction]
    rules = FORMAT_RULES.format(sep=sep, example=example)
    if direction == "zh-en":
        rules += "\n" + POLICY_ZH_EN
    if extra_rule:
        rules += "\n- " + extra_rule
    if mode == "refine":
        lines = "\n".join(f"{w}\t{m}" for w, m in items)
        return REFINE_PROMPT.format(lang=LANG[direction], rules=rules, lines=lines)
    return FILL_PROMPT.format(rules=rules, lines="\n".join(w for w, _ in items))


def batches(items, size):
    for i in range(0, len(items), size):
        yield items[i : i + size]


def read_items(path, mode):
    """[(词, 现有释义)]。fill 只取第一列（释义为空串）；refine 只要带现有释义的行。"""
    items = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip("\r\n")
            if not line.strip() or line.startswith("#"):
                continue
            cols = line.split("\t")
            word = cols[0].strip()
            existing = cols[1].strip() if len(cols) > 1 else ""
            if mode == "refine" and not existing:
                continue
            items.append((word, existing if mode == "refine" else ""))
    return items


def done_words(tsv):
    """已处理过的词（含空释义行）。"""
    if not os.path.exists(tsv):
        return set()
    done = set()
    with open(tsv, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip("\r\n")
            if line and not line.startswith("#"):
                done.add(line.split("\t")[0])
    return done


def parse_response(text, batch):
    """返回 (已接受 [(词, 释义)], 拒收 [(词, 原行)])。
    空释义算有效回答；格式不合法的拒收；批外的词与应答里没出现的词既不接受也不拒收（留待重试）。"""
    wanted = set(batch)
    seen, accepted, rejected = set(), [], []
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("```"):
            continue
        cols = line.split("\t")
        word = cols[0].strip()
        if word not in wanted or word in seen:
            continue
        seen.add(word)
        meaning = cols[1].strip() if len(cols) > 1 else ""
        if len(cols) > 2 or (meaning and not VALID_MEANING.match(meaning)):
            rejected.append((word, line))
        else:
            accepted.append((word, meaning))
    return accepted, rejected


def header(today, mode="fill", direction="zh-en"):
    if mode == "refine":
        src = "ECDICT" if direction == "en-zh" else "CC-CEDICT"
        what = f"重整 {src} 的现有释义：挑最常用的一两个义项并补词性"
    else:
        what = "补 CC-CEDICT 没覆盖的词"
    arrow = "英→中" if direction == "en-zh" else "中→英"
    return (
        f"# {arrow}释义，由 AI（{MODEL}）生成，生成日期 {today}。\n"
        f"# {what}；格式 词<TAB>词性. 义项；空释义行表示没有可靠释义。\n"
    )


def call_claude(prompt):
    key = os.environ.get("ANTHROPIC_API_KEY")
    if not key:
        sys.exit("缺少环境变量 ANTHROPIC_API_KEY")
    body = json.dumps(
        {"model": MODEL, "max_tokens": 16000, "messages": [{"role": "user", "content": prompt}]}
    ).encode()
    req = urllib.request.Request(
        API_URL,
        data=body,
        headers={"x-api-key": key, "anthropic-version": "2023-06-01", "content-type": "application/json"},
    )
    for attempt in range(5):
        try:
            with urllib.request.urlopen(req, timeout=300) as resp:
                data = json.load(resp)
            return "".join(b.get("text", "") for b in data["content"])
        except (urllib.error.URLError, TimeoutError) as e:
            if attempt == 4:
                raise
            print(f"请求失败，重试: {e}", file=sys.stderr)
            time.sleep(2 ** (attempt + 1))


class CallBackend:
    """在线后端：fn(items) -> 应答文本。默认的 API 后端见 ApiBackend。"""

    def __init__(self, fn):
        self.fn = fn

    def fetch(self, index, items, mode, direction):
        return self.fn(items)


class ApiBackend(CallBackend):
    def __init__(self, extra_rule=None):
        super().__init__(None)
        self.extra_rule = extra_rule

    def fetch(self, index, items, mode, direction):
        return call_claude(build_prompt(mode, direction, items, self.extra_rule))


class FileBackend:
    """离线批次后端：写 NNNN.in.tsv，读 NNNN.out.tsv；没有 out 文件的批次留待下次。"""

    def __init__(self, directory):
        self.dir = directory

    def begin(self, items, mode, direction="zh-en", extra_rule=None):
        """批次目录绑定到一份输入：PROMPT.txt 首行记（模式、方向、词表）的哈希，不同输入拒绝复用。
        旧格式（无哈希行）只警告，并按当前输入重写。"""
        body = "\n".join(f"{w}\t{m}" for w, m in items)
        digest = hashlib.sha256((f"{mode}\n{direction}\n{body}" + (f"\n{extra_rule}" if extra_rule else "")).encode()).hexdigest()[:16]
        first_line = "# 输入哈希: " + digest
        prompt = os.path.join(self.dir, "PROMPT.txt")
        if os.path.exists(prompt):
            with open(prompt, encoding="utf-8") as f:
                first = f.readline().strip()
            if first == first_line:
                return
            if not first.startswith("# 输入哈希:"):
                print(f"警告: 批次目录 {self.dir} 的 PROMPT.txt 是旧格式（无输入哈希），已按当前输入重写", file=sys.stderr)
            else:
                sys.exit(f"批次目录 {self.dir} 的输入哈希不同（模式、方向或词表变了），请先清空该目录再跑")
        os.makedirs(self.dir, exist_ok=True)
        placeholder = [("<见 NNNN.in.tsv>", "<现有释义>" if mode == "refine" else "")]
        with open(prompt, "w", encoding="utf-8", newline="\n") as f:
            f.write(first_line + "\n")
            f.write(build_prompt(mode, direction, placeholder, extra_rule))

    def fetch(self, index, items, mode, direction):
        os.makedirs(self.dir, exist_ok=True)
        stem = os.path.join(self.dir, f"{index:04d}")
        if not os.path.exists(stem + ".in.tsv"):
            with open(stem + ".in.tsv", "w", encoding="utf-8", newline="\n") as f:
                for w, m in items:
                    f.write(f"{w}\t{m}\n" if mode == "refine" else f"{w}\n")
        if not os.path.exists(stem + ".out.tsv"):
            return None
        with open(stem + ".out.tsv", encoding="utf-8") as f:
            return f.read()


def run(items, out, backend, mode="fill", direction="zh-en", batch_size=BATCH_SIZE, limit=None, today=None, extra_rule=None):
    """处理 items 里还没出现在 out 的词；每批写完即落盘，中断后重跑接着来。
    批号按完整 items 顺序固定。limit 只限在线后端本次处理的词数。返回统计 dict。"""
    stats = {"written": 0, "blank": 0, "rejected": 0, "pending_batches": 0}
    if hasattr(backend, "begin"):
        backend.begin(items, mode, direction, extra_rule)
    done = done_words(out)
    new_file = not os.path.exists(out) or os.path.getsize(out) == 0
    remaining = limit
    f = None
    try:
        for index, batch in enumerate(batches(items, batch_size)):
            todo = [(w, m) for w, m in batch if w not in done]
            if not todo:
                continue
            if remaining is not None:
                if remaining <= 0:
                    break
                todo = todo[:remaining]
            text = backend.fetch(index, todo, mode, direction)
            if text is None:
                stats["pending_batches"] += 1
                continue
            accepted, rejected = parse_response(text, [w for w, _ in todo])
            if remaining is not None:
                remaining -= len(todo)
            stats["rejected"] += len(rejected)
            if rejected:
                print(f"批 {index:04d} 拒收 {len(rejected)} 行（格式不合法）: {rejected[0][1]!r} ...", file=sys.stderr)
            if not accepted:
                continue
            if f is None:
                f = open(out, "a", encoding="utf-8", newline="\n")
                if new_file:
                    f.write(header(today or date.today().isoformat(), mode, direction))
            for word, meaning in accepted:
                f.write(f"{word}\t{meaning}\n")
                done.add(word)
                stats["written"] += 1
                stats["blank"] += not meaning
            f.flush()
            print(f"批 {index:04d} 已合并 {len(accepted)} 词", file=sys.stderr)
    finally:
        if f:
            f.close()
    return stats


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--mode", choices=["fill", "refine"], default="fill")
    p.add_argument("--direction", choices=["zh-en", "en-zh"], default="zh-en", help="释义语言；fill 只支持 zh-en")
    p.add_argument("--input", "--missing", dest="input", required=True,
                   help="fill：每行一词；refine：build.py export-full 的输出（词<TAB>全部义项）")
    p.add_argument("--out", required=True, help="data/comments-src/ 下的源数据 tsv")
    p.add_argument("--backend", choices=["api", "files"], default="api")
    p.add_argument("--batches-dir", help="files 后端的批次目录")
    p.add_argument("--limit", type=int, help="最多处理多少词（试跑用，仅 api）")
    p.add_argument("--batch-size", type=int, default=BATCH_SIZE)
    p.add_argument("--extra-rule", help="追加到提示词规则末尾的一条（fill/refine 通用）；纳入批次目录的输入哈希")
    a = p.parse_args(argv)
    if a.mode == "fill" and a.direction != "zh-en":
        p.error("fill 只支持 --direction zh-en")
    if a.backend == "files":
        if not a.batches_dir:
            p.error("files 后端需要 --batches-dir")
        backend = FileBackend(a.batches_dir)
    else:
        backend = ApiBackend(a.extra_rule)
    stats = run(read_items(a.input, a.mode), a.out, backend, a.mode, a.direction, a.batch_size, a.limit, extra_rule=a.extra_rule)
    print(
        f"写入 {stats['written']}（其中空释义 {stats['blank']}），拒收 {stats['rejected']}，"
        f"待产出批次 {stats['pending_batches']} -> {a.out}",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
