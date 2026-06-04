import { nextTick, type Ref } from "vue";
import type { SearchEntry } from "@/schemas/searchEntry";

interface UseSettingsSearchOptions {
  /** 当前激活标签页 */
  activeTab: Ref<string>;
  /** 内容滚动容器（App.vue 的 contentRef） */
  container: Ref<HTMLElement | null>;
  onOpenSchemaSettings?: (engine: "pinyin" | "codetable") => void;
  onOpenImportExport?: (mode: "import" | "export") => void;
}

const FLASH_CLASS = "search-flash";
const FLASH_MS = 1500;

export function useSettingsSearch(opts: UseSettingsSearchOptions) {
  /** 跳转到某设置项：切 tab → 等渲染 → 滚动 → 闪烁高亮。返回是否命中锚点 */
  async function jumpTo(entry: SearchEntry): Promise<boolean> {
    opts.activeTab.value = entry.tab;
    await nextTick();
    if (entry.openDialog) {
      if (entry.openDialog === "schemaSettingsPinyin")
        opts.onOpenSchemaSettings?.("pinyin");
      else if (entry.openDialog === "schemaSettingsCodetable")
        opts.onOpenSchemaSettings?.("codetable");
      else if (entry.openDialog === "importDict")
        opts.onOpenImportExport?.("import");
      else if (entry.openDialog === "exportDict")
        opts.onOpenImportExport?.("export");
      return true;
    }
    const el = opts.container.value?.querySelector<HTMLElement>(
      `[data-search-anchor="${entry.anchor}"]`,
    );
    if (!el) return false;
    const scroller = opts.container.value;
    if (scroller) {
      // 只滚动内容容器本身，避免 scrollIntoView 连带滚动 window/body
      // （高内容页如统计页会触发双重滚动导致定位错乱）。把目标在容器视口内居中。
      const elRect = el.getBoundingClientRect();
      const scRect = scroller.getBoundingClientRect();
      const target =
        elRect.top -
        scRect.top +
        scroller.scrollTop -
        scroller.clientHeight / 2 +
        el.offsetHeight / 2;
      scroller.scrollTo({ top: Math.max(0, target), behavior: "smooth" });
    } else {
      el.scrollIntoView({ block: "center", behavior: "smooth" });
    }
    el.classList.add(FLASH_CLASS);
    setTimeout(() => el.classList.remove(FLASH_CLASS), FLASH_MS);
    return true;
  }

  return { jumpTo };
}
