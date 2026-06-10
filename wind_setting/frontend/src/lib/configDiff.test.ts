import { describe, it, expect } from "vitest";
import { diffConfigToItems } from "./configDiff";

describe("diffConfigToItems", () => {
  it("无变化时返回空数组", () => {
    const base = { ui: { font_size: 18 }, toolbar: { visible: true } };
    const cur = { ui: { font_size: 18 }, toolbar: { visible: true } };
    expect(diffConfigToItems(base, cur)).toEqual([]);
  });

  it("标量改动产出点路径 key", () => {
    const base = { ui: { candidate: { font_size: 18 } } };
    const cur = { ui: { candidate: { font_size: 22 } } };
    expect(diffConfigToItems(base, cur)).toEqual([
      { key: "ui.candidate.font_size", value: 22 },
    ]);
  });

  it("嵌套对象只产出改动的叶子", () => {
    const base = { input: { auto_pair: { chinese: true, english: true } } };
    const cur = { input: { auto_pair: { chinese: false, english: true } } };
    expect(diffConfigToItems(base, cur)).toEqual([
      { key: "input.auto_pair.chinese", value: false },
    ]);
  });

  it("数组整体作为一个叶子提交", () => {
    const base = { schema: { available: ["wubi86"] } };
    const cur = { schema: { available: ["wubi86", "pinyin"] } };
    expect(diffConfigToItems(base, cur)).toEqual([
      { key: "schema.available", value: ["wubi86", "pinyin"] },
    ]);
  });

  it("current 比 base 多出的字段会被产出", () => {
    const base = { ui: { theme: {} as Record<string, unknown> } };
    const cur = { ui: { theme: { name: "msime" } } };
    expect(diffConfigToItems(base, cur)).toEqual([
      { key: "ui.theme.name", value: "msime" },
    ]);
  });

  it("base 为 null 时整份 current 作为变化产出", () => {
    const cur = { ui: { candidate: { font_size: 16 } } };
    expect(diffConfigToItems(null, cur)).toEqual([
      { key: "ui.candidate.font_size", value: 16 },
    ]);
  });

  it("不会产出 base 有而 current 没有的字段（formData 不管理的段被忽略）", () => {
    const base = { ui: { candidate: { font_size: 18 } }, features: { stats: { track_english: false } } };
    const cur = { ui: { candidate: { font_size: 18 } } };
    expect(diffConfigToItems(base, cur)).toEqual([]);
  });

  it("features.stats 段改动产出 features.stats.* 点路径 key", () => {
    const base = { features: { stats: { enabled: true, retain_days: 0, track_english: true } } };
    const cur = { features: { stats: { enabled: true, retain_days: 0, track_english: false } } };
    expect(diffConfigToItems(base, cur)).toEqual([
      { key: "features.stats.track_english", value: false },
    ]);
  });

  it("features.stats 段多字段同改产出多条 item", () => {
    const base = { features: { stats: { enabled: true, retain_days: 0, track_english: true } } };
    const cur = { features: { stats: { enabled: false, retain_days: 0, track_english: false } } };
    expect(diffConfigToItems(base, cur)).toEqual([
      { key: "features.stats.enabled", value: false },
      { key: "features.stats.track_english", value: false },
    ]);
  });
});
