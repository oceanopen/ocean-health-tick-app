use std::sync::Mutex;

use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Listener, LogicalPosition, LogicalSize, Manager, Position, WebviewUrl,
    WebviewWindowBuilder,
    menu::{MenuBuilder, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

use crate::shared::app_config::{
    AppConfigState, LANGUAGE_KEY, LONG_BREAK_WINDOW_FULLSCREEN, LONG_BREAK_WINDOW_KEY,
    LONG_BREAK_WINDOW_TOP_RIGHT, LONG_BREAK_WINDOW_TRAY, QUIET_WINDOW_FULLSCREEN, QUIET_WINDOW_KEY,
    QUIET_WINDOW_TOP_RIGHT, QUIET_WINDOW_TRAY, REST_WINDOW_FULLSCREEN, REST_WINDOW_KEY,
    REST_WINDOW_TOP_RIGHT, REST_WINDOW_TRAY, read_app_config_raw,
};
use crate::shared::events::EVENT_PANEL_FORM_CHANGED;
use crate::shared::i18n::{current_language, menu_text};
use crate::shared::screen::{
    MonitorInfo, TaskbarEdge, detect_taskbar_edge, find_monitor_for_rect, find_monitor_for_tray,
};
use crate::shared::types::{PauseSource, Phase, TimerStatePayload};

const PANEL_WIDTH: f64 = 240.0;
const DEFAULT_PANEL_HEIGHT: f64 = 320.0;

/// 右上角形态与屏幕 work_area 边缘的间距（逻辑像素），避免窗口紧贴屏幕边缘。
const PANEL_TOP_RIGHT_MARGIN: f64 = 12.0;

/// panel 窗口当前形态（贴托盘 / 屏幕右上角 / 全屏强制）。
/// 受管状态（app.manage）：phase-changed 监听器按 rest_window / long_break_window / quiet_window 配置写入，
/// show_panel / fit_panel / create_panel / settings 恢复等所有定位路径统一读取，
/// 保证高度自适应、窗口恢复后不跳回托盘位。
#[derive(Clone, Copy, PartialEq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub enum PanelForm {
    /// 贴托盘图标旁（现有默认形态）。
    Tray,
    /// 托盘所在屏 work_area 右上角。
    TopRight,
    /// 伪全屏：窗口铺满托盘所在屏完整边界（不预留任务栏/Dock 高度，置顶无装饰）。
    /// Alerting 起接管，Working（跳过 / 我回来了 / 重置）时退出并恢复小窗。
    /// 不用原生 set_fullscreen：对 borderless+transparent 窗口会补装标题栏装饰且
    /// 异步时序与 show/set_focus 竞争出「带标题栏的 maximized」污染形态（实测）。
    Fullscreen,
}

/// 受管状态体：除当前形态外，携带进入全屏前的小窗高度，
/// 供退出全屏时显式恢复（macOS 退出全屏对 borderless 窗口的 frame 还原不可靠）。
struct PanelFormInner {
    form: PanelForm,
    last_small_height: f64,
}

struct PanelFormState(pub Mutex<PanelFormInner>);

// PanelFormState 读写统一入口：state 缺失 / 锁 poisoned 返回 None，调用方各自回落。
// 与 app_config.rs 的 read/write_app_config_raw 封装先例同构，收敛锁样板。
// 注意：闭包内不可调用跨线程同步 API（inner_size / scale_factor 等会阻塞等主线程），
// 否则与主线程命令入口的 lock 互等成死锁——此类调用一律先在锁外取值再写入。
fn with_panel_form<R>(app: &AppHandle, f: impl FnOnce(&mut PanelFormInner) -> R) -> Option<R> {
    let state = app.try_state::<PanelFormState>()?;
    let mut inner = state.0.lock().ok()?;
    Some(f(&mut inner))
}

// 读当前形态；锁 poisoned 时保守回退 Tray（贴托盘是历史默认行为）。
fn current_panel_form(app: &AppHandle) -> PanelForm {
    with_panel_form(app, |f| f.form).unwrap_or(PanelForm::Tray)
}

// 读进入全屏前记录的小窗高度；缺省 / 锁 poisoned 回落默认高度。
fn last_small_height(app: &AppHandle) -> f64 {
    with_panel_form(app, |f| f.last_small_height)
        .filter(|h| h.is_finite() && *h > 0.0)
        .unwrap_or(DEFAULT_PANEL_HEIGHT)
}

