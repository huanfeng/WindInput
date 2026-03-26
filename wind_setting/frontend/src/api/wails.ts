// Wails API 封装层 - 使用 Wails 绑定调用 Go 后端

// 导入 Wails 生成的绑定和模型
import * as App from "../../wailsjs/go/main/App";
import { main, config, control } from "../../wailsjs/go/models";

// 重新导出类型
export type Config = config.Config;
export type PhraseItem = main.PhraseItem;
export type UserWordItem = main.UserWordItem;
export type ShadowRuleItem = main.ShadowRuleItem;
export type FileChangeStatus = main.FileChangeStatus;
export type ServiceStatus = control.ServiceStatus;
export type ThemeInfo = main.ThemeInfo;
export type SchemaInfo = main.SchemaInfo;
export type SchemaConfig = main.SchemaConfig;

// ===== Schema API =====

export async function getAvailableSchemas(): Promise<SchemaInfo[]> {
  return App.GetAvailableSchemas();
}

export async function getSchemaConfig(schemaID: string): Promise<SchemaConfig> {
  return App.GetSchemaConfig(schemaID);
}

export async function saveSchemaConfig(
  schemaID: string,
  cfg: SchemaConfig,
): Promise<void> {
  return App.SaveSchemaConfig(schemaID, cfg);
}

export async function switchActiveSchema(schemaID: string): Promise<void> {
  return App.SwitchActiveSchema(schemaID);
}

// 词库统计类型
export interface DictStats {
  word_count: number;
  phrase_count: number;
  shadow_count: number;
}

// 方案词库统计类型
export interface SchemaDictStatsItem {
  schema_id: string;
  schema_name: string;
  icon_label: string;
  word_count: number;
  shadow_count: number;
  temp_word_count: number;
}

// 主题预览数据类型
export interface ThemePreview {
  meta: {
    name: string;
    version: string;
    author: string;
  };
  candidate_window: {
    background_color: string;
    border_color: string;
    text_color: string;
    index_color: string;
    index_bg_color: string;
    hover_bg_color: string;
    selected_bg_color: string;
    input_bg_color: string;
    input_text_color: string;
    comment_color: string;
    shadow_color: string;
  };
  toolbar: {
    background_color: string;
    border_color: string;
    grip_color: string;
    mode_chinese_bg_color: string;
    mode_english_bg_color: string;
    mode_text_color: string;
    full_width_on_bg_color: string;
    full_width_off_bg_color: string;
    full_width_on_color: string;
    full_width_off_color: string;
    punct_chinese_bg_color: string;
    punct_english_bg_color: string;
    punct_chinese_color: string;
    punct_english_color: string;
    settings_bg_color: string;
    settings_icon_color: string;
  };
  style?: {
    index_style: string;
    accent_bar_color: string;
  };
  is_dark?: {
    active: boolean;
  };
}

// 配置管理
export async function getConfig(): Promise<Config> {
  return App.GetConfig();
}

export async function saveConfig(cfg: Config): Promise<void> {
  return App.SaveConfig(cfg);
}

export async function checkConfigModified(): Promise<boolean> {
  return App.CheckConfigModified();
}

export async function reloadConfig(): Promise<void> {
  return App.ReloadConfig();
}

// 短语管理
export async function getPhrases(): Promise<PhraseItem[]> {
  return App.GetPhrases();
}

// 系统短语管理
export async function getSystemPhrases(): Promise<PhraseItem[]> {
  return App.GetSystemPhrases();
}

export async function savePhrases(items: PhraseItem[]): Promise<void> {
  return App.SavePhrases(items);
}

export async function addPhrase(
  code: string,
  text: string,
  position: number = 1,
): Promise<void> {
  return App.AddPhrase(code, text, position);
}

export async function removePhrase(code: string, text: string): Promise<void> {
  return App.RemovePhrase(code, text);
}

export async function updatePhrase(
  oldCode: string,
  oldText: string,
  newCode: string,
  newText: string,
  newPosition: number,
): Promise<void> {
  return App.UpdatePhrase(oldCode, oldText, newCode, newText, newPosition);
}

export async function overrideSystemPhrase(
  code: string,
  text: string,
  position: number,
  disabled: boolean,
): Promise<void> {
  return App.OverrideSystemPhrase(code, text, position, disabled);
}

export async function removeSystemPhraseOverride(
  code: string,
  text: string,
): Promise<void> {
  return App.RemoveSystemPhraseOverride(code, text);
}

