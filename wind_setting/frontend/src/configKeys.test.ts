// 配置 key 一致性测试：断言所有 schema 中的 key 都在 Go 反射导出的合法 key 清单中
import { describe, it, expect } from "vitest";
import configKeys from "./generated/config-keys.json";

import {
  punctSchema,
  keyBehaviorSchema,
  overflowSchema,
  quickInputExtraSchema,
  pinyinSeparatorSchema,
  shiftExtraSchema,
  startupExtraSchema,
  themeExtraSchema,
  candidateWindowSchema,
  statusIndicatorSchema,
  toolbarSchema,
  advancedLogSchema,
  advancedPerfSchema,
} from "./schemas";
import { engineSchema } from "./schemas/engine.schema";
import { candidateTooltipSchema, indicatorSchema } from "./schemas/appearance.schema";
import type { FieldDef, PageSchema } from "./schemas/types";
import type { EngineSchema } from "./schemas/schema-engine-types";

const validKeys = new Set<string>(configKeys as string[]);

/** 从 PageSchema 中收集所有带 key 的字段的 key 值 */
function collectKeys(schema: PageSchema): string[] {
  return schema
    .filter((f): f is Extract<FieldDef, { key: string }> => "key" in f)
    .map((f) => f.key);
}

/** 从 EngineSchema 中收集所有带 key 的字段的 key 值 */
function collectEngineKeys(schema: EngineSchema): string[] {
  return schema
    .filter((f): f is Extract<typeof schema[number], { key: string }> => "key" in f)
    .map((f) => f.key);
}

const allPageSchemas: Array<[string, PageSchema]> = [
  ["punctSchema", punctSchema],
  ["keyBehaviorSchema", keyBehaviorSchema],
  ["overflowSchema", overflowSchema],
  ["quickInputExtraSchema", quickInputExtraSchema],
  ["pinyinSeparatorSchema", pinyinSeparatorSchema],
  ["shiftExtraSchema", shiftExtraSchema],
  ["startupExtraSchema", startupExtraSchema],
  ["themeExtraSchema", themeExtraSchema],
  ["candidateWindowSchema", candidateWindowSchema],
  ["statusIndicatorSchema", statusIndicatorSchema],
  ["toolbarSchema", toolbarSchema],
  ["indicatorSchema", indicatorSchema],
  ["candidateTooltipSchema", candidateTooltipSchema],
  ["advancedLogSchema", advancedLogSchema],
  ["advancedPerfSchema", advancedPerfSchema],
];

describe("configKeys 一致性", () => {
  it("config-keys.json 非空", () => {
    expect(validKeys.size).toBeGreaterThan(0);
  });

  for (const [name, schema] of allPageSchemas) {
    it(`${name} 中每个 key 都在 config-keys.json 中`, () => {
      const keys = collectKeys(schema);
      const invalid = keys.filter((k) => !validKeys.has(k));
      expect(invalid, `${name} 含非法 key: ${invalid.join(", ")}`).toEqual([]);
    });
  }

  it("engineSchema 中每个 key 都在 config-keys.json 中（engine.* / learning.* 路径）", () => {
    const keys = collectEngineKeys(engineSchema);
    // engine.* / learning.* 路径不在顶层 Config key 清单中，属于方案级配置；
    // 仅校验非 engine/learning 前缀的 key（如有误迁）
    const nonEngineKeys = keys.filter(
      (k) => !k.startsWith("engine.") && !k.startsWith("learning."),
    );
    const invalid = nonEngineKeys.filter((k) => !validKeys.has(k));
    expect(invalid, `engineSchema 含非法非引擎 key: ${invalid.join(", ")}`).toEqual([]);
  });
});