// 形态唯一写入口：写状态 + 执行全屏进出副作用 + 向 panel 窗口广播形态变化。
//   - 幂等早退（old == target）：Alerting → Breaking 连续触发时不重复 toggle 全屏（防闪烁）。
//   - 副作用在前、emit 在后：保证前端收到事件时窗口形态已就绪，布局切换不被旧 frame 裁剪。
//   - 全屏进入失败降级 Tray 并 emit tray，杜绝「前端全屏布局 + 窗口未全屏」的错位。
fn sync_panel_form(app: &AppHandle, target: PanelForm) {
    let old = current_panel_form(app);
    if old == target {
        return;
    }
    with_panel_form(app, |f| f.form = target);
    match (old, target) {
        // 进入全屏：记录小窗高度（exit 恢复用）。enter 失败时已内部降级为 Tray 并广播。
        (form, PanelForm::Fullscreen) if form != PanelForm::Fullscreen => {
            if !enter_panel_fullscreen(app) {
                with_panel_form(app, |f| f.form = PanelForm::Tray);
                let _ = app.emit_to("panel", EVENT_PANEL_FORM_CHANGED, PanelForm::Tray);
                return;
            }
        }
        // 退出全屏：显式恢复小窗尺寸 + 贴托盘定位（此刻状态已是小窗形态）。
        (PanelForm::Fullscreen, form) if form != PanelForm::Fullscreen => {
            exit_panel_fullscreen(app);
        }
        _ => {}
    }
    let _ = app.emit_to("panel", EVENT_PANEL_FORM_CHANGED, target);
}

// 进入伪全屏：记录小窗高度 → 窗口铺满托盘所在屏的完整边界（不预留任务栏/菜单栏/Dock 高度）。
//
// 为什么铺完整边界而非 work_area：work_area 扣除任务栏后在游戏独占全屏场景下底部会露出
// 一条任务栏高度的空白条，鼠标仍可在该区域操作屏幕，违背全屏提醒的强制休息意图；
// 桌面场景下任务栏盖在遮罩上/下均可接受（macOS 菜单栏 z-order 恒在最上仍盖不住，行为不变）。
//
// 为什么不用原生 set_fullscreen（tao 0.35 macOS 实测两类污染）：
//   1. borderless + transparent 窗口进入原生全屏时，系统会补装标题栏装饰——
//      实测出现「红黄绿交通灯 + Tauri App 标题栏」的带装饰中间形态；
//   2. set_fullscreen 为 dispatch_async 异步下发，与紧随的 show/set_focus 竞争，
//      窗口以可见身份参与窗口管理后被折衷成「带标题栏的 maximized」非全屏 Space 形态，
//      菜单栏/其他窗口仍可见；退出时 frame 还原错乱（工作中卡片滞留 3/4 屏宽）。
// 伪全屏保持窗口参数不变（无装饰、无 Space 切换、同步 set_size/set_position 无时序竞争），
// 代价是盖不住菜单栏（macOS 菜单栏 z-order 恒在最上，可接受）。
// 返回 false 表示进入失败（调用方据此降级 Tray）。
fn enter_panel_fullscreen(app: &AppHandle) -> bool {
    let Some(panel) = app.get_webview_window("panel") else {
        // 窗口尚未懒创建（如启动后首次休息）：不算失败，保持 Fullscreen 状态交由
        // 紧随其后的 show_panel → create_panel 按形态铺满（链路已存在）。
        // 若此处按失败降级 Tray，create_panel 将读到 Tray 走贴托盘分支，首次休息全屏静默失效。
        return true;
    };
    // 进入前先在锁外读取当前小窗高度（inner_size 为物理像素，按 scale_factor 换回逻辑值；
    // 本调用跑在 timer 线程，inner_size/scale_factor 会阻塞等主线程，绝不可发生在持锁期间，
    // 否则与主线程命令入口（fit_panel/get_panel_form 的 lock）互等成死锁）。
    let small_height = panel
        .inner_size()
        .map(|s| {
            s.to_logical::<f64>(panel.scale_factor().unwrap_or(1.0))
                .height
        })
        .unwrap_or(DEFAULT_PANEL_HEIGHT);
    with_panel_form(app, |f| f.last_small_height = small_height);
    // 铺满托盘所在屏完整边界（多屏下与托盘同屏，行为可预期）；探测失败回退贴托盘小窗形态。
    let Some(monitor) = find_monitor_for_tray(app, "tray") else {
        // warn 而非 info：release 构建日志级别为 Warn，info 会被过滤导致生产排障不可见。
        log::warn!("monitor not found for fullscreen panel, fallback to tray panel");
        return false;
    };
    let _ = panel.set_size(LogicalSize::new(monitor.width, monitor.height));
    let _ = panel.set_position(LogicalPosition::new(monitor.x, monitor.y));
    true
}

