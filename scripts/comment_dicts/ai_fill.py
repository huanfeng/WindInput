#!/usr/bin/env python3
"""用 Claude API 补齐 CC-CEDICT 没覆盖的中→英释义，追加写入 zh_en_ai.tsv，可断点续跑。

输入是 `build.py missing` 的输出（每行一词）。每批 200 词，输出 `词<TAB>词性. 英文`；
没有可靠释义的词输出空释义（记空行，下次不再重试）。需要环境变量 ANTHROPIC_API_KEY。
"""

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
from datetime import date

MODEL = "claude-sonnet-5-5"
BATCH_SIZE = 200
API_URL = "https://api.anthropic.com/v1/messages"

PROMPT = """为下列中文词给出简短的英文释义，供输入法候选旁的注释使用。
每词输出一行：词<TAB>词性. 英文释义
- 词性用 n./v./adj./adv./prep./conj./pron./num./mw./int. 之一，取最常用的一个
- 英文释义取最基本的一到两个义项，用分号隔开，尽量短
- 没有把握的生僻词、专名、不成词的片段，只输出「词<TAB>」，不要硬凑
- 只输出结果行，不要解释，不要编号，词与输入完全一致、顺序一致

词表：
{words}
"""


def batches(items, size):
    for i in range(0, len(items), size):
        yield items[i : i + size]


def read_lines(path):
    with open(path, encoding="utf-8") as f:
        return [l.strip() for l in f if l.strip()]


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
    """按批内词序返回 [(词, 释义)]；应答里缺的词记空释义，批外的词丢弃。"""
    got = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("```"):
            continue
        cols = line.split("\t")
        word = cols[0].strip()
        if word in batch:
            got.setdefault(word, cols[1].strip() if len(cols) > 1 else "")
    return [(w, got.get(w, "")) for w in batch]


def header(today):
    return (
        f"# 中→英释义，由 AI（{MODEL}）生成，生成日期 {today}。\n"
        "# 补 CC-CEDICT 没覆盖的词；格式 词<TAB>词性. 英文；空释义行表示没有可靠释义。\n"
    )


def call_claude(batch):
    key = os.environ.get("ANTHROPIC_API_KEY")
    if not key:
        sys.exit("缺少环境变量 ANTHROPIC_API_KEY")
    body = json.dumps(
        {
            "model": MODEL,
            "max_tokens": 16000,
            "messages": [{"role": "user", "content": PROMPT.format(words="\n".join(batch))}],
        }
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


def run(missing, out, batch_size=BATCH_SIZE, limit=None, call=call_claude, today=None):
    """处理 missing 里还没出现在 out 的词，最多 limit 个；每批写完即落盘，中断后重跑接着来。"""
    done = done_words(out)
    todo = [w for w in read_lines(missing) if w not in done]
    if limit is not None:
        todo = todo[:limit]
    if not todo:
        return 0
    new_file = not os.path.exists(out) or os.path.getsize(out) == 0
    with open(out, "a", encoding="utf-8", newline="\n") as f:
        if new_file:
            f.write(header(today or date.today().isoformat()))
        finished = 0
        for batch in batches(todo, batch_size):
            for word, meaning in parse_response(call(batch), batch):
                f.write(f"{word}\t{meaning}\n")
            f.flush()
            finished += len(batch)
            print(f"已处理 {finished}/{len(todo)}", file=sys.stderr)
    return len(todo)


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--missing", required=True, help="build.py missing 的输出")
    p.add_argument("--out", required=True, help="data/comments-src/zh_en_ai.tsv")
    p.add_argument("--limit", type=int, help="最多处理多少词（试跑用）")
    p.add_argument("--batch-size", type=int, default=BATCH_SIZE)
    a = p.parse_args(argv)
    n = run(a.missing, a.out, a.batch_size, a.limit)
    print(f"完成 {n} 词 -> {a.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