export async function checkPhrasesModified(): Promise<boolean> {
  return App.CheckPhrasesModified();
}

export async function reloadPhrases(): Promise<void> {
  return App.ReloadPhrases();
}

// 用户词库管理
export async function getUserDict(): Promise<UserWordItem[]> {
  return App.GetUserDict();
}

export async function addUserWord(
  code: string,
  text: string,
  weight: number = 0,
): Promise<void> {
  return App.AddUserWord(code, text, weight);
}

export async function removeUserWord(
  code: string,
  text: string,
): Promise<void> {
  return App.RemoveUserWord(code, text);
}

export async function searchUserDict(
  query: string,
  limit: number = 100,
): Promise<UserWordItem[]> {
  return App.SearchUserDict(query, limit);
}

export async function getUserDictStats(): Promise<DictStats> {
  const stats = await App.GetUserDictStats();
  return {
    word_count: stats["word_count"] || 0,
    phrase_count: stats["phrase_count"] || 0,
    shadow_count: stats["shadow_count"] || 0,
  };
}

export async function checkUserDictModified(): Promise<boolean> {
  return App.CheckUserDictModified();
}

export async function reloadUserDict(): Promise<void> {
  return App.ReloadUserDict();
}

export async function getUserDictSchemaID(): Promise<string> {
  return App.GetUserDictSchemaID();
}

export async function switchUserDictSchema(schemaID: string): Promise<void> {
  return App.SwitchUserDictSchema(schemaID);
}

// 导入导出结果类型
export interface ImportExportResult {
  cancelled: boolean;
  count: number;
  total?: number;
  path?: string;
}

export async function importUserDict(): Promise<ImportExportResult> {
  return App.ImportUserDict() as unknown as ImportExportResult;
}

export async function exportUserDict(): Promise<ImportExportResult> {
  return App.ExportUserDict() as unknown as ImportExportResult;
}

// ===== 按方案操作词库 =====

export async function getEnabledSchemasWithDictStats(): Promise<
  SchemaDictStatsItem[]
> {
  return App.GetEnabledSchemasWithDictStats() as unknown as SchemaDictStatsItem[];
}

export async function getUserDictBySchema(
  schemaID: string,
): Promise<UserWordItem[]> {
  return App.GetUserDictBySchema(schemaID);
}

export async function addUserWordForSchema(
  schemaID: string,
  code: string,
  text: string,
  weight: number = 0,
): Promise<void> {
  return App.AddUserWordForSchema(schemaID, code, text, weight);
}

export async function removeUserWordForSchema(
  schemaID: string,
  code: string,
  text: string,
): Promise<void> {
  return App.RemoveUserWordForSchema(schemaID, code, text);
}

export async function searchUserDictBySchema(
  schemaID: string,
  query: string,
  limit: number = 100,
): Promise<UserWordItem[]> {
  return App.SearchUserDictBySchema(schemaID, query, limit);
}

export async function importUserDictForSchema(
  schemaID: string,
): Promise<ImportExportResult> {
  return App.ImportUserDictForSchema(schemaID) as unknown as ImportExportResult;
}

export async function exportUserDictForSchema(
  schemaID: string,
): Promise<ImportExportResult> {
  return App.ExportUserDictForSchema(schemaID) as unknown as ImportExportResult;
}

// ===== 临时词库管理 =====

export interface TempWordItem {
  code: string;
  text: string;
  weight: number;
  count: number;
}

export async function getTempDictBySchema(schemaID: string): Promise<TempWordItem[]> {
  return App.GetTempDictBySchema(schemaID) as unknown as TempWordItem[];
}

export async function clearTempDictForSchema(schemaID: string): Promise<number> {
  return App.ClearTempDictForSchema(schemaID);
}

export async function promoteTempWordForSchema(schemaID: string, code: string, text: string): Promise<void> {
  return App.PromoteTempWordForSchema(schemaID, code, text);
}

export async function promoteAllTempWordsForSchema(schemaID: string): Promise<number> {
  return App.PromoteAllTempWordsForSchema(schemaID);
}

export async function removeTempWordForSchema(schemaID: string, code: string, text: string): Promise<void> {
  return App.RemoveTempWordForSchema(schemaID, code, text);
}

export async function getShadowBySchema(
  schemaID: string,
): Promise<ShadowRuleItem[]> {
  return App.GetShadowBySchema(schemaID);
}

export async function pinShadowWordForSchema(
  schemaID: string,
  code: string,
  word: string,
  position: number,
): Promise<void> {
  return App.PinShadowWordForSchema(schemaID, code, word, position);
}

