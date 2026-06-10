import { describe, it, expect } from "vitest";
import {
  filterEntries,
  schemaToEntries,
  type SearchEntry,
} from "./searchEntry";
import type { PageSchema } from "./types";

const sample: SearchEntry[] = [
  {
    id: "input.enter_behavior",
    tab: "input",
    tabLabel: "输入",
    card: "按键行为",
    title: "回车键功能",
    hint: "有编码时按回车键的处理方式",
    options: ["上屏编码", "清空编码"],
    anchor: "input.enter_behavior",
  },
  {
    id: "ui.candidate.font_size",
    tab: "appearance",
    tabLabel: "外观",
    card: "候选窗口",
    title: "字体大小",
    anchor: "ui.candidate.font_size",
    keywords: ["fontsize"],
  },
];

describe("filterEntries", () => {
  it("空查询返回空数组", () => {
    expect(filterEntries(sample, "")).toEqual([]);
    expect(filterEntries(sample, "   ")).toEqual([]);
  });

  it("命中标题子串", () => {
    expect(filterEntries(sample, "回车").map((e) => e.id)).toEqual([
      "input.enter_behavior",
    ]);
  });

  it("命中 hint", () => {
    expect(filterEntries(sample, "处理方式").map((e) => e.id)).toEqual([
      "input.enter_behavior",
    ]);
  });

  it("命中选项标签", () => {
    expect(filterEntries(sample, "清空编码").map((e) => e.id)).toEqual([
      "input.enter_behavior",
    ]);
  });

  it("命中 keywords，且大小写不敏感", () => {
    expect(filterEntries(sample, "FontSize").map((e) => e.id)).toEqual([
      "ui.candidate.font_size",
    ]);
  });

  it("命中卡片名 card", () => {
    expect(filterEntries(sample, "按键行为").map((e) => e.id)).toEqual(["input.enter_behavior"]);
  });

  it("相关性排序：title 命中 > card 命中 > hint 命中", () => {
    const list: SearchEntry[] = [
      {
        id: "a",
        tab: "x",
        tabLabel: "X",
        card: "c",
        title: "数据备份与还原",
        hint: "含统计数据",
        anchor: "a",
      },
      {
        id: "b",
        tab: "x",
        tabLabel: "X",
        card: "c",
        title: "启用输入统计",
        anchor: "b",
      },
      {
        id: "c",
        tab: "x",
        tabLabel: "X",
        card: "统计设置",
        title: "其它项",
        anchor: "c",
      },
    ];
    // 查询"统计"：b 标题命中(80) > c 卡片命中(50) > a 仅 hint 命中(30)
    expect(filterEntries(list, "统计").map((e) => e.id)).toEqual([
      "b",
      "c",
      "a",
    ]);
  });

  it("无匹配返回空", () => {
    expect(filterEntries(sample, "不存在的词")).toEqual([]);
  });
});

const frag: PageSchema = [
  { type: "section", label: "分组标题" },
  {
    type: "toggle",
    key: "input.punct_follow_mode",
    label: "标点随中英文切换",
    hint: "切换到中文模式时自动切换中文标点",
  },
  {
    type: "select",
    key: "input.enter_behavior",
    label: "回车键功能",
    hint: "有编码时按回车键的处理方式",
    options: [
      { value: "commit", label: "上屏编码" },
      { value: "clear", label: "清空编码" },
    ],
  },
];

describe("schemaToEntries", () => {
  const ctx = { tab: "input", tabLabel: "输入", card: "按键行为" };

  it("跳过 card/section 标记，只产出叶子字段", () => {
    const out = schemaToEntries(frag, ctx);
    expect(out.map((e) => e.id)).toEqual([
      "input.punct_follow_mode",
      "input.enter_behavior",
    ]);
  });

  it("anchor/id 取 key，注入 ctx 上下文", () => {
    const [first] = schemaToEntries(frag, ctx);
    expect(first).toMatchObject({
      id: "input.punct_follow_mode",
      anchor: "input.punct_follow_mode",
      tab: "input",
      tabLabel: "输入",
      card: "按键行为",
      title: "标点随中英文切换",
      hint: "切换到中文模式时自动切换中文标点",
    });
  });

  it("select 字段收集选项标签", () => {
    const sel = schemaToEntries(frag, ctx).find(
      (e) => e.id === "input.enter_behavior",
    );
    expect(sel?.options).toEqual(["上屏编码", "清空编码"]);
  });

  it("函数式 hint 不写入（仅保留静态字符串）", () => {
    const dyn: PageSchema = [
      { type: "toggle", key: "x.y", label: "L", hint: () => "动态" },
    ];
    expect(schemaToEntries(dyn, ctx)[0].hint).toBeUndefined();
  });
});