// 退出伪全屏并恢复小窗：显式 set_size 回记录的高度 → 重新贴托盘定位。
// （伪全屏不动窗口装饰与 Space，无「系统 frame 还原不可靠」问题，恢复完全受控。）
// 不 hide：衔接 Working 失焦自动隐藏机制（用户点别处即隐藏）。
fn exit_panel_fullscreen(app: &AppHandle) {
    let Some(panel) = app.get_webview_window("panel") else {
        log::warn!("panel not found when exit fullscreen");
        return;
    };
    let height = last_small_height(app);
    let _ = panel.set_size(LogicalSize::new(PANEL_WIDTH, height));
    if let Some(tray) = app.tray_by_id("tray") {
        position_panel(&tray, &panel);
    }
}

// 按 key + 三取值常量读窗口形态配置并映射为 PanelForm（rest_window / long_break_window / quiet_window 共用）。
// 现读 DB（无缓存，保存后下一次唤起即生效；窗口接管期间热改配置，下次 phase-changed 读到新值即热切换）。
// 三个取值显式分派：
//   - tray / topRight：小窗形态（topRight 叠加在 tray 之上，仅定位不同）。
//   - fullscreen：伪全屏（铺满完整显示器边界，接管屏幕，Working 时退出）。
//   - DB 无值 / 非法值：回退 tray。
fn panel_form_from_key(
    app: &AppHandle,
    key: &str,
    tray: &str,
    top_right: &str,
    fullscreen: &str,
) -> PanelForm {
    let window = app
        .try_state::<AppConfigState>()
        .and_then(|state| read_app_config_raw(&state, key).ok().flatten())
        .unwrap_or_else(|| tray.to_string());
    match window.as_str() {
        v if v == tray => PanelForm::Tray,
        v if v == top_right => PanelForm::TopRight,
        v if v == fullscreen => PanelForm::Fullscreen,
        _ => PanelForm::Tray,
    }
}

// 按休息类型读对应窗口形态配置（正常休息 / 长休息共用入口）。
fn panel_form_by_window_config(app: &AppHandle, is_long_break: bool) -> PanelForm {
    let (key, tray, top_right, fullscreen) = if is_long_break {
        (
            LONG_BREAK_WINDOW_KEY,
            LONG_BREAK_WINDOW_TRAY,
            LONG_BREAK_WINDOW_TOP_RIGHT,
            LONG_BREAK_WINDOW_FULLSCREEN,
        )
    } else {
        (
            REST_WINDOW_KEY,
            REST_WINDOW_TRAY,
            REST_WINDOW_TOP_RIGHT,
            REST_WINDOW_FULLSCREEN,
        )
    };
    panel_form_from_key(app, key, tray, top_right, fullscreen)
}

// 读休息时段窗口形态配置（quiet_hours 命中强制暂停时唤起窗口用）。
fn panel_form_by_quiet_window_config(app: &AppHandle) -> PanelForm {
    panel_form_from_key(
        app,
        QUIET_WINDOW_KEY,
        QUIET_WINDOW_TRAY,
        QUIET_WINDOW_TOP_RIGHT,
        QUIET_WINDOW_FULLSCREEN,
    )
}

// 已构建的托盘菜单项引用，用于语言切换时动态更新文案（MenuItem::set_text）。
struct TrayMenuItems {
    settings: MenuItem<tauri::Wry>,
    restart: MenuItem<tauri::Wry>,
    exit: MenuItem<tauri::Wry>,
}

// 语言切换时原地刷新托盘菜单文案，无需重建菜单或重启应用。
// 由 setup 末尾的 app-config-changed 监听器在 LANGUAGE_KEY 变化时调用。
pub fn refresh_menu_texts(app: &AppHandle) {
    let Some(state) = app.try_state::<Mutex<TrayMenuItems>>() else {
        return;
    };
    let Ok(items) = state.lock() else {
        return;
    };
    let lang = current_language(app);
    let _ = items
        .settings
        .set_text(menu_text(lang, "settings"));
    let _ = items.restart.set_text(menu_text(lang, "restart"));
    let _ = items.exit.set_text(menu_text(lang, "exit"));
}