export async function deleteShadowWordForSchema(
  schemaID: string,
  code: string,
  word: string,
): Promise<void> {
  return App.DeleteShadowWordForSchema(schemaID, code, word);
}

export async function removeShadowRuleForSchema(
  schemaID: string,
  code: string,
  word: string,
): Promise<void> {
  return App.RemoveShadowRuleForSchema(schemaID, code, word);
}

// Shadow 管理（旧接口保留）
export async function getShadowRules(): Promise<ShadowRuleItem[]> {
  return App.GetShadowRules();
}

export async function pinShadowWord(
  code: string,
  word: string,
  position: number,
): Promise<void> {
  return App.PinShadowWord(code, word, position);
}

export async function deleteShadowWord(
  code: string,
  word: string,
): Promise<void> {
  return App.DeleteShadowWord(code, word);
}

export async function removeShadowRule(
  code: string,
  word: string,
): Promise<void> {
  return App.RemoveShadowRule(code, word);
}

// 控制管道
export async function checkServiceRunning(): Promise<boolean> {
  return App.CheckServiceRunning();
}

export async function notifyReload(target: string): Promise<void> {
  return App.NotifyReload(target);
}

export async function openLogFolder(): Promise<void> {
  return App.OpenLogFolder();
}

export async function openConfigFolder(): Promise<void> {
  return App.OpenConfigFolder();
}

export async function openExternalURL(url: string): Promise<void> {
  return App.OpenExternalURL(url);
}

export async function getServiceStatus(): Promise<ServiceStatus | null> {
  return App.GetServiceStatus();
}

// 文件变化检测
export async function checkAllFilesModified(): Promise<FileChangeStatus> {
  return App.CheckAllFilesModified();
}

export async function reloadAllFiles(): Promise<void> {
  return App.ReloadAllFiles();
}

// 主题管理
export async function getAvailableThemes(): Promise<ThemeInfo[]> {
  return App.GetAvailableThemes();
}

export async function getThemePreview(
  themeName: string,
  themeStyle: string = "system",
): Promise<ThemePreview> {
  const preview = await App.GetThemePreview(themeName, themeStyle);
  return preview as unknown as ThemePreview;
}

// 启动页面
export async function getStartPage(): Promise<string> {
  return App.GetStartPage();
}

// 版本号
export async function getVersion(): Promise<string> {
  return App.GetVersion();
}

// 默认配置
export function getDefaultConfig(): Config {
  return new config.Config({
    startup: {
      remember_last_state: false,
      default_chinese_mode: true,
      default_full_width: false,
      default_chinese_punct: true,
    },
    schema: {
      active: "wubi86",
      available: ["wubi86", "pinyin"],
    },
    dictionary: {
      system_dict: "dict/wubi86/wubi86.txt",
      pinyin_user_dict: "pinyin_user_words.txt",
      wubi_user_dict: "wubi_user_words.txt",
      pinyin_dict: "dict/pinyin",
    },
    engine: {
      type: "wubi",
      filter_mode: "smart",
      pinyin: {
        show_wubi_hint: true,
        fuzzy: {
          enabled: false,
          zh_z: false,
          ch_c: false,
          sh_s: false,
          n_l: false,
          f_h: false,
          r_l: false,
          an_ang: false,
          en_eng: false,
          in_ing: false,
          ian_iang: false,
          uan_uang: false,
        },
      },
      wubi: {
        auto_commit_at_4: false,
        clear_on_empty_at_4: false,
        top_code_commit: false,
        punct_commit: true,
        candidate_sort_mode: "frequency",
      },
    },
    hotkeys: {
      toggle_mode_keys: ["lshift", "rshift"],
      commit_on_switch: true,
      switch_engine: "ctrl+`",
      toggle_full_width: "shift+space",
      toggle_punct: "ctrl+.",
    },
    ui: {
      font_size: 18,
      candidates_per_page: 7,
      font_path: "",
      inline_preedit: true,
      hide_candidate_window: false,
      candidate_layout: "horizontal",
      status_indicator_duration: 800,
      status_indicator_offset_x: 0,
      status_indicator_offset_y: 0,
      theme: "default",
    },
    toolbar: {
      visible: true,
    },
    input: {
      punct_follow_mode: false,
      select_key_groups: ["semicolon_quote"],
      page_keys: ["pageupdown", "minus_equal"],
    },
    advanced: {
      log_level: "info",
    },
  });
}
