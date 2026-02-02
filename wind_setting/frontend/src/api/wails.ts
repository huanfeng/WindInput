// Wails API 封装层 - 使用 Wails 绑定调用 Go 后端

// 导入 Wails 生成的绑定和模型
import * as App from '../../wailsjs/go/main/App';
import { main, config, control } from '../../wailsjs/go/models';

// 重新导出类型
export type Config = config.Config;
export type PhraseItem = main.PhraseItem;
export type UserWordItem = main.UserWordItem;
export type ShadowRuleItem = main.ShadowRuleItem;
export type FileChangeStatus = main.FileChangeStatus;
export type ServiceStatus = control.ServiceStatus;

// 词库统计类型
export interface DictStats {
  word_count: number;
  phrase_count: number;
  shadow_count: number;
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

export async function addPhrase(code: string, text: string, weight: number = 0): Promise<void> {
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

export async function addUserWord(code: string, text: string, weight: number = 0): Promise<void> {
  return App.AddUserWord(code, text, weight);
}

export async function removeUserWord(code: string, text: string): Promise<void> {
  return App.RemoveUserWord(code, text);
}

export async function searchUserDict(query: string, limit: number = 100): Promise<UserWordItem[]> {
  return App.SearchUserDict(query, limit);
}

export async function getUserDictStats(): Promise<DictStats> {
  const stats = await App.GetUserDictStats();
  return {
    word_count: stats['word_count'] || 0,
    phrase_count: stats['phrase_count'] || 0,
    shadow_count: stats['shadow_count'] || 0,
  };
}

export async function checkUserDictModified(): Promise<boolean> {
  return App.CheckUserDictModified();
}

export async function reloadUserDict(): Promise<void> {
  return App.ReloadUserDict();
}

// Shadow 管理
export async function getShadowRules(): Promise<ShadowRuleItem[]> {
  return App.GetShadowRules();
}

export async function addShadowRule(code: string, word: string, action: string, weight: number = 0): Promise<void> {
  return App.AddShadowRule(code, word, action, weight);
}

export async function removeShadowRule(code: string, word: string): Promise<void> {
  return App.RemoveShadowRule(code, word);
}

// 控制管道
export async function checkServiceRunning(): Promise<boolean> {
  return App.CheckServiceRunning();
}

export async function notifyReload(target: string): Promise<void> {
  return App.NotifyReload(target);
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
      system_dict: 'dict/pinyin/pinyin.txt',
      user_dict: 'user_dict.txt',
      pinyin_dict: 'dict/pinyin/pinyin.txt',
    },
    engine: {
      type: 'pinyin',
      filter_mode: 'smart',
      pinyin: {
        show_wubi_hint: true,
      },
      wubi: {
        auto_commit_at_4: false,
        clear_on_empty_at_4: false,
        top_code_commit: true,
        punct_commit: true,
      },
    },
    hotkeys: {
      toggle_mode_keys: ['lshift', 'rshift'],
      commit_on_switch: true,
      switch_engine: 'ctrl+`',
      toggle_full_width: 'shift+space',
      toggle_punct: 'ctrl+.',
    },
    ui: {
      font_size: 18,
      candidates_per_page: 9,
      font_path: '',
      inline_preedit: true,
      hide_candidate_window: false,
    },
    toolbar: {
      visible: false,
      position_x: 0,
      position_y: 0,
    },
    input: {
      punct_follow_mode: false,
      select_key_groups: ['semicolon_quote'],
      page_keys: ['pageupdown', 'minus_equal'],
    },
    advanced: {
      log_level: 'info',
    },
  });
}
