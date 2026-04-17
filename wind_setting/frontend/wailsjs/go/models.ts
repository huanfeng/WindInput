export namespace config {
	
	export class AdvancedConfig {
	    log_level: string;
	    host_render_processes?: string[];
	
	    static createFrom(source: any = {}) {
	        return new AdvancedConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.log_level = source["log_level"];
	        this.host_render_processes = source["host_render_processes"];
	    }
	}
	export class AutoPairConfig {
	    chinese: boolean;
	    english: boolean;
	    blacklist: string[];
	    chinese_pairs: string[];
	    english_pairs: string[];
	
	    static createFrom(source: any = {}) {
	        return new AutoPairConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.chinese = source["chinese"];
	        this.english = source["english"];
	        this.blacklist = source["blacklist"];
	        this.chinese_pairs = source["chinese_pairs"];
	        this.english_pairs = source["english_pairs"];
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
	export class OverflowBehaviorConfig {
	    number_key: string;
	    select_key: string;
	    select_char_key: string;
	
	    static createFrom(source: any = {}) {
	        return new OverflowBehaviorConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.number_key = source["number_key"];
	        this.select_key = source["select_key"];
	        this.select_char_key = source["select_char_key"];
	    }
	}
	export class QuickInputConfig {
	    enabled: boolean;
	    trigger_key: string;
	    force_vertical: boolean;
	    decimal_places: number;
	    numpad_behavior: string;
	
	    static createFrom(source: any = {}) {
	        return new QuickInputConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.enabled = source["enabled"];
	        this.trigger_key = source["trigger_key"];
	        this.force_vertical = source["force_vertical"];
	        this.decimal_places = source["decimal_places"];
	        this.numpad_behavior = source["numpad_behavior"];
	    }
	}
	export class PunctCustomConfig {
	    enabled: boolean;
	    mappings?: Record<string, Array<string>>;
	
	    static createFrom(source: any = {}) {
	        return new PunctCustomConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.enabled = source["enabled"];
	        this.mappings = source["mappings"];
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
	    filter_mode: string;
	    select_key_groups: string[];
	    page_keys: string[];
	    highlight_keys: string[];
	    select_char_keys: string[];
	    smart_punct_after_digit: boolean;
	    smart_punct_list: string;
	    enter_behavior: string;
	    space_on_empty_behavior: string;
	    pinyin_separator: string;
	    shift_temp_english: ShiftTempEnglishConfig;
	    capslock_behavior: CapsLockBehaviorConfig;
	    temp_pinyin: TempPinyinConfig;
	    auto_pair: AutoPairConfig;
	    punct_custom: PunctCustomConfig;
	    quick_input: QuickInputConfig;
	    overflow_behavior: OverflowBehaviorConfig;
	
	    static createFrom(source: any = {}) {
	        return new InputConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.punct_follow_mode = source["punct_follow_mode"];
	        this.filter_mode = source["filter_mode"];
	        this.select_key_groups = source["select_key_groups"];
	        this.page_keys = source["page_keys"];
	        this.highlight_keys = source["highlight_keys"];
	        this.select_char_keys = source["select_char_keys"];
	        this.smart_punct_after_digit = source["smart_punct_after_digit"];
	        this.smart_punct_list = source["smart_punct_list"];
	        this.enter_behavior = source["enter_behavior"];
	        this.space_on_empty_behavior = source["space_on_empty_behavior"];
	        this.pinyin_separator = source["pinyin_separator"];
	        this.shift_temp_english = this.convertValues(source["shift_temp_english"], ShiftTempEnglishConfig);
	        this.capslock_behavior = this.convertValues(source["capslock_behavior"], CapsLockBehaviorConfig);
	        this.temp_pinyin = this.convertValues(source["temp_pinyin"], TempPinyinConfig);
	        this.auto_pair = this.convertValues(source["auto_pair"], AutoPairConfig);
	        this.punct_custom = this.convertValues(source["punct_custom"], PunctCustomConfig);
	        this.quick_input = this.convertValues(source["quick_input"], QuickInputConfig);
	        this.overflow_behavior = this.convertValues(source["overflow_behavior"], OverflowBehaviorConfig);
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
	export class StatusIndicatorConfig {
	    enabled: boolean;
	    duration: number;
	    display_mode: string;
	    schema_name_style: string;
	    show_mode: boolean;
	    show_punct: boolean;
	    show_full_width: boolean;
	    position_mode: string;
	    offset_x: number;
	    offset_y: number;
	    custom_x: number;
	    custom_y: number;
	    font_size: number;
	    opacity: number;
	    background_color: string;
	    text_color: string;
	    border_radius: number;
	
	    static createFrom(source: any = {}) {
	        return new StatusIndicatorConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.enabled = source["enabled"];
	        this.duration = source["duration"];
	        this.display_mode = source["display_mode"];
	        this.schema_name_style = source["schema_name_style"];
	        this.show_mode = source["show_mode"];
	        this.show_punct = source["show_punct"];
	        this.show_full_width = source["show_full_width"];
	        this.position_mode = source["position_mode"];
	        this.offset_x = source["offset_x"];
	        this.offset_y = source["offset_y"];
	        this.custom_x = source["custom_x"];
	        this.custom_y = source["custom_y"];
	        this.font_size = source["font_size"];
	        this.opacity = source["opacity"];
	        this.background_color = source["background_color"];
	        this.text_color = source["text_color"];
	        this.border_radius = source["border_radius"];
	    }
	}
	export class UIConfig {
	    font_size: number;
	    candidates_per_page: number;
	    font_family: string;
	    font_path: string;
	    inline_preedit: boolean;
	    hide_candidate_window: boolean;
	    candidate_layout: string;
	    status_indicator_duration: number;
	    status_indicator_offset_x: number;
	    status_indicator_offset_y: number;
	    theme: string;
	    theme_style: string;
	    tooltip_delay: number;
	    preedit_mode: string;
	    text_render_mode?: string;
	    gdi_font_weight?: number;
	    gdi_font_scale?: number;
	    menu_font_weight?: number;
	    menu_font_size?: number;
	    status_indicator: StatusIndicatorConfig;
	
	    static createFrom(source: any = {}) {
	        return new UIConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.font_size = source["font_size"];
	        this.candidates_per_page = source["candidates_per_page"];
	        this.font_family = source["font_family"];
	        this.font_path = source["font_path"];
	        this.inline_preedit = source["inline_preedit"];
	        this.hide_candidate_window = source["hide_candidate_window"];
	        this.candidate_layout = source["candidate_layout"];
	        this.status_indicator_duration = source["status_indicator_duration"];
	        this.status_indicator_offset_x = source["status_indicator_offset_x"];
	        this.status_indicator_offset_y = source["status_indicator_offset_y"];
	        this.theme = source["theme"];
	        this.theme_style = source["theme_style"];
	        this.tooltip_delay = source["tooltip_delay"];
	        this.preedit_mode = source["preedit_mode"];
	        this.text_render_mode = source["text_render_mode"];
	        this.gdi_font_weight = source["gdi_font_weight"];
	        this.gdi_font_scale = source["gdi_font_scale"];
	        this.menu_font_weight = source["menu_font_weight"];
	        this.menu_font_size = source["menu_font_size"];
	        this.status_indicator = this.convertValues(source["status_indicator"], StatusIndicatorConfig);
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
	export class HotkeyConfig {
	    toggle_mode_keys: string[];
	    commit_on_switch: boolean;
	    switch_engine: string;
	    toggle_full_width: string;
	    toggle_punct: string;
	    delete_candidate: string;
	    pin_candidate: string;
	    toggle_toolbar: string;
	    open_settings: string;
	    add_word: string;
	    global_hotkeys: string[];
	
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
	        this.delete_candidate = source["delete_candidate"];
	        this.pin_candidate = source["pin_candidate"];
	        this.toggle_toolbar = source["toggle_toolbar"];
	        this.open_settings = source["open_settings"];
	        this.add_word = source["add_word"];
	        this.global_hotkeys = source["global_hotkeys"];
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

export namespace main {
	
	export class AddWordParams {
	    text: string;
	    code: string;
	    schema_id: string;
	
	    static createFrom(source: any = {}) {
	        return new AddWordParams(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.text = source["text"];
	        this.code = source["code"];
	        this.schema_id = source["schema_id"];
	    }
	}
	export class DictImportPreview {
	    schema_id: string;
	    schema_name: string;
	    generator: string;
	    exported_at: string;
	    sections: Record<string, number>;
	    source_file: string;
	
	    static createFrom(source: any = {}) {
	        return new DictImportPreview(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.schema_id = source["schema_id"];
	        this.schema_name = source["schema_name"];
	        this.generator = source["generator"];
	        this.exported_at = source["exported_at"];
	        this.sections = source["sections"];
	        this.source_file = source["source_file"];
	    }
	}
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
	export class ImportPreviewSchema {
	    id: string;
	    name: string;
	    version: string;
	    author: string;
	    description: string;
	    engine_type: string;
	    dict_count: number;
	    conflict: boolean;
	    conflict_src: string;
	
	    static createFrom(source: any = {}) {
	        return new ImportPreviewSchema(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.id = source["id"];
	        this.name = source["name"];
	        this.version = source["version"];
	        this.author = source["author"];
	        this.description = source["description"];
	        this.engine_type = source["engine_type"];
	        this.dict_count = source["dict_count"];
	        this.conflict = source["conflict"];
	        this.conflict_src = source["conflict_src"];
	    }
	}
	export class ImportPreview {
	    zip_path: string;
	    schemas: ImportPreviewSchema[];
	    file_count: number;
	
	    static createFrom(source: any = {}) {
	        return new ImportPreview(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.zip_path = source["zip_path"];
	        this.schemas = this.convertValues(source["schemas"], ImportPreviewSchema);
	        this.file_count = source["file_count"];
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
	export class PagedDictResult {
	    words: UserWordItem[];
	    total: number;
	
	    static createFrom(source: any = {}) {
	        return new PagedDictResult(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.words = this.convertValues(source["words"], UserWordItem);
	        this.total = source["total"];
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
	export class PathInfo {
	    config_dir: string;
	    config_dir_display: string;
	    logs_dir: string;
	    logs_dir_display: string;
	    is_portable: boolean;
	
	    static createFrom(source: any = {}) {
	        return new PathInfo(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.config_dir = source["config_dir"];
	        this.config_dir_display = source["config_dir_display"];
	        this.logs_dir = source["logs_dir"];
	        this.logs_dir_display = source["logs_dir_display"];
	        this.is_portable = source["is_portable"];
	    }
	}
	export class PhraseItem {
	    code: string;
	    text?: string;
	    texts?: string;
	    name?: string;
	    type: string;
	    position: number;
	    enabled: boolean;
	    is_system: boolean;
	
	    static createFrom(source: any = {}) {
	        return new PhraseItem(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.code = source["code"];
	        this.text = source["text"];
	        this.texts = source["texts"];
	        this.name = source["name"];
	        this.type = source["type"];
	        this.position = source["position"];
	        this.enabled = source["enabled"];
	        this.is_system = source["is_system"];
	    }
	}
	export class SchemaConfigFreq {
	    enabled: boolean;
	    protect_top_n?: number;
	    half_life?: number;
	    boost_max?: number;
	    max_recency?: number;
	    base_scale?: number;
	    streak_scale?: number;
	    streak_cap?: number;
	
	    static createFrom(source: any = {}) {
	        return new SchemaConfigFreq(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.enabled = source["enabled"];
	        this.protect_top_n = source["protect_top_n"];
	        this.half_life = source["half_life"];
	        this.boost_max = source["boost_max"];
	        this.max_recency = source["max_recency"];
	        this.base_scale = source["base_scale"];
	        this.streak_scale = source["streak_scale"];
	        this.streak_cap = source["streak_cap"];
	    }
	}
	export class SchemaConfigAutoLearn {
	    enabled: boolean;
	    count_threshold?: number;
	    min_word_length?: number;
	    weight_delta?: number;
	    add_weight?: number;
	
	    static createFrom(source: any = {}) {
	        return new SchemaConfigAutoLearn(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.enabled = source["enabled"];
	        this.count_threshold = source["count_threshold"];
	        this.min_word_length = source["min_word_length"];
	        this.weight_delta = source["weight_delta"];
	        this.add_weight = source["add_weight"];
	    }
	}
	export class SchemaConfigLearning {
	    auto_learn?: SchemaConfigAutoLearn;
	    freq?: SchemaConfigFreq;
	    protect_top_n?: number;
	    unigram_path?: string;
	    temp_max_entries?: number;
	    temp_promote_count?: number;
	
	    static createFrom(source: any = {}) {
	        return new SchemaConfigLearning(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.auto_learn = this.convertValues(source["auto_learn"], SchemaConfigAutoLearn);
	        this.freq = this.convertValues(source["freq"], SchemaConfigFreq);
	        this.protect_top_n = source["protect_top_n"];
	        this.unigram_path = source["unigram_path"];
	        this.temp_max_entries = source["temp_max_entries"];
	        this.temp_promote_count = source["temp_promote_count"];
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
	export class SchemaConfigDict {
	    id: string;
	    path: string;
	    type: string;
	    default: boolean;
	    role?: string;
	    weight_spec?: any;
	
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
	        this.weight_spec = source["weight_spec"];
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
	    learning: SchemaConfigLearning;
	    encoder?: any;
	
	    static createFrom(source: any = {}) {
	        return new SchemaConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.schema = this.convertValues(source["schema"], SchemaConfigMeta);
	        this.engine = this.convertValues(source["engine"], SchemaConfigEngine);
	        this.dictionaries = this.convertValues(source["dictionaries"], SchemaConfigDict);
	        this.learning = this.convertValues(source["learning"], SchemaConfigLearning);
	        this.encoder = source["encoder"];
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
	
	
	
	
	
	
	export class SchemaDictStats {
	    schema_id: string;
	    schema_name: string;
	    icon_label: string;
	    word_count: number;
	    shadow_count: number;
	    temp_word_count: number;
	
	    static createFrom(source: any = {}) {
	        return new SchemaDictStats(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.schema_id = source["schema_id"];
	        this.schema_name = source["schema_name"];
	        this.icon_label = source["icon_label"];
	        this.word_count = source["word_count"];
	        this.shadow_count = source["shadow_count"];
	        this.temp_word_count = source["temp_word_count"];
	    }
	}
	export class SchemaInfo {
	    id: string;
	    name: string;
	    icon_label: string;
	    version: string;
	    description: string;
	    engine_type: string;
	    source: string;
	    error?: string;
	
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
	        this.source = source["source"];
	        this.error = source["error"];
	    }
	}
	export class SchemaStatusItem {
	    schema_id: string;
	    schema_name: string;
	    engine_type: string;
	    is_mixed: boolean;
	    status: string;
	    user_words: number;
	    temp_words: number;
	    shadow_rules: number;
	    freq_records: number;
	
	    static createFrom(source: any = {}) {
	        return new SchemaStatusItem(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.schema_id = source["schema_id"];
	        this.schema_name = source["schema_name"];
	        this.engine_type = source["engine_type"];
	        this.is_mixed = source["is_mixed"];
	        this.status = source["status"];
	        this.user_words = source["user_words"];
	        this.temp_words = source["temp_words"];
	        this.shadow_rules = source["shadow_rules"];
	        this.freq_records = source["freq_records"];
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
	export class SystemFontInfo {
	    family: string;
	    display_name: string;
	
	    static createFrom(source: any = {}) {
	        return new SystemFontInfo(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.family = source["family"];
	        this.display_name = source["display_name"];
	    }
	}
	export class TSFLogConfig {
	    mode: string;
	    level: string;
	
	    static createFrom(source: any = {}) {
	        return new TSFLogConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.mode = source["mode"];
	        this.level = source["level"];
	    }
	}
	export class TempWordItem {
	    code: string;
	    text: string;
	    weight: number;
	    count: number;
	
	    static createFrom(source: any = {}) {
	        return new TempWordItem(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.code = source["code"];
	        this.text = source["text"];
	        this.weight = source["weight"];
	        this.count = source["count"];
	    }
	}
	export class TextListPreviewResult {
	    total: number;
	    success_count: number;
	    fail_count: number;
	    results: rpcapi.EncodeResultItem[];
	
	    static createFrom(source: any = {}) {
	        return new TextListPreviewResult(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.total = source["total"];
	        this.success_count = source["success_count"];
	        this.fail_count = source["fail_count"];
	        this.results = this.convertValues(source["results"], rpcapi.EncodeResultItem);
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
	export class ThemeInfo {
	    name: string;
	    display_name: string;
	    author: string;
	    version: string;
	    is_builtin: boolean;
	    is_active: boolean;
	    has_variants: boolean;
	
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
	        this.has_variants = source["has_variants"];
	    }
	}
	
	export class ZipSchemaPreviewItem {
	    schema_id: string;
	    schema_name: string;
	    sections: Record<string, number>;
	
	    static createFrom(source: any = {}) {
	        return new ZipSchemaPreviewItem(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.schema_id = source["schema_id"];
	        this.schema_name = source["schema_name"];
	        this.sections = source["sections"];
	    }
	}
	export class ZipImportPreview {
	    schemas: ZipSchemaPreviewItem[];
	    has_phrases: boolean;
	    phrase_count: number;
	
	    static createFrom(source: any = {}) {
	        return new ZipImportPreview(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.schemas = this.convertValues(source["schemas"], ZipSchemaPreviewItem);
	        this.has_phrases = source["has_phrases"];
	        this.phrase_count = source["phrase_count"];
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

export namespace rpcapi {
	
	export class EncodeResultItem {
	    word: string;
	    code: string;
	    status: string;
	    error?: string;
	
	    static createFrom(source: any = {}) {
	        return new EncodeResultItem(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.word = source["word"];
	        this.code = source["code"];
	        this.status = source["status"];
	        this.error = source["error"];
	    }
	}
	export class SystemStatusReply {
	    running: boolean;
	    schema_id: string;
	    engine_type: string;
	    chinese_mode: boolean;
	    full_width: boolean;
	    chinese_punct: boolean;
	    store_enabled: boolean;
	    user_words: number;
	    temp_words: number;
	    phrases: number;
	    shadow_rules: number;
	
	    static createFrom(source: any = {}) {
	        return new SystemStatusReply(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.running = source["running"];
	        this.schema_id = source["schema_id"];
	        this.engine_type = source["engine_type"];
	        this.chinese_mode = source["chinese_mode"];
	        this.full_width = source["full_width"];
	        this.chinese_punct = source["chinese_punct"];
	        this.store_enabled = source["store_enabled"];
	        this.user_words = source["user_words"];
	        this.temp_words = source["temp_words"];
	        this.phrases = source["phrases"];
	        this.shadow_rules = source["shadow_rules"];
	    }
	}

}

