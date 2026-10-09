mod shared;
mod timer;
mod windows;

use tauri_specta::{Builder, collect_commands};

#[tauri::command]
#[specta::specta]
fn exit_app(app: tauri::AppHandle) {
    app.exit(0);
}

// 集中注册所有 IPC 命令 + 跨语言共享常量到 tauri-specta Builder。
// run()（注册 invoke handler）与 bin/export_bindings.rs（生成 TS 绑定）共用此函数，
// 保证命令/常量清单单一来源，避免两份注册表漂移。
pub fn build_specta_builder() -> Builder<tauri::Wry> {
    use crate::shared::app_config as ac;
    use crate::shared::events as ev;
    use crate::shared::types::{AppConfigChangedPayload, QuietHourPeriod, YesNo};
    use crate::timer as t;

    // quiet_hours 双形态一致性守卫：结构化 DEFAULT_QUIET_HOURS 与 read_quiet_hours
    // 运行时用的 DEFAULT_QUIET_HOURS_JSON 必须等值，漂移则启动即 panic（早暴露）。
    debug_assert!(
        serde_json::to_string(&t::DEFAULT_QUIET_HOURS).unwrap() == t::DEFAULT_QUIET_HOURS_JSON
    );
    // dnd_hours 双形态一致性守卫（对称防护，默认空数组）。
    debug_assert!(
        serde_json::to_string(&t::DEFAULT_DND_HOURS).unwrap() == t::DEFAULT_DND_HOURS_JSON
    );

    Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            exit_app,
            windows::settings::show_settings_window,
            windows::panel::fit_panel,
            windows::panel::get_panel_form,
            shared::app_config::get_app_config,
            shared::app_config::set_app_config,
            timer::get_timer_state,
            timer::start_work,
            timer::confirm_break,
            timer::confirm_return,
            timer::toggle_pause,
            timer::reset,
            timer::manual_break,
            timer::skip_break,
        ])
        // AppConfigChangedPayload 不出现在任何 command 签名中（仅 set_app_config 内部 emit），
        // 用 typ 显式注册，让 specta 把它导出到 bindings.ts 供前端 listen 复用。
        .typ::<AppConfigChangedPayload>()
        // YesNo enum 同样不在 command 签名中（read_*_enabled 内部 parse 消费），
        // 注册以让前端 bindings.ts 拿到字面量联合类型 "Y" | "N"。
        .typ::<YesNo>()
        // QuietHourPeriod 结构体仅出现在下方常量导出中（read_quiet_hours 用局部反序列化），
        // 显式注册让 bindings.ts 导出类型供 DEFAULT_QUIET_HOURS 常量引用。
        .typ::<QuietHourPeriod>()
        // ==================== 跨语言共享常量（SSOT：Rust）====================
        // tauri-specta rc.25 起原生支持 constant 导出，生成 `export const ... as const`。
        // 前端 appConfig.ts / events.ts 对应常量已降级为从此处 re-export。
        // 导出名 = 前端旧命名（TS 消费方零改动）；修改值必须重跑 `pnpm gen:bindings`。
        // —— 事件名（SSOT：shared/events.rs）——
        .constant("EVENT_TIMER_TICK", ev::EVENT_TIMER_TICK)
        .constant("EVENT_PHASE_CHANGED", ev::EVENT_PHASE_CHANGED)
        .constant(
            "EVENT_APP_CONFIG_CHANGED",
            ev::EVENT_APP_CONFIG_CHANGED,
        )
        .constant(
            "EVENT_SETTINGS_NAVIGATE",
            ev::EVENT_SETTINGS_NAVIGATE,
        )
        .constant(
            "EVENT_PANEL_FORM_CHANGED",
            ev::EVENT_PANEL_FORM_CHANGED,
        )
        // —— 配置 key：timer.rs 业务（清单以本注册列表为准）——
        .constant("WORK_DURATION_KEY", t::KEY_WORK_DURATION)
        .constant("BREAK_DURATION_KEY", t::KEY_BREAK_DURATION)
        .constant(
            "LONG_BREAK_ENABLED_KEY",
            t::KEY_LONG_BREAK_ENABLED,
        )
        .constant(
            "LONG_BREAK_INTERVAL_KEY",
            t::KEY_LONG_BREAK_INTERVAL,
        )
        .constant(
            "LONG_BREAK_DURATION_KEY",
            t::KEY_LONG_BREAK_DURATION,
        )
        .constant("REST_CONFIRM_KEY", t::KEY_REST_CONFIRM)
        .constant("REST_END_CONFIRM_KEY", t::KEY_REST_END_CONFIRM)
        .constant("PAUSE_ON_IDLE_KEY", t::KEY_PAUSE_ON_IDLE)
        .constant(
            "IDLE_PAUSE_THRESHOLD_KEY",
            t::KEY_IDLE_PAUSE_THRESHOLD,
        )
        .constant("QUIET_HOURS_KEY", t::KEY_QUIET_HOURS)
        .constant(
            "QUIET_HOURS_ENABLED_KEY",
            t::KEY_QUIET_HOURS_ENABLED,
        )
        .constant("DND_HOURS_KEY", t::KEY_DND_HOURS)
        .constant("DND_HOURS_ENABLED_KEY", t::KEY_DND_HOURS_ENABLED)
        .constant("REMINDERS_KEY", t::KEY_REMINDERS)
        .constant("BREAK_SKIP_MAX_KEY", t::KEY_BREAK_SKIP_MAX)
        // —— 配置 key + 默认值：shared/app_config.rs（i18n / 窗口形态）——
        .constant("LANGUAGE_KEY", ac::LANGUAGE_KEY)
        .constant("DEFAULT_LANGUAGE", ac::DEFAULT_LANGUAGE)
        .constant("REST_WINDOW_KEY", ac::REST_WINDOW_KEY)
        .constant("DEFAULT_REST_WINDOW", ac::DEFAULT_REST_WINDOW)
        .constant("LONG_BREAK_WINDOW_KEY", ac::LONG_BREAK_WINDOW_KEY)
        .constant(
            "DEFAULT_LONG_BREAK_WINDOW",
            ac::DEFAULT_LONG_BREAK_WINDOW,
        )
        .constant("QUIET_WINDOW_KEY", ac::QUIET_WINDOW_KEY)
        .constant("DEFAULT_QUIET_WINDOW", ac::DEFAULT_QUIET_WINDOW)
        .constant("LAUNCH_AT_LOGIN_KEY", ac::LAUNCH_AT_LOGIN_KEY)
        .constant(
            "DEFAULT_LAUNCH_AT_LOGIN",
            ac::DEFAULT_LAUNCH_AT_LOGIN,
        )
        // —— 默认值 / 范围（数值型）——
        .constant(
            "DEFAULT_WORK_DURATION",
            t::DEFAULT_WORK_DURATION_MIN,
        )
        .constant(
            "DEFAULT_BREAK_DURATION",
            t::DEFAULT_BREAK_DURATION_MIN,
        )
        .constant(
            "DEFAULT_LONG_BREAK_ENABLED",
            t::DEFAULT_LONG_BREAK_ENABLED,
        )
        .constant(
            "DEFAULT_LONG_BREAK_INTERVAL",
            t::DEFAULT_LONG_BREAK_INTERVAL,
        )
        .constant(
            "DEFAULT_LONG_BREAK_DURATION",
            t::DEFAULT_LONG_BREAK_DURATION_MIN,
        )
        .constant("DEFAULT_REST_CONFIRM", t::DEFAULT_REST_CONFIRM)
        .constant(
            "DEFAULT_QUIET_HOURS_ENABLED",
            t::DEFAULT_QUIET_HOURS_ENABLED,
        )
        .constant(
            "DEFAULT_DND_HOURS_ENABLED",
            t::DEFAULT_DND_HOURS_ENABLED,
        )
        .constant(
            "DEFAULT_REST_END_CONFIRM",
            t::DEFAULT_REST_END_CONFIRM,
        )
        .constant("DEFAULT_PAUSE_ON_IDLE", t::DEFAULT_PAUSE_ON_IDLE)
        .constant(
            "DEFAULT_IDLE_PAUSE_THRESHOLD",
            t::DEFAULT_IDLE_PAUSE_THRESHOLD,
        )
        .constant(
            "MIN_IDLE_PAUSE_THRESHOLD",
            t::MIN_IDLE_PAUSE_THRESHOLD,
        )
        .constant(
            "MAX_IDLE_PAUSE_THRESHOLD",
            t::MAX_IDLE_PAUSE_THRESHOLD,
        )
        .constant(
            "DEFAULT_BREAK_SKIP_MAX",
            t::DEFAULT_BREAK_SKIP_MAX,
        )
        .constant("MIN_BREAK_SKIP_MAX", t::MIN_BREAK_SKIP_MAX)
        .constant("MAX_BREAK_SKIP_MAX", t::MAX_BREAK_SKIP_MAX)
        // —— 默认值：结构化（quiet_hours / dnd_hours / 提醒文案）——
        .constant("DEFAULT_QUIET_HOURS", t::DEFAULT_QUIET_HOURS)
        .constant("DEFAULT_DND_HOURS", t::DEFAULT_DND_HOURS)
        .constant(
            "DEFAULT_HEALTH_TEXTS",
            shared::reminder_texts::DEFAULT_HEALTH_TEXTS,
        )
        .constant(
            "DEFAULT_WHISPER_TEXTS",
            shared::reminder_texts::DEFAULT_WHISPER_TEXTS,
        )
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let specta_builder = build_specta_builder();

    // 单实例插件必须第一个注册（官方要求）：需早于其它插件初始化，才能可靠拦截二次启动。
    // 二次启动的进程拿锁失败即退出，已有实例收到回调 → 唤起 panel 主窗口
    // （复用 show_panel：按当前形态定位 + show + focus，窗口不存在则重建）。
    // dev/release identifier 不同（tauri.dev.conf.json 的 .dev 后缀），单实例锁按
    // identifier 区分 → dev 调试实例与 release 实例仍可并存，不破坏现有隔离设计。
    // #[cfg(desktop)] 重绑定：该插件仅桌面平台有效，移动端构建不编译（lib.rs 保留 mobile
    // 入口），且无需 mut（非 desktop 目标不会产生 unused_mut 警告）。
    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(
        |app, _argv, _cwd| {
            windows::panel::show_panel(app);
        },
    ));

    builder
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        // 开机自启动：dev/release 用不同 app_name 隔离登录项标识（macOS LaunchAgent
        // plist 文件名 / Windows Registry 键名），避免 dev 调试注册覆盖 release 的登录项。
        // ⚠️ 勿用 `tauri build --debug`：会得到 release identifier + dev plist 名的组合，
        // 启动回写污染 release 共享 DB 镜像（数据目录隔离依赖 tauri.dev.conf.json）。
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .app_name(if cfg!(debug_assertions) {
                    "ocean-health-tick-dev"
                } else {
                    "ocean-health-tick"
                })
                .build(),
        )
        .invoke_handler(specta_builder.invoke_handler())
        .setup(move |app| {
            // macOS 隐藏 Dock 图标：将应用激活策略设为 Accessory（代理应用），
            // 应用不再出现在程序坞和应用菜单栏，只保留顶部状态栏托盘图标。
            // 该 API 仅 macOS 生效；Windows/Linux 任务栏隐藏由各窗口的 skip_taskbar(true) 负责。
            #[cfg(target_os = "macos")]
            {
                let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }

            // 日志插件：dev/release 均注册。
            // dev：Info + Stdout（终端实时观察）+ LogDir；release：Warn + LogDir 并启用轮转，方便生产排障。
            use tauri_plugin_log::{Target, TargetKind};
            let log_plugin = if cfg!(debug_assertions) {
                tauri_plugin_log::Builder::default()
                    .level(log::LevelFilter::Info)
                    .targets([
                        Target::new(TargetKind::Stdout),
                        Target::new(TargetKind::LogDir { file_name: None }),
                    ])
                    .build()
            } else {
                tauri_plugin_log::Builder::default()
                    .level(log::LevelFilter::Warn)
                    .targets([Target::new(TargetKind::LogDir {
                        file_name: None,
                    })])
                    // 1 MiB/文件，保留最近 1 份（旧的重命名带日期），总量 ~1 MiB 有界
                    .max_file_size(1_048_576)
                    .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(1))
                    .build()
            };
            app.handle().plugin(log_plugin)?;

            // app_config 先于 panel::setup：panel 构建托盘菜单时需读 AppConfigState 解析语言偏好，
            // 否则首次启动会 fallback 到系统语言而忽略用户存的 language 偏好。
            shared::app_config::init(app)?;
            // autostart 紧随 app_config：启动回写需写 DB 镜像；须在设置窗口（懒创建）前完成对齐。
            shared::autostart::init(app.handle());
            windows::panel::setup(app)?;
            timer::init(app)?;

            specta_builder.mount_events(app);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
