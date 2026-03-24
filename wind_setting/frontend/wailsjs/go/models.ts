export namespace config {
	
	export class AdvancedConfig {
	    log_level: string;
	
	    static createFrom(source: any = {}) {
	        return new AdvancedConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.log_level = source["log_level"];
	    }
	}
	export class CapsLockBehaviorConfig {
	    cancel_on_mode_switch: boolean;
	
	    static createFrom(source: any = {}) {
	        return new CapsLockBehaviorConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.cancel_on_mode_switch = source["cancel_on_mode_switch"];
	    }
	}
	export class TempPinyinConfig {
	    trigger_keys: string[];
	
	    static createFrom(source: any = {}) {
	        return new TempPinyinConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.trigger_keys = source["trigger_keys"];
	    }
	}
	export class ShiftTempEnglishConfig {
	    enabled: boolean;
	    show_english_candidates: boolean;
	
	    static createFrom(source: any = {}) {
	        return new ShiftTempEnglishConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.enabled = source["enabled"];
	        this.show_english_candidates = source["show_english_candidates"];
	    }
	}
	export class InputConfig {
	    punct_follow_mode: boolean;
	    select_key_groups: string[];
	    page_keys: string[];
	    highlight_keys: string[];
	    pinyin_separator: string;
	    shift_temp_english: ShiftTempEnglishConfig;
	    capslock_behavior: CapsLockBehaviorConfig;
	    temp_pinyin: TempPinyinConfig;
	
	    static createFrom(source: any = {}) {
	        return new InputConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.punct_follow_mode = source["punct_follow_mode"];
	        this.select_key_groups = source["select_key_groups"];
	        this.page_keys = source["page_keys"];
	        this.highlight_keys = source["highlight_keys"];
	        this.pinyin_separator = source["pinyin_separator"];
	        this.shift_temp_english = this.convertValues(source["shift_temp_english"], ShiftTempEnglishConfig);
	        this.capslock_behavior = this.convertValues(source["capslock_behavior"], CapsLockBehaviorConfig);
	        this.temp_pinyin = this.convertValues(source["temp_pinyin"], TempPinyinConfig);
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}
	export class ToolbarConfig {
	    visible: boolean;
	
	    static createFrom(source: any = {}) {
	        return new ToolbarConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.visible = source["visible"];
	    }
	}
	export class UIConfig {
	    font_size: number;
	    candidates_per_page: number;
	    font_path: string;
	    inline_preedit: boolean;
	    hide_candidate_window: boolean;
	    candidate_layout: string;
	    status_indicator_duration: number;
	    status_indicator_offset_x: number;
	    status_indicator_offset_y: number;
	    theme: string;
	    tooltip_delay: number;
	    text_render_mode?: string;
	    gdi_font_weight?: number;
	    gdi_font_scale?: number;
	    menu_font_weight?: number;
	    menu_font_size?: number;
	
	    static createFrom(source: any = {}) {
	        return new UIConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.font_size = source["font_size"];
	        this.candidates_per_page = source["candidates_per_page"];
	        this.font_path = source["font_path"];
	        this.inline_preedit = source["inline_preedit"];
	        this.hide_candidate_window = source["hide_candidate_window"];
	        this.candidate_layout = source["candidate_layout"];
	        this.status_indicator_duration = source["status_indicator_duration"];
	        this.status_indicator_offset_x = source["status_indicator_offset_x"];
	        this.status_indicator_offset_y = source["status_indicator_offset_y"];
	        this.theme = source["theme"];
	        this.tooltip_delay = source["tooltip_delay"];
	        this.text_render_mode = source["text_render_mode"];
	        this.gdi_font_weight = source["gdi_font_weight"];
	        this.gdi_font_scale = source["gdi_font_scale"];
	        this.menu_font_weight = source["menu_font_weight"];
	        this.menu_font_size = source["menu_font_size"];
	    }
	}
	export class HotkeyConfig {
	    toggle_mode_keys: string[];
	    commit_on_switch: boolean;
	    switch_engine: string;
	    toggle_full_width: string;
	    toggle_punct: string;
	
	    static createFrom(source: any = {}) {
	        return new HotkeyConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.toggle_mode_keys = source["toggle_mode_keys"];
	        this.commit_on_switch = source["commit_on_switch"];
	        this.switch_engine = source["switch_engine"];
	        this.toggle_full_width = source["toggle_full_width"];
	        this.toggle_punct = source["toggle_punct"];
	    }
	}
	export class SchemaConfig {
	    active: string;
	    available: string[];
	
	    static createFrom(source: any = {}) {
	        return new SchemaConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.active = source["active"];
	        this.available = source["available"];
	    }
	}
	export class StartupConfig {
	    remember_last_state: boolean;
	    default_chinese_mode: boolean;
	    default_full_width: boolean;
	    default_chinese_punct: boolean;
	
	    static createFrom(source: any = {}) {
	        return new StartupConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.remember_last_state = source["remember_last_state"];
	        this.default_chinese_mode = source["default_chinese_mode"];
	        this.default_full_width = source["default_full_width"];
	        this.default_chinese_punct = source["default_chinese_punct"];
	    }
	}
	export class Config {
	    startup: StartupConfig;
	    schema: SchemaConfig;
	    hotkeys: HotkeyConfig;
	    ui: UIConfig;
	    toolbar: ToolbarConfig;
	    input: InputConfig;
	    advanced: AdvancedConfig;
	
	    static createFrom(source: any = {}) {
	        return new Config(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.startup = this.convertValues(source["startup"], StartupConfig);
	        this.schema = this.convertValues(source["schema"], SchemaConfig);
	        this.hotkeys = this.convertValues(source["hotkeys"], HotkeyConfig);
	        this.ui = this.convertValues(source["ui"], UIConfig);
	        this.toolbar = this.convertValues(source["toolbar"], ToolbarConfig);
	        this.input = this.convertValues(source["input"], InputConfig);
	        this.advanced = this.convertValues(source["advanced"], AdvancedConfig);
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}
	
	
	
	
	
	
	

}

export namespace control {
	
	export class ServiceStatus {
	    running: boolean;
	    engine_type: string;
	    chinese_mode: boolean;
	    full_width: boolean;
	    chinese_punct: boolean;
	    dict_entries: number;
	    user_dict_count: number;
	    phrase_count: number;
	    shadow_count: number;
	
	    static createFrom(source: any = {}) {
	        return new ServiceStatus(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.running = source["running"];
	        this.engine_type = source["engine_type"];
	        this.chinese_mode = source["chinese_mode"];
	        this.full_width = source["full_width"];
	        this.chinese_punct = source["chinese_punct"];
	        this.dict_entries = source["dict_entries"];
	        this.user_dict_count = source["user_dict_count"];
	        this.phrase_count = source["phrase_count"];
	        this.shadow_count = source["shadow_count"];
	    }
	}

}

export namespace main {
	
	export class FileChangeStatus {
	    config_changed: boolean;
	    phrases_changed: boolean;
	    shadow_changed: boolean;
	    userdict_changed: boolean;
	
	    static createFrom(source: any = {}) {
	        return new FileChangeStatus(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.config_changed = source["config_changed"];
	        this.phrases_changed = source["phrases_changed"];
	        this.shadow_changed = source["shadow_changed"];
	        this.userdict_changed = source["userdict_changed"];
	    }
	}
	export class ImportExportResult {
	    cancelled: boolean;
	    count: number;
	    total?: number;
	    path?: string;
	
	    static createFrom(source: any = {}) {
	        return new ImportExportResult(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.cancelled = source["cancelled"];
	        this.count = source["count"];
	        this.total = source["total"];
	        this.path = source["path"];
	    }
	}
	export class PhraseItem {
	    code: string;
	    text: string;
	    candidates?: string[];
	    type?: string;
	    handler?: string;
	    weight: number;
	
	    static createFrom(source: any = {}) {
	        return new PhraseItem(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.code = source["code"];
	        this.text = source["text"];
	        this.candidates = source["candidates"];
	        this.type = source["type"];
	        this.handler = source["handler"];
	        this.weight = source["weight"];
	    }
	}
	export class SchemaConfigLearning {
	    mode: string;
	    unigram_path?: string;
	
	    static createFrom(source: any = {}) {
	        return new SchemaConfigLearning(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.mode = source["mode"];
	        this.unigram_path = source["unigram_path"];
	    }
	}
	export class SchemaConfigUserData {
	    shadow_file: string;
	    user_dict_file: string;
	    user_freq_file?: string;
	
	    static createFrom(source: any = {}) {
	        return new SchemaConfigUserData(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.shadow_file = source["shadow_file"];
	        this.user_dict_file = source["user_dict_file"];
	        this.user_freq_file = source["user_freq_file"];
	    }
	}
	export class SchemaConfigDict {
	    id: string;
	    path: string;
	    type: string;
	    default: boolean;
	    role?: string;
	
	    static createFrom(source: any = {}) {
	        return new SchemaConfigDict(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.id = source["id"];
	        this.path = source["path"];
	        this.type = source["type"];
	        this.default = source["default"];
	        this.role = source["role"];
	    }
	}
	export class SchemaConfigEngine {
	    type: string;
	    codetable?: Record<string, any>;
	    pinyin?: Record<string, any>;
	    mixed?: Record<string, any>;
	    filter_mode: string;
	
	    static createFrom(source: any = {}) {
	        return new SchemaConfigEngine(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.type = source["type"];
	        this.codetable = source["codetable"];
	        this.pinyin = source["pinyin"];
	        this.mixed = source["mixed"];
	        this.filter_mode = source["filter_mode"];
	    }
	}
	export class SchemaConfigMeta {
	    id: string;
	    name: string;
	    icon_label: string;
	    version: string;
	    author: string;
	    description: string;
	
	    static createFrom(source: any = {}) {
	        return new SchemaConfigMeta(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.id = source["id"];
	        this.name = source["name"];
	        this.icon_label = source["icon_label"];
	        this.version = source["version"];
	        this.author = source["author"];
	        this.description = source["description"];
	    }
	}
	export class SchemaConfig {
	    schema: SchemaConfigMeta;
	    engine: SchemaConfigEngine;
	    dictionaries: SchemaConfigDict[];
	    user_data: SchemaConfigUserData;
	    learning: SchemaConfigLearning;
	
	    static createFrom(source: any = {}) {
	        return new SchemaConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.schema = this.convertValues(source["schema"], SchemaConfigMeta);
	        this.engine = this.convertValues(source["engine"], SchemaConfigEngine);
	        this.dictionaries = this.convertValues(source["dictionaries"], SchemaConfigDict);
	        this.user_data = this.convertValues(source["user_data"], SchemaConfigUserData);
	        this.learning = this.convertValues(source["learning"], SchemaConfigLearning);
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}
	
	
	
	
	
	export class SchemaInfo {
	    id: string;
	    name: string;
	    icon_label: string;
	    version: string;
	    description: string;
	    engine_type: string;
	
	    static createFrom(source: any = {}) {
	        return new SchemaInfo(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.id = source["id"];
	        this.name = source["name"];
	        this.icon_label = source["icon_label"];
	        this.version = source["version"];
	        this.description = source["description"];
	        this.engine_type = source["engine_type"];
	    }
	}
	export class ShadowRuleItem {
	    code: string;
	    word: string;
	    type: string;
	    position: number;
	
	    static createFrom(source: any = {}) {
	        return new ShadowRuleItem(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.code = source["code"];
	        this.word = source["word"];
	        this.type = source["type"];
	        this.position = source["position"];
	    }
	}
	export class ThemeInfo {
	    name: string;
	    display_name: string;
	    author: string;
	    version: string;
	    is_builtin: boolean;
	    is_active: boolean;
	
	    static createFrom(source: any = {}) {
	        return new ThemeInfo(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.name = source["name"];
	        this.display_name = source["display_name"];
	        this.author = source["author"];
	        this.version = source["version"];
	        this.is_builtin = source["is_builtin"];
	        this.is_active = source["is_active"];
	    }
	}
	export class UserWordItem {
	    code: string;
	    text: string;
	    weight: number;
	    created_at: string;
	
	    static createFrom(source: any = {}) {
	        return new UserWordItem(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.code = source["code"];
	        this.text = source["text"];
	        this.weight = source["weight"];
	        this.created_at = source["created_at"];
	    }
	}

}

