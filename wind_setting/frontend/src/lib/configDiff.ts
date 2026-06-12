// 配置增量 diff：对 base(加载快照) 与 current(编辑中) 做 deep-diff，
// 只产出 current 相对 base 变化的叶子项，供按-key 提交（Config.Set）。
// 点路径约定与后端 resolveKeyPath/setNestedKey 对齐（如 input.auto_pair.chinese）。
//
// 规则：
// - 普通对象 → 递归进入（只遍历 current 键，base 有而 current 没有的顶层段不产出）；
// - 例外：若 base 比 current 多出子键（条目被删除，如 punct_custom.mappings 清空），
//   把该对象整体视为叶子提交，否则递归只遍历 current 键会漏报；
// - 数组 / 标量 → 视为叶子，JSON.stringify 比较，不等则整体提交 current 值；
// - 只遍历 current 实际拥有的字段，因此 base 有而 current 没有的段（如 formData
//   不管理的 stats）永不被产出 —— 这是独立段隔离、根治覆盖 bug 的关键。

export interface ConfigSetItem {
  key: string;
  value: any;
}

function isPlainObject(v: any): boolean {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

export function diffConfigToItems(
  base: any,
  current: any,
  prefix = "",
): ConfigSetItem[] {
  const items: ConfigSetItem[] = [];
  if (current == null || typeof current !== "object") return items;

  for (const k of Object.keys(current)) {
    const path = prefix ? `${prefix}.${k}` : k;
    const cv = current[k];
    const bv = base == null ? undefined : base[k];

    // base 比 current 多出子键 → 条目被删除（如 mappings 全部清空），整体视为叶子。
    // 安全前提：此启发式仅对"扁平动态 key map"正确（如 punct_custom.mappings: Record<string, string[]>）。
    // 对 struct 型嵌套对象不适用——formData 不一定暴露 base 的所有子键，误判会整体覆盖。
    // 若将来新增 struct 型嵌套对象且 formData 省略其部分字段，需在此处按路径豁免。
    const hasRemovedKeys =
      isPlainObject(cv) &&
      isPlainObject(bv) &&
      Object.keys(bv as object).some((bk) => !(bk in (cv as object)));

    if (isPlainObject(cv) && !hasRemovedKeys && (isPlainObject(bv) || bv === undefined)) {
      items.push(...diffConfigToItems(bv ?? null, cv, path));
    } else if (JSON.stringify(cv) !== JSON.stringify(bv)) {
      items.push({ key: path, value: cv });
    }
  }
  return items;
}