/// 查询当前 Control 键是否被按下。
///
/// 用于托盘 Ctrl+左键单击：Tauri 2.11.2 的 TrayIconEvent::Click 不携带修饰键信息，
/// 故在左键单击触发瞬间主动查询键盘状态（查询式 API，与 shared/idle.rs 一致）。
///
/// 平台说明：
/// - macOS：CGEventSourceFlagsState 读取硬件级修饰键状态（CoreGraphics，无需辅助功能权限）。
/// - Windows：GetAsyncKeyState 异步读取按键状态，最高位为 1 表示当前按下（user32，无需权限）。
/// - Linux 等：托盘 show_menu 本就不支持，返回 false，维持原左键 toggle 行为。
fn control_key_pressed() -> bool {
    #[cfg(target_os = "macos")]
    {
        // kCGEventSourceStateHIDSystemState = 1（硬件级状态，最即时）；
        // kCGEventFlagMaskControl = 1 << 18 = 0x40000。
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGEventSourceFlagsState(state_id: i32, flags: u64) -> u64;
        }
        const STATE_HID: i32 = 1;
        const MASK_CONTROL: u64 = 1 << 18;
        unsafe { CGEventSourceFlagsState(STATE_HID, MASK_CONTROL) & MASK_CONTROL != 0 }
    }
    #[cfg(target_os = "windows")]
    {
        unsafe extern "system" {
            fn GetAsyncKeyState(v_key: i32) -> i16;
        }
        const VK_CONTROL: i32 = 0x11;
        // 返回值最高位（bit 15）为 1 表示按键当前处于按下状态，i16 解读即为负数。
        unsafe { GetAsyncKeyState(VK_CONTROL) < 0 }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    // CARGO_MANIFEST_DIR 在编译期由 cargo 注入，指向 src-tauri/ 根目录的绝对路径。
    // 用 concat!() 拼成 include_bytes! 的路径，让 icon 资源路径与 panel.rs 当前所在目录解耦。
    let icon = tauri::image::Image::from_bytes(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/icons/32x32.png"
    )))
    .expect("failed to load tray icon");

    // 右键菜单：系统设置 / 重启 / 退出。文案随当前语言偏好（app_config language，三态）。
    let lang = current_language(app.handle());
    let settings_item = MenuItem::with_id(
        app,
        "settings",
        menu_text(lang, "settings"),
        true,
        None::<&str>,
    )?;
    let restart_item = MenuItem::with_id(
        app,
        "restart",
        menu_text(lang, "restart"),
        true,
        None::<&str>,
    )?;
    let exit_item = MenuItem::with_id(
        app,
        "exit",
        menu_text(lang, "exit"),
        true,
        None::<&str>,
    )?;
    let menu = MenuBuilder::new(app)
        .items(&[&settings_item, &restart_item, &exit_item])
        .build()?;

    // tooltip 从配置文件 productName 读取：dev 构建（tauri.dev.conf.json）为 "Ocean Health Tick [DEV]"，
    // release 构建（tauri.conf.json）为 "Ocean Health Tick"，肉眼即可区分 dev/prod 产物。
    let tooltip = app
        .config()
        .product_name
        .as_deref()
        .unwrap_or("Ocean Health Tick");

    TrayIconBuilder::with_id("tray")
        .icon(icon)
        .tooltip(tooltip)
        .menu(&menu)
        // 关键：禁用左键弹菜单，保持左键 toggle panel 的现有行为（跨平台统一）。
        // macOS 默认左键会弹菜单，此处显式关闭；Linux 上为 no-op（左键本就触发 event）。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "settings" => {
                let app = app.clone();
                // show_settings_window 是 async（Windows 上同步调用会触发 wry#583 死锁），
                // 在同步的 menu event 闭包里用 async_runtime::spawn 调度。
                tauri::async_runtime::spawn(async move {
                    // 托盘是通用入口：不传分区，保留用户上次所在分区（窗口 hide 不销毁，hash 存活）。
                    if let Err(e) = crate::windows::settings::show_settings_window(app, None).await
                    {
                        log::warn!("show_settings_window from tray menu failed: {e}");
                    }
                });
            }
            "restart" => app.request_restart(),
            "exit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                // Ctrl+左键单击：与原生右键效果一致，弹出托盘菜单。Click 事件本身不带修饰键，
                // 故在触发瞬间查询 Control 键状态：按下则走弹菜单分支，否则走 toggle panel。
                if control_key_pressed() {
                    // with_inner_tray_icon → show_menu 是弹菜单的唯一入口；其内部用
                    // run_on_main_thread + 同步阻塞等待结果。本回调可能在主线程触发，
                    // 直接调用会死锁。故 clone 后在独立后台线程触发：后台线程阻塞等待，
                    // 主线程不被阻塞即可正常派发 show_menu。
                    let tray = tray.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = tray.with_inner_tray_icon(|inner| {
                            inner.show_menu();
                        }) {
                            log::warn!("failed to show tray menu: {e}");
                        }
                    });
                    return;
                }
                let app = tray.app_handle();
                match app.get_webview_window("panel") {
                    Some(panel) if panel.is_visible().unwrap_or(false) => {
                        // 可收起类窗口（点托盘隐藏）：Working 小窗 + Paused(Dnd) 免打扰「想看才看」
                        // 查看面板（与前端失焦自动隐藏的 hideOnBlur 守卫语义对齐）。
                        // 其余非 Working（提醒/休息/等待/手动或休息时段暂停）窗口常驻，不允许托盘点击隐藏
                        // （与 settings.rs 关闭恢复守卫语义对齐）；改走 show_panel 置顶唤起，
                        // 窗口被遮挡时点托盘可找回。
                        let phase = crate::timer::current_phase(app);
                        let dismissible = phase == Phase::Working
                            || (phase == Phase::Paused
                                && crate::timer::current_pause_source(app) == PauseSource::Dnd);
                        if dismissible {
                            let _ = panel.hide();
                        } else {
                            show_panel(app);
                        }
                    }
                    _ => show_panel(app),
                }
            }
        })
        .build(app)?;

    // 保存菜单项句柄，供 refresh_menu_texts 在语言切换时动态更新文案。
    app.manage(Mutex::new(TrayMenuItems {
        settings: settings_item,
        restart: restart_item,
        exit: exit_item,
    }));

    // panel 窗口形态受管状态（初始贴托盘；启动即 Working，首次休息时监听器按配置写入）。
    app.manage(PanelFormState(Mutex::new(PanelFormInner {
        form: PanelForm::Tray,
        last_small_height: DEFAULT_PANEL_HEIGHT,
    })));

    let app_handle = app.handle().clone();
    // 启动即 fresh_working（L1），初始恒为 Working；显式设置一次避免启动瞬间显示默认 32x32.png（G3）。
    set_tray_icon_by_phase(&app_handle, Phase::Working);

    // 订阅 phase-changed：phase 切换时同步切换托盘图标（G2），
    // 并在进入非 Working 阶段时主动唤起 panel 窗口（常驻提醒）。
    // 窗口形态按休息类型分派（is_long_break 由 timer.rs 在 on_work_done 归零时刻提前写入，
    // Alerting 休息前提醒阶段即为本轮真实类型，非上轮残留）：
    //   - Alerting / Breaking：读对应配置（长休息 → long_break_window，正常 → rest_window），
    //     topRight → 屏幕右上角，fullscreen → macOS 原生全屏（Alerting 起接管，
    //     期间热改配置 Breaking 读到新值即热切换），tray / 非法值 → 贴托盘。
    //   - Waiting：沿用当前形态（全屏保持至确认返回）。
    //   - Paused：休息时段（Quiet）触发按 quiet_window 配置分派形态；免打扰（Dnd）收起不弹；
    //     手动暂停沿用当前形态不跳变。
    //   - Working：重置贴托盘（sync 内部触发退出全屏 + 恢复小窗尺寸与托盘定位）。
    // 闭包持有 owned AppHandle（Clone + Send + Sync），满足 Listener 要求的 'static。
    app.handle().listen(
        crate::shared::events::EVENT_PHASE_CHANGED,
        move |event| {
            let payload = serde_json::from_str::<TimerStatePayload>(event.payload()).ok();
            let phase = payload.as_ref().map(|p| p.phase);
            let prev_phase = payload.as_ref().and_then(|p| p.prev_phase);
            match phase {
                Some(phase) => {
                    set_tray_icon_by_phase(&app_handle, phase);
                    match phase {
                        Phase::Alerting | Phase::Breaking => {
                            let is_long = payload.as_ref().is_some_and(|p| p.is_long_break);
                            sync_panel_form(
                                &app_handle,
                                panel_form_by_window_config(&app_handle, is_long),
                            );
                            show_panel(&app_handle);
                        }
                        Phase::Waiting => {
                            // 沿用当前形态（全屏形态下即保持全屏，至确认返回进 Working 才退出）
                            show_panel(&app_handle);
                        }
                        Phase::Paused => {
                            // 按暂停来源分派（含 Paused→Paused 来源切换事件，timer.rs 时段
                            // quiet↔dnd 重叠边界发出；单事件单分支处理形态，不闪窗）：
                            //   - Quiet（休息时段，打断式）：按 quiet_window 配置分派形态 + 弹窗常驻
                            //   - Dnd（免打扰，非打断式）：收起不弹 + 显式归位 Tray 形态——
                            //     若从 Alerting 全屏被切走，仅 hide 会留下「Fullscreen 形态的隐藏窗」，
                            //     托盘点击时会 show 出全屏窗；sync(Tray) 幂等退出全屏恢复小窗定位
                            //   - Manual（手动暂停）：沿用当前形态不跳变，弹窗常驻
                            match payload.as_ref().map(|p| p.pause_source) {
                                Some(PauseSource::Quiet) => {
                                    sync_panel_form(
                                        &app_handle,
                                        panel_form_by_quiet_window_config(&app_handle),
                                    );
                                    show_panel(&app_handle);
                                }
                                Some(PauseSource::Dnd) => {
                                    sync_panel_form(&app_handle, PanelForm::Tray);
                                    hide_panel(&app_handle);
                                }
                                Some(PauseSource::Manual) | None => {
                                    show_panel(&app_handle);
                                }
                            }
                        }
                        Phase::Working => {
                            // sync 到 Tray 统一处理：Fullscreen 时退出全屏并显式恢复小窗尺寸
                            // （exit 内部含贴托盘定位）。幂等：已是 Tray 时零副作用。
                            sync_panel_form(&app_handle, PanelForm::Tray);
                            // 时段暂停结束自动重开工作（quiet/dnd 共用）：不显示工作窗口，收起回归工作流。
                            let auto_resumed = payload.as_ref().is_some_and(|p| p.auto_resumed);
                            match prev_phase {
                                // 从休息侧进入 Working（跳过 / 我回来了 / 休息窗口重置）：
                                // 收起窗口回归工作流，需要时点托盘唤起。
                                Some(Phase::Breaking)
                                | Some(Phase::Waiting)
                                | Some(Phase::Alerting) => {
                                    hide_panel(&app_handle);
                                }
                                // 时段结束自动恢复（Paused → Working）：同样收起窗口，不显示工作状态。
                                Some(Phase::Paused) if auto_resumed => {
                                    hide_panel(&app_handle);
                                }
                                // Working 自身重置 / 手动 Paused 恢复 / 启动初始化：保持原可见状态。
                                // 可见时原地重定位回托盘位（如 topRight 点完按钮滞留右上角），
                                // 隐藏则保持隐藏（衔接失焦自动隐藏机制）。不 show / 不 set_focus。
                                _ => {
                                    if let Some(panel) = app_handle.get_webview_window("panel") {
                                        if panel.is_visible().unwrap_or(false) {
                                            if let Some(tray) = app_handle.tray_by_id("tray") {
                                                position_panel(&tray, &panel);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                None => log::warn!("phase-changed payload parse failed, skip tray icon update"),
            }
        },
    );

    // 监听 app-config-changed：语言偏好变化时刷新托盘菜单文案（运行时切换，无需重启）。
    let lang_handle = app.handle().clone();
    app.listen(
        crate::shared::events::EVENT_APP_CONFIG_CHANGED,
        move |event| {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(event.payload()) else {
                return;
            };
            if value.get("key").and_then(|v| v.as_str()) == Some(LANGUAGE_KEY) {
                refresh_menu_texts(&lang_handle);
            }
        },
    );

    Ok(())
}

// 按 phase 选托盘图标字节。dev build（pnpm tauri dev）用带红色 DEV 圆点的 -dev 变体，
// release build 用原图标。用 #[cfg] 而非 cfg!() 运行时分支：include_bytes! 是编译期展开，
// cfg!() 会把两套 PNG 都编进二进制；#[cfg] 在编译期二选一，release 产物完全不含 dev 资源。
#[cfg(debug_assertions)]
fn phase_icon_bytes(phase: Phase) -> &'static [u8] {
    match phase {
        Phase::Working => include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/icons/tray/working-dev.png"
        )),
        Phase::Alerting | Phase::Breaking | Phase::Waiting | Phase::Paused => {
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/icons/tray/nonworking-dev.png"
            ))
        }
    }
}

#[cfg(not(debug_assertions))]
fn phase_icon_bytes(phase: Phase) -> &'static [u8] {
    match phase {
        Phase::Working => include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/icons/tray/working.png"
        )),
        Phase::Alerting | Phase::Breaking | Phase::Waiting | Phase::Paused => {
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/icons/tray/nonworking.png"
            ))
        }
    }
}

