import { describe, it, expect } from "vitest";
import { searchIndex } from "./searchIndex";
import type { SearchEntry } from "@/schemas/searchEntry";

const KNOWN_TABS = new Set([
  "general",
  "input",
  "hotkey",
  "appearance",
  "dictionary",
  "advanced",
  "stats",
  "about",
]);

describe("searchIndex 一致性护栏", () => {
  it("非空", () => {
    expect(searchIndex.length).toBeGreaterThan(0);
  });

  it("id 全局唯一", () => {
    const ids = searchIndex.map((e) => e.id);
    const seen = new Set<string>();
    const dups = ids.filter((id) => seen.has(id) || !seen.add(id));
    expect(dups).toEqual([]);
  });

  it("每条形状合法：tab 属于已知集合、title/tabLabel 非空、anchor 或 openDialog 二选一非空", () => {
    const bad = searchIndex.filter(
      (e: SearchEntry) =>
        !KNOWN_TABS.has(e.tab) ||
        !e.title?.trim() ||
        (!e.anchor?.trim() && !e.openDialog) ||
        !e.tabLabel?.trim(),
    );
    expect(bad.map((e) => e.id)).toEqual([]);
  });
});
