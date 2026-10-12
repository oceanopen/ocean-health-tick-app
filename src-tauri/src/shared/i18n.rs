use tauri::{AppHandle, Manager};

use crate::shared::app_config::{AppConfigState, LANGUAGE_KEY, read_app_config_raw};

use sys_locale::get_locale;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ResolvedLanguage {
    ZhCn,
    En,
}

// 系统语言探测：非 zh locale 默认英文（与前端 i18next fallbackLng='en' 一致）。
fn detect_system_language() -> ResolvedLanguage {
    match get_locale() {
        Some(locale) if locale.to_lowercase().starts_with("zh") => ResolvedLanguage::ZhCn,
        _ => ResolvedLanguage::En,
    }
}

// 三态解析：zh-CN/en 直接映射，None 或 "system" 走系统 locale 探测。
pub fn resolve(raw: Option<&str>) -> ResolvedLanguage {
    match raw {
        Some("zh-CN") => ResolvedLanguage::ZhCn,
        Some("en") => ResolvedLanguage::En,
        _ => detect_system_language(),
    }
}

// 读取当前语言偏好（app_config 的 language key）并解析为具体语言。
// 三态：zh-CN/en 直接映射，缺失或 "system" 走系统 locale 探测（默认英文）。
// 消费方：托盘菜单文案刷新（refresh_menu_texts）、settings 窗口 title。
pub fn current_language(app: &AppHandle) -> ResolvedLanguage {
    let Some(state) = app.try_state::<AppConfigState>() else {
        return resolve(None);
    };
    let raw = read_app_config_raw(state.inner(), LANGUAGE_KEY).unwrap_or(None);
    resolve(raw.as_deref())
}

/// 后端文案覆盖托盘菜单与 settings 窗口 title（业务文案在前端 react-i18next）。
/// 加 key 时同步 refresh_menu_texts 与 setup 的菜单构建。
pub fn menu_text(lang: ResolvedLanguage, key: &str) -> &'static str {
    match (lang, key) {
        (ResolvedLanguage::ZhCn, "settings") => "系统设置",
        (ResolvedLanguage::ZhCn, "restEnter") => "进入休息",
        (ResolvedLanguage::ZhCn, "restEnterLong") => "进入长休息",
        (ResolvedLanguage::ZhCn, "restActive") => "休息中",
        (ResolvedLanguage::ZhCn, "restActiveLong") => "长休息中",
        (ResolvedLanguage::ZhCn, "restart") => "重启",
        (ResolvedLanguage::ZhCn, "exit") => "退出",
        (ResolvedLanguage::En, "settings") => "Settings",
        (ResolvedLanguage::En, "restEnter") => "Enter Break",
        (ResolvedLanguage::En, "restEnterLong") => "Enter Long Break",
        (ResolvedLanguage::En, "restActive") => "On Break",
        (ResolvedLanguage::En, "restActiveLong") => "On Long Break",
        (ResolvedLanguage::En, "restart") => "Restart",
        (ResolvedLanguage::En, "exit") => "Quit",
        _ => "",
    }
}