// 按 phase 切换托盘图标（G2）。在 setup 末尾订阅 phase-changed 时调用；
// G3 落地后启动时也会显式调一次（Phase::Working）。
// 失败容错：图片解码 / tray 缺失 / set_icon 失败均 log::warn! 并返回，不 panic
// （托盘图标切换是非关键路径，不应阻塞状态机主流程）。
pub fn set_tray_icon_by_phase(app: &AppHandle, phase: Phase) {
    let icon = match tauri::image::Image::from_bytes(phase_icon_bytes(phase)) {
        Ok(img) => img,
        Err(e) => {
            log::warn!("decode tray icon failed for {:?}: {e}", phase);
            return;
        }
    };
    let Some(tray) = app.tray_by_id("tray") else {
        log::warn!(
            "tray not found when set_tray_icon_by_phase({:?})",
            phase
        );
        return;
    };
    if let Err(e) = tray.set_icon(Some(icon)) {
        log::warn!("set_icon failed for {:?}: {e}", phase);
    }
}

// 强制显示 panel（H4）：panel 不存在 → create；存在 → position + show + set_focus。
// phase-changed 监听器在进入非 Working 阶段时调用此函数唤起窗口。
// 定位按当前 PanelForm 分派（Tray 贴托盘 / TopRight 屏幕右上角 / Fullscreen no-op）。
pub fn show_panel(app: &AppHandle) {
    let Some(tray) = app.tray_by_id("tray") else {
        log::warn!("tray not found when show_panel");
        return;
    };
    if let Some(panel) = app.get_webview_window("panel") {
        position_panel_by_form(&tray, &panel);
        let _ = panel.show();
        let _ = panel.set_focus();
    } else {
        create_panel(app, &tray);
    }
}

