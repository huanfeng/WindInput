import { describe, it, expect, vi } from "vitest";
import { ref } from "vue";
import { useSettingsSearch } from "./useSettingsSearch";
import type { SearchEntry } from "@/schemas/searchEntry";

function fakeEl() {
  return {
    scrollIntoView: vi.fn(),
    classList: { add: vi.fn(), remove: vi.fn() },
    getBoundingClientRect: () => ({ top: 100, bottom: 140, height: 40 }),
    offsetHeight: 40,
  };
}

const entry: SearchEntry = {
  id: "ui.candidate.font_size",
  tab: "appearance",
  tabLabel: "外观",
  card: "候选窗口",
  title: "字体大小",
  anchor: "ui.candidate.font_size",
};

describe("useSettingsSearch.jumpTo", () => {
  it("切换 activeTab，命中元素时滚动并加高亮类，返回 true", async () => {
    const el = fakeEl();
    const scrollTo = vi.fn();
    const activeTab = ref("general");
    const container = ref<any>({
      querySelector: vi.fn().mockReturnValue(el),
      getBoundingClientRect: () => ({ top: 0 }),
      scrollTop: 0,
      clientHeight: 500,
      scrollTo,
    });
    const { jumpTo } = useSettingsSearch({ activeTab, container });

    const ok = await jumpTo(entry);

    expect(activeTab.value).toBe("appearance");
    expect(container.value.querySelector).toHaveBeenCalledWith(
      '[data-search-anchor="ui.candidate.font_size"]',
    );
    expect(scrollTo).toHaveBeenCalled();
    expect(el.classList.add).toHaveBeenCalledWith("search-flash");
    expect(ok).toBe(true);
  });

  it("锚点不存在时仍切 tab，但返回 false 不报错", async () => {
    const scrollTo = vi.fn();
    const activeTab = ref("general");
    const container = ref<any>({
      querySelector: vi.fn().mockReturnValue(null),
      getBoundingClientRect: () => ({ top: 0 }),
      scrollTop: 0,
      clientHeight: 500,
      scrollTo,
    });
    const { jumpTo } = useSettingsSearch({ activeTab, container });

    const ok = await jumpTo(entry);

    expect(activeTab.value).toBe("appearance");
    expect(ok).toBe(false);
    expect(scrollTo).not.toHaveBeenCalled();
  });
});

describe("useSettingsSearch.jumpTo - openDialog 分支", () => {
  it("openDialog=schemaSettingsPinyin：切 tab + 以 'pinyin' 调用 onOpenSchemaSettings + 不查 DOM + 返回 true", async () => {
    const activeTab = ref("general");
    const querySelectorMock = vi.fn();
    const container = ref<any>({ querySelector: querySelectorMock });
    const onOpenSchemaSettings = vi.fn();
    const { jumpTo } = useSettingsSearch({
      activeTab,
      container,
      onOpenSchemaSettings,
    });

    const dialogEntry: SearchEntry = {
      id: "dialog.schema_settings_pinyin",
      tab: "general",
      tabLabel: "方案",
      card: "方案专属设置",
      title: "拼音方案设置",
      anchor: "",
      openDialog: "schemaSettingsPinyin",
    };

    const ok = await jumpTo(dialogEntry);

    expect(activeTab.value).toBe("general");
    expect(onOpenSchemaSettings).toHaveBeenCalledWith("pinyin");
    expect(querySelectorMock).not.toHaveBeenCalled();
    expect(ok).toBe(true);
  });

  it("openDialog=schemaSettingsCodetable：切 tab + 以 'codetable' 调用 onOpenSchemaSettings + 不查 DOM + 返回 true", async () => {
    const activeTab = ref("general");
    const querySelectorMock = vi.fn();
    const container = ref<any>({ querySelector: querySelectorMock });
    const onOpenSchemaSettings = vi.fn();
    const { jumpTo } = useSettingsSearch({
      activeTab,
      container,
      onOpenSchemaSettings,
    });

    const dialogEntry: SearchEntry = {
      id: "dialog.schema_settings_codetable",
      tab: "general",
      tabLabel: "方案",
      card: "方案专属设置",
      title: "码表方案设置",
      anchor: "",
      openDialog: "schemaSettingsCodetable",
    };

    const ok = await jumpTo(dialogEntry);

    expect(activeTab.value).toBe("general");
    expect(onOpenSchemaSettings).toHaveBeenCalledWith("codetable");
    expect(querySelectorMock).not.toHaveBeenCalled();
    expect(ok).toBe(true);
  });

  it("openDialog=importDict：切 tab + 以 'import' 调用 onOpenImportExport + 返回 true", async () => {
    const activeTab = ref("general");
    const container = ref<any>({ querySelector: vi.fn() });
    const onOpenImportExport = vi.fn();
    const { jumpTo } = useSettingsSearch({
      activeTab,
      container,
      onOpenImportExport,
    });

    const dialogEntry: SearchEntry = {
      id: "dialog.import_dict",
      tab: "dictionary",
      tabLabel: "词库",
      card: "词库管理",
      title: "导入词库",
      anchor: "",
      openDialog: "importDict",
    };

    const ok = await jumpTo(dialogEntry);

    expect(activeTab.value).toBe("dictionary");
    expect(onOpenImportExport).toHaveBeenCalledWith("import");
    expect(ok).toBe(true);
  });

  it("openDialog=exportDict：以 'export' 调用 onOpenImportExport + 返回 true", async () => {
    const activeTab = ref("general");
    const container = ref<any>({ querySelector: vi.fn() });
    const onOpenImportExport = vi.fn();
    const { jumpTo } = useSettingsSearch({
      activeTab,
      container,
      onOpenImportExport,
    });

    const dialogEntry: SearchEntry = {
      id: "dialog.export_dict",
      tab: "dictionary",
      tabLabel: "词库",
      card: "词库管理",
      title: "导出词库",
      anchor: "",
      openDialog: "exportDict",
    };

    const ok = await jumpTo(dialogEntry);

    expect(activeTab.value).toBe("dictionary");
    expect(onOpenImportExport).toHaveBeenCalledWith("export");
    expect(ok).toBe(true);
  });
});
