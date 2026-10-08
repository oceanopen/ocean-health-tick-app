use rusqlite::{Connection, OptionalExtension, params};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::shared::types::AppConfigChangedPayload;

/// 语言偏好 key（后端 SSOT，经 Builder.constant 导出；前端 appConfig.ts re-export）。
/// 后端仅托盘菜单消费（current_language 读取），其余业务不读。
pub const LANGUAGE_KEY: &str = "language";
/// 语言默认值："system"（缺失/非法时回退，后端 resolve 走系统 locale 探测）。
pub const DEFAULT_LANGUAGE: &str = "system";

/// 长休息窗口形态 key + 默认值（后端 SSOT，经 Builder.constant 导出）。
/// 取值同前端 RestWindow（"tray" | "topRight" | "fullscreen"），后端 panel.rs 在长休息
/// （含其休息前提醒 Alerting）唤起窗口时读取分派。三形态均已实现：tray / topRight 为小窗定位，
/// fullscreen 为伪全屏：窗口铺满 work_area（Alerting 起接管，Working 时退出并恢复小窗）。
pub const LONG_BREAK_WINDOW_KEY: &str = "long_break_window";
pub const DEFAULT_LONG_BREAK_WINDOW: &str = LONG_BREAK_WINDOW_TRAY;
pub const LONG_BREAK_WINDOW_TRAY: &str = "tray";
pub const LONG_BREAK_WINDOW_TOP_RIGHT: &str = "topRight";
pub const LONG_BREAK_WINDOW_FULLSCREEN: &str = "fullscreen";

/// 正常休息窗口形态 key（后端 SSOT，经 Builder.constant 导出）。
/// 取值与读取语义同上（三形态均已实现），后端 panel.rs 在正常休息（含其休息前提醒 Alerting）唤起窗口时读取分派。
pub const REST_WINDOW_KEY: &str = "rest_window";
pub const DEFAULT_REST_WINDOW: &str = REST_WINDOW_TRAY;
pub const REST_WINDOW_TRAY: &str = "tray";
pub const REST_WINDOW_TOP_RIGHT: &str = "topRight";
pub const REST_WINDOW_FULLSCREEN: &str = "fullscreen";

/// 休息时段窗口形态 key（后端 SSOT，经 Builder.constant 导出）。
/// 取值与读取语义同上（三形态均已实现），后端 panel.rs 在休息时段（quiet_hours）命中强制暂停时读取分派。
pub const QUIET_WINDOW_KEY: &str = "quiet_window";
pub const DEFAULT_QUIET_WINDOW: &str = QUIET_WINDOW_TRAY;
pub const QUIET_WINDOW_TRAY: &str = "tray";
pub const QUIET_WINDOW_TOP_RIGHT: &str = "topRight";
pub const QUIET_WINDOW_FULLSCREEN: &str = "fullscreen";

/// 开机自启动 key（后端 SSOT，经 Builder.constant 导出）。
/// Y/N 存储（YesNo 约定），语义为系统登录项状态的本地镜像（系统优先）：
/// 启动时 shared/autostart.rs 以系统真实状态回写本配置，保存时经事件监听
/// 同步系统登录项。跨平台由 tauri-plugin-autostart 抹平（macOS LaunchAgent /
/// Windows Registry / Linux XDG）。
pub const LAUNCH_AT_LOGIN_KEY: &str = "launch_at_login";
/// 默认值：false（未开启，与全新安装的系统登录项状态一致）。
pub const DEFAULT_LAUNCH_AT_LOGIN: bool = false;

pub struct AppConfigState(pub Mutex<Connection>);

pub fn init(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = app.path().app_data_dir()?;
    // dev/release 通过不同 identifier（com.ocean.health.tick.dev / com.ocean.health.tick）自动隔离
    // app_data_dir，无需手动拼接子目录。
    std::fs::create_dir_all(&data_dir)?;
    let db_path = data_dir.join("app.db");
    let conn = Connection::open(db_path)?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS app_config (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )?;
    app.manage(AppConfigState(Mutex::new(conn)));
    Ok(())
}

pub fn read_app_config_conn(conn: &Connection, key: &str) -> Result<Option<String>, String> {
    let mut stmt = conn
        .prepare("SELECT value FROM app_config WHERE key = ?1")
        .map_err(|e| e.to_string())?;
    stmt.query_row(params![key], |row| row.get::<_, String>(0))
        .optional()
        .map_err(|e| e.to_string())
}

pub fn read_app_config_raw(state: &AppConfigState, key: &str) -> Result<Option<String>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    read_app_config_conn(&conn, key)
}

pub fn write_app_config_conn(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO app_config (key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn write_app_config_raw(state: &AppConfigState, key: &str, value: &str) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    write_app_config_conn(&conn, key, value)
}

#[tauri::command]
#[specta::specta]
pub fn get_app_config(
    state: State<'_, AppConfigState>,
    key: String,
) -> Result<Option<String>, String> {
    read_app_config_raw(&state, &key)
}

#[tauri::command]
#[specta::specta]
pub fn set_app_config(
    app: AppHandle,
    state: State<'_, AppConfigState>,
    key: String,
    value: String,
) -> Result<(), String> {
    // 值未变化的写入跳过广播（emit 收口）：避免无差异事件触发订阅方副作用——
    // 典型是 launch_at_login 在 Y 不变时重发会经监听器重建刚被用户手动删除的
    // 登录项 plist（运行期"复活"）；对 language/appearance 等也省去无谓刷新。
    let old_value = read_app_config_raw(&state, &key)?;
    if old_value.as_deref() == Some(value.as_str()) {
        return Ok(());
    }
    write_app_config_raw(&state, &key, &value)?;
    app.emit(
        crate::shared::events::EVENT_APP_CONFIG_CHANGED,
        AppConfigChangedPayload { key, value },
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