// 收起 panel（若存在）：show_panel 的对仗入口。窗口不存在（尚未懒创建）为 no-op。
// 调用点：Paused(Dnd) 免打扰收起、从休息侧/时段结束进 Working 的收起。
pub fn hide_panel(app: &AppHandle) {
    if let Some(panel) = app.get_webview_window("panel") {
        let _ = panel.hide();
    }
}

// 按当前形态分派定位入口（fit_panel / show_panel / create_panel 共用）。
fn position_panel_by_form(tray: &tauri::tray::TrayIcon, panel: &tauri::WebviewWindow) {
    match current_panel_form(tray.app_handle()) {
        PanelForm::TopRight => position_panel_top_right(tray, panel),
        PanelForm::Tray => position_panel(tray, panel),
        // 伪全屏的铺满尺寸/位置由 enter/create 一次性设定，周期性重定位会破坏铺满状态，跳过。
        PanelForm::Fullscreen => {}
    }
}

// 右上角形态定位：panel 放到托盘所在屏 work_area 右上角（与托盘形态同屏，多屏行为可预期），
// 距边缘留 PANEL_TOP_RIGHT_MARGIN。锚点为右上角，窗口高度不参与定位计算
// （高度自适应由 fit_panel 重设尺寸后再次调用本函数完成）。
// 显示器探测失败（tray rect 缺失 / 不在任何屏内）回退贴托盘定位。
fn position_panel_top_right(tray: &tauri::tray::TrayIcon, panel: &tauri::WebviewWindow) {
    let Some(monitor) = find_monitor_for_tray(tray.app_handle(), "tray") else {
        log::warn!("monitor not found for topRight panel, fallback to tray position");
        position_panel(tray, panel);
        return;
    };
    let x = monitor.wa_x + monitor.wa_width - PANEL_WIDTH - PANEL_TOP_RIGHT_MARGIN;
    let y = monitor.wa_y + PANEL_TOP_RIGHT_MARGIN;
    let _ = panel.set_position(LogicalPosition::new(x, y));
}

