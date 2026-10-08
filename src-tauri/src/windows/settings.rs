use tauri::{Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::shared::events::EVENT_SETTINGS_NAVIGATE;
use crate::shared::i18n::{current_language, menu_text};
use crate::shared::screen::{
    DEFAULT_SIZE, SETTINGS_RATIO, find_monitor_for_tray, ratio_size, work_area_center,
};
use crate::shared::types::{PauseSource, Phase};

// async：WebviewWindowBuilder::build() 在 Windows 上于同步 #[tauri::command] 中会触发 wry#583
// 死锁（白屏 + 主线程卡死）；改 async 让 IPC 线程不阻塞消息泵。macOS 无此问题（WKWebView 无 COM 模型）。
#[tauri::command]
#[specta::specta]
pub async fn show_settings_window(
    app: tauri::AppHandle,
    navigate_to: Option<String>,
) -> Result<(), String> {
    // 按 tray.rect() 所在屏算尺寸；探测失败用 DEFAULT_SIZE 兜底，后续 set_position 也跳过。
    let monitor = find_monitor_for_tray(&app, "tray");
    let (width, height) = monitor
        .as_ref()
        .map(|m| ratio_size(m, SETTINGS_RATIO))
        .unwrap_or(DEFAULT_SIZE);

    // match 一次性产出 (窗口, 是否已存在)。is_existing 决定深链通道：
    // 已存在（webview 已就绪、前端 listen 已注册）→ show 后 emit_to 事件导航；
    // 首开 → 初始 URL 拼 hash（build() 返回时前端 listen 尚未注册，emit 必丢）。
    let (settings_win, settings_win_is_existing) = match app.get_webview_window("settings") {
        Some(w) => {
            // 二次唤起：显式重设尺寸，避免窗口实例首次建好后跨分辨率屏固化。
            let _ = w.set_size(LogicalSize::new(width, height));
            (w, true)
        }
        None => {
            let product = app
                .config()
                .product_name
                .as_deref()
                .unwrap_or("Ocean Health Tick");
            // 首开深链：分区直接拼进初始 URL 的 hash，由前端 HashRouter 解析。
            let url = match &navigate_to {
                Some(section) => format!("settings.html#/{section}").into(),
                None => "settings.html".into(),
            };
            let win = WebviewWindowBuilder::new(&app, "settings", WebviewUrl::App(url))
                .title(format!(
                    "{product} - {}",
                    menu_text(current_language(&app), "settings")
                ))
                .inner_size(width, height)
                // 默认在主屏居中；下方 set_position 修正到 tray 所在屏，探测失败保持主屏。
                .center()
                // 窗口不进任务栏与 Alt+Tab（Windows/Linux），macOS 上为 no-op（Dock 由 ActivationPolicy 控制）。
                .skip_taskbar(true)
                .build()
                .map_err(|e| e.to_string())?;

            let w = win.clone();
            win.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = w.hide();
                    // show_settings_window 打开时主动 hide 了 panel；非 Working 阶段需恢复常驻。
                    // 免打扰暂停（Paused+Dnd）例外：它是「收起不弹、想看才看」的静默状态，
                    // 恢复常驻会闪现面板又随失焦消失，违背非打断式语义（与托盘 dismissible 守卫对齐）。
                    let app = w.app_handle();
                    let phase = crate::timer::current_phase(app);
                    let dnd_paused = phase == Phase::Paused
                        && crate::timer::current_pause_source(app) == PauseSource::Dnd;
                    if phase != Phase::Working && !dnd_paused {
                        crate::windows::panel::show_panel(app);
                    }
                }
            });

            (win, false)
        }
    };

    // 新建和二次唤起都按 tray 所在屏的 work_area 居中；在 show 之前调用，无视觉跳跃。
    if let Some(m) = &monitor {
        let (x, y) = work_area_center(m, width, height);
        let _ = settings_win.set_position(LogicalPosition::new(x, y));
    }

    if let Some(panel) = app.get_webview_window("panel") {
        let _ = panel.hide();
    }

    let _ = settings_win.show();
    let _ = settings_win.unminimize();
    let _ = settings_win.set_focus();

    // 二次唤起深链：show 之后 emit，前端 goMenu 完成导航。无参不 emit——窗口 hide 不销毁，
    // hash 天然存活，保留用户上次所在分区。非法值由前端 isMenuKey 守卫回落默认分区。
    if let (true, Some(section)) = (settings_win_is_existing, navigate_to) {
        let _ = app.emit_to("settings", EVENT_SETTINGS_NAVIGATE, section);
    }

    Ok(())
}
