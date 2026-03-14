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

// 词库统计类型
export interface DictStats {
  word_count: number;
  phrase_count: number;
  shadow_count: number;
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
  };
  toolbar: {
    background_color: string;
    border_color: string;
    mode_chinese_bg_color: string;
    mode_english_bg_color: string;
    full_width_on_bg_color: string;
    punct_chinese_bg_color: string;
  };
  style?: {
    index_style: string;
    accent_bar_color: string;
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

export async function savePhrases(items: PhraseItem[]): Promise<void> {
  return App.SavePhrases(items);
}

export async function addPhrase(
  code: string,
  text: string,
  weight: number = 0,
): Promise<void> {
  return App.AddPhrase(code, text, weight);
}

export async function removePhrase(code: string, text: string): Promise<void> {
  return App.RemovePhrase(code, text);
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

export async function getUserDictEngineType(): Promise<string> {
  return App.GetUserDictEngineType();
}

export async function switchUserDictEngine(engineType: string): Promise<void> {
  return App.SwitchUserDictEngine(engineType);
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

// Shadow 管理
export async function getShadowRules(): Promise<ShadowRuleItem[]> {
  return App.GetShadowRules();
}

export async function addShadowRule(
  code: string,
  word: string,
  action: string,
  weight: number = 0,
): Promise<void> {
  return App.AddShadowRule(code, word, action, weight);
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
): Promise<ThemePreview> {
  const preview = await App.GetThemePreview(themeName);
  return preview as unknown as ThemePreview;
}

// 启动页面
export async function getStartPage(): Promise<string> {
  return App.GetStartPage();
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
    dictionary: {
      system_dict: "dict/wubi/wubi86.txt",
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
      position_x: 0,
      position_y: 0,
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