fn position_panel(tray: &tauri::tray::TrayIcon, panel: &tauri::WebviewWindow) {
    if let Ok(Some(rect)) = tray.rect() {
        let sf = panel.scale_factor().unwrap_or(1.0);

        let monitor = find_monitor_for_rect(tray.app_handle(), &rect);
        let sf = monitor.as_ref().map_or(sf, |m| m.scale_factor);

        let pos = rect.position.to_logical::<f64>(sf);
        let size = rect.size.to_logical::<f64>(sf);

        let panel_height = panel
            .inner_size()
            .map(|s| s.to_logical::<f64>(sf).height)
            .unwrap_or(DEFAULT_PANEL_HEIGHT);

        let (x, y) = if let Some(m) = &monitor {
            compute_panel_position(
                m,
                pos.x,
                pos.y,
                size.width,
                size.height,
                panel_height,
            )
        } else {
            (pos.x, pos.y + size.height)
        };

        let _ = panel.set_position(Position::Logical(LogicalPosition::new(x, y)));
    }
}

fn compute_panel_position(
    monitor: &MonitorInfo,
    icon_x: f64,
    icon_y: f64,
    icon_w: f64,
    icon_h: f64,
    panel_height: f64,
) -> (f64, f64) {
    let (x, y) = match detect_taskbar_edge(monitor, icon_x, icon_y, icon_w, icon_h) {
        TaskbarEdge::Top => (icon_x, icon_y + icon_h),
        TaskbarEdge::Bottom => (icon_x, icon_y - panel_height),
        TaskbarEdge::Left => (icon_x + icon_w, icon_y),
        TaskbarEdge::Right => (icon_x - PANEL_WIDTH, icon_y),
    };

    // 防御性上界塌缩：panel_height 可能大于 work_area 高度——典型时序是退出全屏瞬间
    // （跳过/结束休息）set_size 尚未生效或 fit_panel 收到铺满期高度，inner_size 仍为铺满
    // 整屏的窗口高度，此时 wa_y + wa_height - panel_height < wa_y，clamp(min>max) 直接
    // panic。上界塌缩到工作区下/右边界保证 clamp 恒合法，后续 fit_panel 收到前端真实
    // 小窗高度后再修正定位，仅存在一帧级的过渡偏移。
    let x = x.clamp(
        monitor.wa_x,
        (monitor.wa_x + monitor.wa_width - PANEL_WIDTH).max(monitor.wa_x),
    );
    let y = y.clamp(
        monitor.wa_y,
        (monitor.wa_y + monitor.wa_height - panel_height).max(monitor.wa_y),
    );

    (x, y)
}

