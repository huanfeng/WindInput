// Schema 类型定义 — 全局 Config 驱动的 FieldDef 体系
//
// 用法:
//   import type { PageSchema, FieldDef } from '@/schemas/types'
//
// 渲染器通过 getPath/setPath 操作 Config 对象，无需手写绑定。

import type { Config } from "@/api/settings";

type CfgFn<T> = (cfg: Config) => T;

// ── 布局标记 ──────────────────────────────────────────────────

export interface CardDef {
  type: "card";
  label: string;
}

export interface SectionDef {
  type: "section";
  label: string;
}

// ── 基础字段属性 ──────────────────────────────────────────────

interface BaseField {
  /** lodash 风格路径，如 "input.filter_mode"、"ui.status_indicator.enabled" */
  key: string;
  label: string;
  hint?: string | CfgFn<string>;
  /** 返回 false 时控件灰显（disabled），仍然渲染 */
  dependsOn?: CfgFn<boolean>;
  /** 返回 true 时整行隐藏（v-if） */
  hidden?: CfgFn<boolean>;
}

// ── 具体字段类型 ──────────────────────────────────────────────

export interface ToggleField extends BaseField {
  type: "toggle";
}

export interface SelectOption {
  value: string;
  label: string;
  description?: string;
  tag?: string;
}

export interface SelectField extends BaseField {
  type: "select";
  options: SelectOption[];
  /** 控件宽度 CSS 值，默认 '160px' */
  width?: string;
}

export interface SliderField extends BaseField {
  type: "slider";
  min: number;
  max: number;
  step: number;
  unit?: string;
  /** 自定义显示值，如透明度转百分比 */
  displayValue?: (v: number) => string;
}

export interface NumberInputField extends BaseField {
  type: "number-input";
  min?: number;
  max?: number;
  step?: number;
}

// ── 联合类型 ──────────────────────────────────────────────────

export type FieldDef =
  | CardDef
  | SectionDef
  | ToggleField
  | SelectField
  | SliderField
  | NumberInputField;

export type PageSchema = FieldDef[];

// ── 工具函数 ──────────────────────────────────────────────────

/** 从对象中按点分路径读取值 */
export function getPath(obj: any, path: string): any {
  return path.split(".").reduce((cur, key) => cur?.[key], obj);
}

/** 向对象按点分路径写入值（直接 mutation，适配 Vue reactive 对象） */
export function setPath(obj: any, path: string, value: any): void {
  const parts = path.split(".");
  let cur = obj;
  for (let i = 0; i < parts.length - 1; i++) {
    if (cur[parts[i]] == null) cur[parts[i]] = {};
    cur = cur[parts[i]];
  }
  cur[parts[parts.length - 1]] = value;
}