#[tauri::command]
#[specta::specta]
pub fn fit_panel(app: tauri::AppHandle, height: f64) -> Result<(), String> {
    // 伪全屏期间前端 ResizeObserver 仍会触发（遮罩布局挂载即触发一次），此处为权威抑制点：
    // set_size(240, N) 会把铺满的窗口缩成 240 宽。退出全屏后布局回小窗，RO 自然再触发恢复高度。
    if current_panel_form(&app) == PanelForm::Fullscreen {
        return Ok(());
    }
    let panel = app
        .get_webview_window("panel")
        .ok_or("panel not found")?;
    let tray = app.tray_by_id("tray").ok_or("tray not found")?;

    let _ = panel.set_size(LogicalSize::new(PANEL_WIDTH, height));
    position_panel_by_form(&tray, &panel);

    Ok(())
}

// 供 panel 前端 mount 时拉取当前形态（webview 重载收不到历史 panel-form-changed 事件）。
#[tauri::command]
#[specta::specta]
pub fn get_panel_form(app: tauri::AppHandle) -> PanelForm {
    current_panel_form(&app)
}

fn create_panel(app: &tauri::AppHandle, tray: &tauri::tray::TrayIcon) {
    let panel = WebviewWindowBuilder::new(app, "panel", WebviewUrl::App("panel.html".into()))
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .shadow(false)
        // 窗口不进任务栏与 Alt+Tab（Windows/Linux），macOS 上为 no-op（Dock 由 ActivationPolicy 控制）。
        .skip_taskbar(true)
        .focused(true)
        .inner_size(PANEL_WIDTH, DEFAULT_PANEL_HEIGHT);

    if let Ok(w) = panel.build() {
        // 全屏形态下的懒创建有两条路径，均在此直接以铺满完整边界的尺寸建窗（伪全屏）：
        //   - 首次休息（启动后窗口从未创建）：监听器 sync(Fullscreen) 时窗口不存在，
        //     enter_panel_fullscreen 对 not-found 返回 true 保持状态，落到此处铺满；
        //   - 全屏中途 webview 意外销毁重建。
        // 铺满失败（显示器探测不到）回退贴托盘小窗。前端 mount 后 getPanelForm 拉到 Fullscreen
        // 切全屏布局。
        if current_panel_form(app) == PanelForm::Fullscreen {
            if let Some(monitor) = find_monitor_for_tray(app, "tray") {
                let _ = w.set_size(LogicalSize::new(monitor.width, monitor.height));
                let _ = w.set_position(LogicalPosition::new(monitor.x, monitor.y));
            } else {
                position_panel_by_form(tray, &w);
            }
        } else {
            position_panel_by_form(tray, &w);
        }
        let _ = w.show();
        let _ = w.set_focus();
    }
}
