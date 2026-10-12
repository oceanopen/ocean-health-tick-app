use rand::RngExt;
use rusqlite::Connection;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Listener, Manager, State};

use crate::shared::app_config::{AppConfigState, read_app_config_conn, write_app_config_conn};
use crate::shared::reminder_texts::{DEFAULT_HEALTH_TEXTS, DEFAULT_WHISPER_TEXTS};
use crate::shared::time::{now_epoch, now_hhmm, today_string};
use crate::shared::types::{PauseSource, Phase, QuietHourPeriod, TimerStatePayload, YesNo};

// ============================================================
// 内部状态
// ============================================================

struct TimerInner {
    /// 当前状态机阶段（FSM 5 状态之一）。
    phase: Phase,
    /// working 阶段的目标结束 epoch 秒。绝对时间机制：每秒 tick 时
    /// `remaining = target_epoch - now`，避免系统休眠 / 时钟跳变导致的累积漂移。
    /// 仅 working 阶段有意义（breaking 用 remaining_seconds 递减）。
    target_epoch: i64,
    /// 当前 phase 的剩余秒数（working 与 breaking 共用此字段）。
    /// - working 阶段：每秒 tick 时由 target_epoch 重算（不持久化）
    /// - breaking 阶段：递减（M2 实现）
    ///
    /// 是 emit 给前端的核心字段之一（圆环 + MM:SS）。
    remaining_seconds: i64,
    /// 本轮总秒数。working 阶段 = work_duration × 60；breaking 阶段 = break 或 long_break 时长。
    /// UI 圆环进度分母：progress = remaining_seconds / total_seconds。
    total_seconds: i64,
    /// 进入 Paused 前的阶段，恢复时回到这里（M2 toggle_pause 使用）。
    /// 例如 working → paused 时存 Working，恢复时回到 working。
    paused_phase: Option<Phase>,
    /// 当前 Paused 的触发来源（manual / quiet / dnd），仅 phase == Paused 时有意义。
    ///   - 休息/免打扰时段命中 → apply_enter_pause_window 写入 Quiet/Dnd；
    ///     时段全部结束时自动 start_work 新一轮（作废当前轮，不恢复原进度）
    ///   - 用户手动 → apply_enter_paused 写入 Manual；不自动恢复，等用户点继续
    ///   - apply_resume 对 Quiet/Dnd 返回 false（时段暂停期间忽略 toggle_pause）
    ///   - 重叠边界来源切换（quiet↔dnd）由 tick 判定块直接更新本字段，不切 phase
    paused_source: PauseSource,
    /// 已完成的工作-休息轮数。M3 长休息判定输入：
    /// `completed_cycles > 0 && completed_cycles % long_break_interval == 0`。
    /// 正常完成 on_break_done 与真正跳过休息（skip_break）均递增——跳过也占用一轮休息名额。
    completed_cycles: u32,
    /// 跳过休息的累计点击次数。M2 skip_break 中递增，达到 break_skip_max 次才真正跳过；
    /// 每次 start_break 进入 breaking 时清零。前端按钮显示「跳过 (n/max)」。
    break_skip_count: u32,
    /// 今日累计「真正跳过休息」次数（break_skip_count 达到 break_skip_max 才计一次）。
    /// 持久化到 app_config 表 today_skip_count（{date,count} JSON），跨天自动归零；
    /// init / 跨天时由 fresh_working 从 DB 读取（date 校验自带归零），skip_break 真正跳过时 +1 并回写。
    /// 与 break_skip_count（防误触、不持久化）严格区分。payload 暴露给 AlertingView 显隐警示横幅。
    today_skip_count: u32,
    /// 当前随机抽取的随笔心语文案（来自 reminders 配置的 whisper 池）。M2 的 on_work_done 中抽取；
    /// working / waiting / paused 阶段为空字符串。
    current_whisper_reminder: String,
    /// 当前随机抽取的健康提醒文案（来自 reminders 配置的 health 池）。
    /// breaking 阶段在 BreakingView 以绿色横幅展示；alerting 已抽取但不展示；其他阶段为空字符串。
    current_health_reminder: String,
    /// 本次休息是否为长休息。on_work_done（归零时刻，Alerting 提前写入）与 manual_break 判定，
    /// apply_start_break 接收传入值；从 Alerting 提醒到 Waiting 的整条链路均携带本值：
    /// 前端 AlertingView/WaitingView 据此切换标题，panel.rs 据此分派窗口形态。进 Working 前清零。
    is_long_break: bool,
    /// 当前状态对应的日期（yyyy-MM-dd，本地时区）。
    /// 运行时跨天判断：tick 中若 save_date != today，触发 fresh_working 开始新一轮。
    save_date: String,
    /// Breaking 内部「鼠标活动软暂停」标志。true 时不递减 remaining（倒计时冻结）。
    /// 不切 phase（始终 Breaking），前端 BreakingView 据此显隐活动提示横幅。
    break_paused: bool,
    /// 进入本次 Breaking 的 epoch 秒，用于启动宽限期判定（进入后 4 秒内不触发活动暂停，
    /// 避免用户点击「开始休息」按钮的鼠标余波误判）。apply_start_break 时设置。
    break_start_epoch: Option<i64>,
    /// 上次 skip_break 点击的 epoch 秒，用于跳过宽限期判定（点击后 3 秒内不触发活动暂停，
    /// 避免点击「跳过」按钮的鼠标活动被误判为「在用电脑」）。
    last_skip_epoch: Option<i64>,
    /// 用户在休息时段（Quiet）弹窗点击「跳过本次提醒」时命中的时段 (start, end)。
    /// 内存字段（不持久化，重启自然失效）：panel.rs 的 Quiet 弹窗分支据此抑制自动唤起；
    /// 计时暂停与时段结束自动恢复行为完全不变。run_timer_loop 每秒校验，一旦不再处于
    /// 任何启用的 quiet 时段内（时段结束 / 开关关闭 / 配置清空）即清空——下个时段恢复提醒。
    quiet_skip_period: Option<(String, String)>,
}

// Tauri 全局状态容器：在 init 中通过 app.manage(TimerState) 注册，
// 之后 command 函数（如 get_timer_state）通过 State<'_, TimerState> 注入访问。
// 内部用 Arc<Mutex<TimerInner>> 而非直接 Mutex：因为 1Hz 定时器循环（spawn 的 async 任务）
// 和 app-config-changed handler（listen 闭包）都需要 owned 句柄跨上下文共享同一份状态。
// 与 AppConfigState 的 newtype 风格保持一致。
pub struct TimerState {
    inner: Arc<Mutex<TimerInner>>,
}

// ============================================================
// 业务配置 key + 默认值（SSOT：本文件）
// ============================================================
//
// 下方 key / 默认值 / 范围常量是唯一可信源，经 lib.rs build_specta_builder().constant()
// 自动导出到 src/shared/bindings.ts（`export const ... as const`），前端 appConfig.ts
// 直接 re-export，两端永不漂移；修改本区常量后必须跑 `pnpm gen:bindings` 重新生成。
// 历史包袱说明：常量名沿用前端 appConfig.ts 旧命名（不带 _MIN 后缀等），DB 已存量的
// key 字符串不可变，重命名导出名需同步前端 re-export 层。

pub const KEY_WORK_DURATION: &str = "work_duration";
pub const DEFAULT_WORK_DURATION_MIN: u32 = 30;

pub const KEY_BREAK_DURATION: &str = "break_duration";
pub const DEFAULT_BREAK_DURATION_MIN: u32 = 1;

pub const KEY_LONG_BREAK_ENABLED: &str = "long_break_enabled";
pub const DEFAULT_LONG_BREAK_ENABLED: bool = true;

pub const KEY_LONG_BREAK_INTERVAL: &str = "long_break_interval";
pub const DEFAULT_LONG_BREAK_INTERVAL: u32 = 2;

pub const KEY_LONG_BREAK_DURATION: &str = "long_break_duration";
pub const DEFAULT_LONG_BREAK_DURATION_MIN: u32 = 5;

pub const KEY_REST_CONFIRM: &str = "rest_confirm";
pub const DEFAULT_REST_CONFIRM: bool = true;

// 休息后确认：true 休息结束进 Waiting 等用户点「我回来了」；false 休息结束自动进入
// Working（跳过 Waiting）。与前端 REST_END_CONFIRM_KEY / DEFAULT_REST_END_CONFIRM 对齐。
// 场景：run_timer_loop Breaking 归零时刻现读分流。时段暂停优先：时段判定在 phase 推进前，
// Breaking 被强制切 Paused 不会走到归零分支，故无需在读取处再做时段判断。
pub const KEY_REST_END_CONFIRM: &str = "rest_end_confirm";
pub const DEFAULT_REST_END_CONFIRM: bool = true;

pub const KEY_PAUSE_ON_IDLE: &str = "pause_on_idle";
// 离开（锁屏/休眠/AFK）时冻结工作倒计时。与前端 DEFAULT_PAUSE_ON_IDLE 对齐。
// read_pause_on_idle 容错回退此项（DB 无值/非法 → 默认开启）。
pub const DEFAULT_PAUSE_ON_IDLE: bool = true;

// 判定「用户离开」的空闲阈值（秒），用户可配（前端 Slider 30-300 step 30，默认 60）。
// idle > 该值且处于 Working 且 pause_on_idle 开启 → 冻结工作倒计时。
// 与前端 IDLE_PAUSE_THRESHOLD_KEY / DEFAULT / MIN / MAX 对齐；STEP 仅前端 UI 用，后端不校验。
pub const KEY_IDLE_PAUSE_THRESHOLD: &str = "idle_pause_threshold";
pub const DEFAULT_IDLE_PAUSE_THRESHOLD: f64 = 60.0;
// 合法范围（闭区间，秒）。read_idle_pause_threshold 会 clamp，防 DB 被外部写入异常值。改范围同步前端。
pub const MIN_IDLE_PAUSE_THRESHOLD: f64 = 30.0;
pub const MAX_IDLE_PAUSE_THRESHOLD: f64 = 300.0;

pub const KEY_BREAK_SKIP_MAX: &str = "break_skip_max";
// 休息跳过防误触门槛（连点 N 次才真正跳过）。与前端 DEFAULT_BREAK_SKIP_MAX 对齐。
// read_break_skip_max 会 clamp 到 [MIN, MAX]，防止 DB 被外部写入异常值。
pub const DEFAULT_BREAK_SKIP_MAX: u32 = 1;
// 跳过次数的合法范围（闭区间）。前后端均引用，改范围时同步 src/shared/appConfig.ts。
pub const MIN_BREAK_SKIP_MAX: u32 = 1;
pub const MAX_BREAK_SKIP_MAX: u32 = 3;

pub const KEY_REMINDERS: &str = "reminders";

// quiet_hours：JSON 数组 [{start: "HH:mm", end: "HH:mm"}]，schema 见 shared/types.rs
// QuietHourPeriod（derive Type，随 bindings 导出）；支持跨午夜（start > end）。
pub const KEY_QUIET_HOURS: &str = "quiet_hours";
// 默认休息时段。DB 无值 / 解析失败时回退此项，避免「前端显示 2 项但后端不暂停」的兜底不一致。
// 运行时读取（read_quiet_hours）仍走 JSON 字符串解析；前端常量导出见 DEFAULT_QUIET_HOURS。
pub const DEFAULT_QUIET_HOURS_JSON: &str =
    "[{\"start\":\"12:00\",\"end\":\"14:00\"},{\"start\":\"18:00\",\"end\":\"18:30\"}]";
// 结构化默认值：仅作 constant 导出源（read_quiet_hours 运行时用 _JSON 兜底）。
// 与 DEFAULT_QUIET_HOURS_JSON 的一致性由 lib.rs 导出前的 debug_assert 守卫。
pub const DEFAULT_QUIET_HOURS: &[QuietHourPeriod] = &[
    QuietHourPeriod {
        start: "12:00",
        end: "14:00",
    },
    QuietHourPeriod {
        start: "18:00",
        end: "18:30",
    },
];

// 休息时段总开关（YesNo）：N 时 quiet_hours 列表不再参与暂停判定（列表配置保留）。
// 默认 Y——存量用户升级后行为逐字节不变（此前无开关、恒检测）。
pub const KEY_QUIET_HOURS_ENABLED: &str = "quiet_hours_enabled";
pub const DEFAULT_QUIET_HOURS_ENABLED: bool = true;

// 免打扰时段：与 quiet_hours 同 schema（QuietHourPeriod 数组、支持跨午夜），但命中为
// 非打断式暂停——不弹窗、已有窗口收起、点托盘可查看（PauseSource::Dnd）。
pub const KEY_DND_HOURS: &str = "dnd_hours";
// 默认无免打扰时段：空数组即「未配置」，不回退任何非空默认。
pub const DEFAULT_DND_HOURS_JSON: &str = "[]";
// 结构化默认值：仅作 constant 导出源（read_dnd_hours 运行时用 _JSON 兜底）。
pub const DEFAULT_DND_HOURS: &[QuietHourPeriod] = &[];

// 免打扰时段总开关（YesNo）：默认 N，新功能静默待启。
pub const KEY_DND_HOURS_ENABLED: &str = "dnd_hours_enabled";
pub const DEFAULT_DND_HOURS_ENABLED: bool = false;

// 拿 TimerInner 锁，poisoned 时自动恢复（避免线程 panic 级联导致整个状态机死锁）。
// 场景：所有需要读写 TimerInner 的同步代码块入口。
fn lock_inner(inner: &Mutex<TimerInner>) -> MutexGuard<'_, TimerInner> {
    inner.lock().unwrap_or_else(|e| e.into_inner())
}

// ============================================================
// 配置读取
// ============================================================

// 从 DB 读 work_duration 配置（单位：分钟）。
// 容错：DB 无值 / 非数字 / 解析失败 → 回退默认 30 分钟（与 src/shared/appConfig.ts 对齐）。
// 场景：fresh_working 初始化工作时长 + on_app_config_changed 响应配置变化时重新读取。
fn read_work_duration_minutes(conn: &Connection) -> u32 {
    read_app_config_conn(conn, KEY_WORK_DURATION)
        .ok()
        .flatten()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_WORK_DURATION_MIN)
}

// 读 break_duration（分钟），同样容错回退。
fn read_break_duration_minutes(conn: &Connection) -> u32 {
    read_app_config_conn(conn, KEY_BREAK_DURATION)
        .ok()
        .flatten()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_BREAK_DURATION_MIN)
}

// 读 long_break_enabled（YesNo），容错回退默认。
fn read_long_break_enabled(conn: &Connection) -> bool {
    read_app_config_conn(conn, KEY_LONG_BREAK_ENABLED)
        .ok()
        .flatten()
        .map(|s| {
            s.parse::<YesNo>()
                .map(|y| y.is_yes())
                .unwrap_or(false)
        })
        .unwrap_or(DEFAULT_LONG_BREAK_ENABLED)
}

// 读 long_break_interval（轮），容错回退 2。
fn read_long_break_interval(conn: &Connection) -> u32 {
    read_app_config_conn(conn, KEY_LONG_BREAK_INTERVAL)
        .ok()
        .flatten()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_LONG_BREAK_INTERVAL)
}

// 读 long_break_duration（分钟），容错回退 5。
fn read_long_break_duration_minutes(conn: &Connection) -> u32 {
    read_app_config_conn(conn, KEY_LONG_BREAK_DURATION)
        .ok()
        .flatten()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_LONG_BREAK_DURATION_MIN)
}

// 按休息类型读对应时长并转秒（长休息 → long_break_duration，正常 → break_duration）。
// confirm_break（is_long 已由 on_work_done 提前写入，只补时长）等只需时长的调用点使用。
fn read_break_total_secs(conn: &Connection, is_long: bool) -> i64 {
    let minutes = if is_long {
        read_long_break_duration_minutes(conn)
    } else {
        read_break_duration_minutes(conn)
    };
    (minutes as i64) * 60
}

// 长休息判定 + 对应时长总秒数，一次性完成（run_timer_loop 归零分支与 manual_break 共用）。
// completed_cycles 传递增前的值（与 check_is_long_break 约定一致）。
fn read_break_plan(conn: &Connection, completed_cycles: u32) -> (bool, i64) {
    let is_long = check_is_long_break(
        read_long_break_enabled(conn),
        read_long_break_interval(conn),
        completed_cycles,
    );
    (is_long, read_break_total_secs(conn, is_long))
}

// 读 break_skip_max（休息跳过防误触门槛）。容错：非数字/超范围 → clamp 到 [MIN,MAX]；无值 → 回退默认。
// 场景：skip_break 判定门槛时实时读取（不缓存，保证用户改配置后立即生效）。
fn read_break_skip_max(conn: &Connection) -> u32 {
    read_app_config_conn(conn, KEY_BREAK_SKIP_MAX)
        .ok()
        .flatten()
        .and_then(|s| s.parse::<u32>().ok())
        .map(|n| n.clamp(MIN_BREAK_SKIP_MAX, MAX_BREAK_SKIP_MAX))
        .unwrap_or(DEFAULT_BREAK_SKIP_MAX)
}

// 今日累计跳过次数（内部统计，非用户配置）：持久化到 app_config 表 key today_skip_count，
// 值为 JSON {"date":"yyyy-MM-dd","count":N}。date != today 或缺失/非法 → 视为 0（跨天自然归零）。
// 与 KEY_BREAK_SKIP_MAX（用户可配的防误触门槛）完全独立——后者控制「点几次才真跳过」，
// 本项统计「今日真正跳过了几次」。前端 skip_count_reminder 阈值是纯 UI 配置，后端不读。
// 内部统计 key（非用户配置、无前端镜像），不参与 constant 导出。
const KEY_TODAY_SKIP_COUNT: &str = "today_skip_count";

// today_skip_count 的持久化结构。与前端无契约（前端经 TimerStatePayload.todaySkipCount 读缓存值，
// 不直接读此 key），故 schema 仅后端内部约定。
#[derive(serde::Serialize, serde::Deserialize)]
struct TodaySkipRecord {
    date: String,
    count: u32,
}

// 读今日累计跳过次数。fresh_working（init + 跨天）调用，date 校验自带跨天归零：
// DB 中记录的 date != today（昨日及更早）→ 返回 0。
fn read_today_skip_count(conn: &Connection) -> u32 {
    let today = today_string();
    read_app_config_conn(conn, KEY_TODAY_SKIP_COUNT)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<TodaySkipRecord>(&s).ok())
        .filter(|r| r.date == today)
        .map(|r| r.count)
        .unwrap_or(0)
}

// 写今日累计跳过次数（连同一日日期）。skip_break 真正跳过分支调用。
// 用 write_app_config_conn（conn 级，不 emit app-config-changed）——内部统计非用户配置变更，
// 不应触发 on_app_config_changed handler 或广播给前端窗口（前端经 payload 获取最新值）。
fn write_today_skip_count(conn: &Connection, count: u32) {
    let record = TodaySkipRecord {
        date: today_string(),
        count,
    };
    match serde_json::to_string(&record) {
        Ok(value) => {
            if let Err(e) = write_app_config_conn(conn, KEY_TODAY_SKIP_COUNT, &value) {
                log::warn!("write today_skip_count failed: {e}");
            }
        }
        Err(e) => log::warn!("serialize today_skip_count failed: {e}"),
    }
}

// 读 rest_confirm（YesNo），容错回退默认。
// Yes：工作结束时先进 Alerting 等用户确认；No：直接进 Breaking。
fn read_rest_confirm(conn: &Connection) -> bool {
    read_app_config_conn(conn, KEY_REST_CONFIRM)
        .ok()
        .flatten()
        .map(|s| {
            s.parse::<YesNo>()
                .map(|y| y.is_yes())
                .unwrap_or(false)
        })
        .unwrap_or(DEFAULT_REST_CONFIRM)
}

// 读 rest_end_confirm（YesNo），容错回退默认。
// Yes：休息结束进 Waiting 等用户确认；No：休息结束自动进入 Working。
fn read_rest_end_confirm(conn: &Connection) -> bool {
    read_app_config_conn(conn, KEY_REST_END_CONFIRM)
        .ok()
        .flatten()
        .map(|s| {
            s.parse::<YesNo>()
                .map(|y| y.is_yes())
                .unwrap_or(false)
        })
        .unwrap_or(DEFAULT_REST_END_CONFIRM)
}

// 读 pause_on_idle（YesNo），容错回退默认（DB 无值/非法 → 默认开启）。
// 场景：run_timer_loop 每秒现读，Working 阶段离开冻结判定；与前端 PAUSE_ON_IDLE_KEY 对齐。
fn read_pause_on_idle(conn: &Connection) -> bool {
    read_app_config_conn(conn, KEY_PAUSE_ON_IDLE)
        .ok()
        .flatten()
        .map(|s| {
            s.parse::<YesNo>()
                .map(|y| y.is_yes())
                .unwrap_or(DEFAULT_PAUSE_ON_IDLE)
        })
        .unwrap_or(DEFAULT_PAUSE_ON_IDLE)
}

// 读 idle_pause_threshold（f64 秒），容错回退默认。clamp 到 [MIN,MAX]，防止 DB 被外部写入异常值。
// 场景：run_timer_loop 每秒现读，Working 阶段离开冻结判定（idle > 该值且开关开启 → 冻结）。
fn read_idle_pause_threshold(conn: &Connection) -> f64 {
    read_app_config_conn(conn, KEY_IDLE_PAUSE_THRESHOLD)
        .ok()
        .flatten()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|n| n.clamp(MIN_IDLE_PAUSE_THRESHOLD, MAX_IDLE_PAUSE_THRESHOLD))
        .unwrap_or(DEFAULT_IDLE_PAUSE_THRESHOLD)
}

// 读 quiet_hours：JSON 数组 [{start: "HH:mm", end: "HH:mm"}]，schema 与前端一致。
// 容错策略（与前端 decodeQuietHours 对齐，避免兜底不一致）：
//   - DB 无值 / 锁失败 → 回退 DEFAULT_QUIET_HOURS（2 项默认时段）
//   - JSON 解析失败 → 同样回退默认
//   - 用户主动保存 "[]"（清空所有时段）→ 尊重用户选择，返回空 vec
// 反序列化用局部 String 字段 struct：共享类型 QuietHourPeriod 的 &'static str 字段
// 无法反序列化短生命周期 JSON（借用检查），schema 一致由 JSON 形态保证。
// 每秒被 run_timer_loop 调用一次，开销可忽略（quiet_hours 通常 < 5 项）。
fn read_quiet_hours(conn: &Connection) -> Vec<(String, String)> {
    read_period_list(conn, KEY_QUIET_HOURS, DEFAULT_QUIET_HOURS_JSON)
}

// 读 dnd_hours：与 read_quiet_hours 同 schema / 同容错分支，唯默认值为 "[]"——
// 免打扰时段无内置默认，未配置 / 损坏均解析为空（空 = 无免打扰）。
fn read_dnd_hours(conn: &Connection) -> Vec<(String, String)> {
    read_period_list(conn, KEY_DND_HOURS, DEFAULT_DND_HOURS_JSON)
}

// quiet_hours / dnd_hours 共用的时段列表解析核心（fallback_json 为无值 / 解析失败兜底）。
// 容错与前端 parsePeriods 对齐（避免「前端显示 X、后端按 Y 暂停」的兜底脱节）：
//   - DB 无值 / 非 JSON / 非数组 → 回退 fallback_json 解析结果
//   - 数组（含空数组）→ 逐条容错过滤：仅保留 start/end 均为 string 的条目，
//     混合合法/非法时取合法子集（整体不作废）；空数组 = 用户显式清空，原样返回
fn read_period_list(conn: &Connection, key: &str, fallback_json: &str) -> Vec<(String, String)> {
    let raw = read_app_config_conn(conn, key).ok().flatten();
    let parsed: Option<Vec<(String, String)>> = raw.as_deref().and_then(|json| {
        serde_json::from_str::<serde_json::Value>(json)
            .ok()
            .and_then(|v| {
                v.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            let start = item.get("start")?.as_str()?.to_string();
                            let end = item.get("end")?.as_str()?.to_string();
                            Some((start, end))
                        })
                        .collect()
                })
            })
    });
    match parsed {
        Some(periods) => periods,
        None => {
            #[derive(serde::Deserialize)]
            struct Period {
                start: String,
                end: String,
            }
            serde_json::from_str::<Vec<Period>>(fallback_json)
                .unwrap_or_default()
                .into_iter()
                .map(|p| (p.start, p.end))
                .collect()
        }
    }
}

// 读 quiet_hours_enabled（YesNo），容错回退默认（DB 无值 / 非法 → 默认开启）。
// 仿 read_pause_on_idle 写法（parse 失败也回退默认），而非 read_long_break_enabled 的
// unwrap_or(false)——后者会把「DB 脏值」错判成关闭，与前端 parseYesNo 语义不一致。
// 场景：run_timer_loop 每秒现读，休息时段判定总开关。
fn read_quiet_hours_enabled(conn: &Connection) -> bool {
    read_app_config_conn(conn, KEY_QUIET_HOURS_ENABLED)
        .ok()
        .flatten()
        .map(|s| {
            s.parse::<YesNo>()
                .map(|y| y.is_yes())
                .unwrap_or(DEFAULT_QUIET_HOURS_ENABLED)
        })
        .unwrap_or(DEFAULT_QUIET_HOURS_ENABLED)
}

// 读 dnd_hours_enabled（YesNo），容错回退默认（DB 无值 / 非法 → 默认关闭）。
fn read_dnd_hours_enabled(conn: &Connection) -> bool {
    read_app_config_conn(conn, KEY_DND_HOURS_ENABLED)
        .ok()
        .flatten()
        .map(|s| {
            s.parse::<YesNo>()
                .map(|y| y.is_yes())
                .unwrap_or(DEFAULT_DND_HOURS_ENABLED)
        })
        .unwrap_or(DEFAULT_DND_HOURS_ENABLED)
}

// 判断某 HH:mm 时刻是否落在任一时段列表内（quiet_hours / dnd_hours 共用的纯区间判定）。
// 单时段区间语义见 is_in_period；find_active_period 也复用该判定，保证边界语义单点维护。
// 注：用字符串比较合法，因为 "HH:mm" 定长格式字典序等价于时间序（"09:00" < "12:00" < "22:00"）。
fn is_in_periods(periods: &[(String, String)], hhmm: &str) -> bool {
    periods
        .iter()
        .any(|(start, end)| is_in_period(start, end, hhmm))
}

// 单时段区间判定（is_in_periods 的内核）：
// - 同日时段（start ≤ end，如 12:00-14:00）：start <= hhmm && hhmm < end
// - 跨午夜时段（start > end，如 22:00-06:00）：hhmm >= start || hhmm < end
fn is_in_period(start: &str, end: &str, hhmm: &str) -> bool {
    if start <= end {
        // 同日时段：[start, end)
        hhmm >= start && hhmm < end
    } else {
        // 跨午夜时段：[start, 24:00) ∪ [00:00, end)
        hhmm >= start || hhmm < end
    }
}

// 统一时段命中判定：当前时刻命中的暂停来源；两类时段均不命中（或开关关闭）返回 None。
// 免打扰优先：同时命中时返回 Dnd（不弹窗，更强的不打扰意图，如培训/网课场景）。
// 判定顺序即优先级，tick 循环每秒调用（开销可忽略）。
fn active_pause_source(
    quiet_enabled: bool,
    quiet_periods: &[(String, String)],
    dnd_enabled: bool,
    dnd_periods: &[(String, String)],
    hhmm: &str,
) -> Option<PauseSource> {
    if dnd_enabled && is_in_periods(dnd_periods, hhmm) {
        return Some(PauseSource::Dnd);
    }
    if quiet_enabled && is_in_periods(quiet_periods, hhmm) {
        return Some(PauseSource::Quiet);
    }
    None
}

// 找出当前时刻命中的第一个时段（返回 (start, end) 的 owned 拷贝）。
// 场景：skip_quiet_reminder 记录被跳过的具体时段（quiet_hours / dnd_hours 共用判定，当前仅 quiet 用）。
fn find_active_period(periods: &[(String, String)], hhmm: &str) -> Option<(String, String)> {
    periods
        .iter()
        .find(|(start, end)| is_in_period(start, end, hhmm))
        .cloned()
}

// 从 reminders 配置随机抽取两条：返回 (whisper, health)。
// reminders 的 JSON 形态为对象 { health: string[], whisper: string[] }：
//   - whisper 池 = 随笔心语（文学摘抄），写入 current_whisper_reminder，展示交互不变；
//   - health 池 = 健康提醒（走动/喝水等），写入 current_health_reminder，BreakingView 绿色横幅展示。
// 空池兜底（按池独立）：用户配置某池为空（DB 无值 / JSON 解析失败 / 池被清空）时，
// 该池改从 shared/reminder_texts.rs 的默认常量池抽取，与前端设置页展示逻辑一致——
// 避免「未在设置页保存过就弹窗无文案」。
// 随机源：rand crate 的 ThreadRng（CSPRNG 种子 + 高质量 PRNG）。
// 调用点：on_work_done（工作结束自动）与 manual_break（手动休息）。
fn pick_random_reminders(conn: &Connection) -> (String, String) {
    let raw = read_app_config_conn(conn, KEY_REMINDERS)
        .ok()
        .flatten()
        .unwrap_or_default();
    #[derive(serde::Deserialize)]
    struct AppRemindersConfig {
        #[serde(default)]
        health: Vec<String>,
        #[serde(default)]
        whisper: Vec<String>,
    }
    let cfg: AppRemindersConfig = serde_json::from_str(&raw).unwrap_or(AppRemindersConfig {
        health: Vec::new(),
        whisper: Vec::new(),
    });
    let pick = |user_list: &[String], default_list: &[&str]| -> String {
        // 用户池空 → 默认池兜底（常量池非空，随机范围安全）。
        if user_list.is_empty() {
            let idx = rand::rng().random_range(0..default_list.len());
            return default_list[idx].to_string();
        }
        let idx = rand::rng().random_range(0..user_list.len());
        user_list[idx].clone()
    };
    (
        pick(&cfg.whisper, DEFAULT_WHISPER_TEXTS),
        pick(&cfg.health, DEFAULT_HEALTH_TEXTS),
    )
}

// ============================================================
// 状态构造与序列化
// ============================================================

// 构造一个全新的 Working 阶段状态：按当前 work_duration 设 target_epoch = now + duration。
// 场景：应用启动初始化（init）+ 运行时跨天重置（run_timer_loop 检测到 save_date 变化）。
//      其他 phase（Alerting/Breaking/...）的构造逻辑在 M2（start_break 等）实现。
fn fresh_working(conn: &Connection) -> TimerInner {
    let minutes = read_work_duration_minutes(conn);
    let total_seconds = (minutes as i64) * 60;
    let target_epoch = now_epoch() + total_seconds;
    TimerInner {
        phase: Phase::Working,
        target_epoch,
        remaining_seconds: total_seconds,
        total_seconds,
        paused_phase: None,
        paused_source: PauseSource::Manual,
        completed_cycles: 0,
        break_skip_count: 0,
        today_skip_count: read_today_skip_count(conn),
        current_whisper_reminder: String::new(),
        current_health_reminder: String::new(),
        is_long_break: false,
        save_date: today_string(),
        break_paused: false,
        break_start_epoch: None,
        last_skip_epoch: None,
        quiet_skip_period: None,
    }
}

// ============================================================
// 内部状态转移纯函数（操作 &mut TimerInner，不读 DB、不 emit）
// ============================================================
//
// 这些函数由 command（持有 State）与定时器循环（持有 Arc<Mutex>）共享调用，
// 确保状态修改规则集中、可测试。调用方负责：
//   1. 持有 TimerInner 锁
//   2. 临时锁 AppConfigState 读 DB（按 inner → app_config 顺序避免死锁）
//   3. 锁外 emit phase-changed / timer-tick

// 长休息判定（读取递增前的 completed_cycles）：
//   `enabled && interval > 0 && completed_cycles > 0 && completed_cycles % interval == 0`
// 关键：completed_cycles 在 on_break_done 中后置递增，第一次休息时 = 0，永不长休息。
fn check_is_long_break(enabled: bool, interval: u32, completed_cycles: u32) -> bool {
    enabled && interval > 0 && completed_cycles > 0 && completed_cycles % interval == 0
}

// 进入 Working 阶段：重设 target_epoch = now + work_total_secs + 清休息相关字段。
// 场景：start_work command / confirm_return / reset / 跳过休息。
fn apply_start_work(inner: &mut TimerInner, work_total_secs: i64, now: i64) {
    inner.phase = Phase::Working;
    inner.target_epoch = now + work_total_secs;
    inner.remaining_seconds = work_total_secs;
    inner.total_seconds = work_total_secs;
    inner.paused_phase = None;
    inner.paused_source = PauseSource::Manual;
    inner.break_skip_count = 0;
    inner.current_whisper_reminder = String::new();
    inner.current_health_reminder = String::new();
    inner.is_long_break = false;
    inner.quiet_skip_period = None;
}

// 工作倒计时归零：抽取 reminder + 按 rest_confirm 分流。
// rest_confirm=true → 进 Alerting（等用户确认）；=false → 调用方继续 apply_start_break。
// is_long / break_total_secs 由调用方按 check_is_long_break 判定后传入（用递增前的 completed_cycles）；
// 此处提前写入 is_long_break，使 Alerting（休息前提醒）阶段的 payload 即携带本轮休息类型——
// panel.rs 据此分派提醒窗口形态，前端 AlertingView 据此显示「该长休息啦」。
// 返回进入的下一个 phase，调用方据此决定是否继续 breaking 流程。
fn apply_on_work_done(
    inner: &mut TimerInner,
    whisper_reminder: String,
    health_reminder: String,
    rest_confirm: bool,
    is_long: bool,
) -> Phase {
    inner.current_whisper_reminder = whisper_reminder;
    inner.current_health_reminder = health_reminder;
    inner.is_long_break = is_long;
    if rest_confirm {
        inner.phase = Phase::Alerting;
        inner.remaining_seconds = 0;
        inner.target_epoch = 0;
        Phase::Alerting
    } else {
        // 调用方继续：调 apply_start_break
        Phase::Breaking
    }
}

// 进入 Breaking 阶段：清 break_skip_count + 设剩余/总秒数。
// is_long 由调用方判定/传递（on_work_done / manual_break 经 read_break_plan 判定；
// confirm_break 读 on_work_done 提前写入的 inner.is_long_break）。on_work_done 已提前写入同值，
// 此处再赋一次保证 manual_break（不经 on_work_done）路径也正确。
fn apply_start_break(inner: &mut TimerInner, break_total_secs: i64, is_long: bool, now: i64) {
    inner.phase = Phase::Breaking;
    inner.target_epoch = 0; // breaking 不用 target_epoch（递减模式）
    inner.remaining_seconds = break_total_secs;
    inner.total_seconds = break_total_secs;
    inner.is_long_break = is_long;
    inner.break_skip_count = 0;
    inner.paused_phase = None;
    inner.paused_source = PauseSource::Manual;
    inner.break_paused = false;
    inner.break_start_epoch = Some(now);
    inner.last_skip_epoch = None;
}

// 休息正常结束（on_break_done）：后置递增 completed_cycles + 进 Waiting。
// 关键：completedCycles 后置递增 → 下次 on_work_done 长休息判定时读到递增后的值。
fn apply_on_break_done(inner: &mut TimerInner) {
    inner.completed_cycles += 1;
    inner.phase = Phase::Waiting;
    inner.remaining_seconds = 0;
    inner.target_epoch = 0;
}

// 进入 Paused（手动）：仅 Working / Breaking 可暂停；保存原 phase 到 paused_phase。
// 手动暂停（Manual）：不自动恢复，等用户点继续。
fn apply_enter_paused(inner: &mut TimerInner) -> bool {
    if inner.phase != Phase::Working && inner.phase != Phase::Breaking {
        return false;
    }
    inner.paused_phase = Some(inner.phase);
    inner.paused_source = PauseSource::Manual;
    inner.phase = Phase::Paused;
    true
}

// 进入 Paused（时段触发，source 由调用方经 active_pause_source 判定后显式传入）：
// 任意非 Paused 阶段都强制进 Paused。Quiet（休息时段，打断式）与 Dnd（免打扰，非打断式）
// 共用本函数，行为差异（弹窗 vs 收起）由 panel.rs 按 payload.pause_source 分派。
// 与 apply_enter_paused 区别：
//   - 适用阶段更广（Alerting / Waiting 也会被打断）
//   - 时段结束时由 tick 自动 start_work 新一轮（不恢复原进度，作废当前轮）
fn apply_enter_pause_window(inner: &mut TimerInner, source: PauseSource) -> bool {
    if inner.phase == Phase::Paused {
        return false;
    }
    debug_assert!(matches!(
        source,
        PauseSource::Quiet | PauseSource::Dnd
    ));
    inner.paused_phase = Some(inner.phase);
    inner.paused_source = source;
    inner.phase = Phase::Paused;
    true
}

// 从 Paused 恢复：根据 paused_phase 重设（working 重设 target_epoch；breaking 保持 remaining 递减）。
// ⚠️ 时段触发的 paused（Quiet/Dnd）不允许手动恢复：返回 false，toggle_pause 忽略；
//    等系统在 tick 中检测两类时段全部结束后自动 start_work。
fn apply_resume(inner: &mut TimerInner, now: i64) -> bool {
    if inner.phase != Phase::Paused {
        return false;
    }
    match inner.paused_source {
        // 时段暂停期间，用户手动「继续」无效
        PauseSource::Quiet | PauseSource::Dnd => return false,
        PauseSource::Manual => {}
    }
    let prev = inner.paused_phase.unwrap_or(Phase::Working);
    inner.phase = prev;
    inner.paused_phase = None;
    if prev == Phase::Working && inner.remaining_seconds > 0 {
        // 重设 target_epoch = now + remaining，继续 working 倒计时
        inner.target_epoch = now + inner.remaining_seconds;
    }
    true
}

// 把 TimerInner 转成可序列化的 TimerStatePayload（前端能直接消费的形态）。
// prev_phase 取值：
//   - None：普通 tick 快照（timer-tick 事件、get_timer_state command 返回）
//   - Some(old)：刚从 old 切换到当前 phase（phase-changed 事件，前端用于切 UI 分支）
fn build_payload(inner: &TimerInner, prev_phase: Option<Phase>) -> TimerStatePayload {
    TimerStatePayload {
        phase: inner.phase,
        prev_phase,
        remaining_seconds: inner.remaining_seconds,
        total_seconds: inner.total_seconds,
        current_whisper_reminder: inner.current_whisper_reminder.clone(),
        current_health_reminder: inner.current_health_reminder.clone(),
        is_long_break: inner.is_long_break,
        break_skip_count: inner.break_skip_count,
        today_skip_count: inner.today_skip_count,
        completed_cycles: inner.completed_cycles,
        pause_source: inner.paused_source,
        break_paused: inner.break_paused,
        auto_resumed: false,
    }
}

// ============================================================
// 事件 emit
// ============================================================

// emit "timer-tick"：每秒推送当前状态快照，前端 useTimerState（M5）订阅以更新圆环倒计时。
// emit 失败仅 log warn，不中断主循环（前端暂时少一帧不影响下秒恢复）。
fn emit_timer_tick(app: &AppHandle, payload: TimerStatePayload) {
    if let Err(e) = app.emit(crate::shared::events::EVENT_TIMER_TICK, payload) {
        log::warn!("emit timer-tick failed: {e}");
    }
}

// emit "phase-changed"：phase 切换时推送（带 prev_phase），前端 PanelApp（M4/M5）用于切换 UI 分支。
// panel.rs 同步监听此事件：进入非 Working 阶段时主动唤起 panel 窗口（常驻提醒）。
fn emit_phase_changed(app: &AppHandle, payload: TimerStatePayload) {
    if let Err(e) = app.emit(
        crate::shared::events::EVENT_PHASE_CHANGED,
        payload,
    ) {
        log::warn!("emit phase-changed failed: {e}");
    }
}

// ============================================================
// 1Hz 定时器循环
// ============================================================

// 1Hz 定时器主循环：在 init 中 spawn 一次，永久运行（与应用同生命周期）。
// 每秒执行：
//   1. 跨天判断（save_date != today → fresh_working + emit phase-changed）
//   2. 时段暂停判断（每秒检查，休息/免打扰统一判定，见下方转移表）
//   3. 按当前 phase 推进（若未被时段切到 Paused）：
//      - Working：按 target_epoch 重算 remaining；归零 → on_work_done → Alerting 或直接 Breaking
//      - Breaking：remaining -= 1；归零 → on_break_done → Waiting
//      - Alerting / Waiting / Paused：不递减、不切换
//   4. emit timer-tick；有 phase 切换则 emit phase-changed（panel.rs 据此唤起非 Working 窗口）
// 关键约束：所有锁内操作必须不跨 .await（std::sync::Mutex 的 MutexGuard 不是 Send），
//          payload 构建在锁内完成，emit 在锁外执行。
async fn run_timer_loop(app: AppHandle, inner: Arc<Mutex<TimerInner>>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));

    loop {
        interval.tick().await;

        // 同步块：所有状态修改 + payload 构建在锁内完成，不跨 await 持锁
        let (tick_payload, phase_change_payload) = {
            let mut state = lock_inner(&inner);
            let mut phase_change_payload = None;

            // 运行时跨天判断：长时间运行到次日，开始新一轮工作
            let today = today_string();
            if state.save_date != today {
                let prev = state.phase;
                let app_config_state = app.state::<AppConfigState>();
                if let Ok(conn) = app_config_state.0.lock() {
                    // fresh_working 会清空 quiet_skip_period（全新状态）；但跨午夜 quiet 时段
                    // （如 22:00-06:00）内跨天时用户仍处于同一时段——必须保留标记，否则午夜
                    // 一过已跳过的弹窗会复活。恢复后交由下方标记清理块按最新配置校验
                    // （届时若已不在任何 quiet 时段内则照常清空，语义不变）。
                    let quiet_skip = state.quiet_skip_period.take();
                    *state = fresh_working(&conn);
                    state.quiet_skip_period = quiet_skip;
                    phase_change_payload = Some(build_payload(&state, Some(prev)));
                } else {
                    log::warn!("app_config lock poisoned, skip cross-day reset");
                }
            }

            // 一次性批量读配置（时段判定 + 自动 start_work + Working 冻结共用，
            // 避免各分支里再锁 AppConfigState）。
            // 锁失败时保守回退：休息时段开关默认开 + 空列表（等价无暂停）+ 默认 work 时长。
            let (
                quiet_enabled,
                quiet_periods,
                dnd_enabled,
                dnd_periods,
                work_min,
                pause_on_idle,
                idle_pause_threshold,
            ) = {
                let app_config_state = app.state::<AppConfigState>();
                match app_config_state.0.lock() {
                    Ok(conn) => (
                        read_quiet_hours_enabled(&conn),
                        read_quiet_hours(&conn),
                        read_dnd_hours_enabled(&conn),
                        read_dnd_hours(&conn),
                        read_work_duration_minutes(&conn),
                        read_pause_on_idle(&conn),
                        read_idle_pause_threshold(&conn),
                    ),
                    Err(e) => {
                        log::warn!("app_config lock failed, skip period pause check: {e}");
                        (
                            DEFAULT_QUIET_HOURS_ENABLED,
                            Vec::new(),
                            DEFAULT_DND_HOURS_ENABLED,
                            Vec::new(),
                            DEFAULT_WORK_DURATION_MIN,
                            DEFAULT_PAUSE_ON_IDLE,
                            DEFAULT_IDLE_PAUSE_THRESHOLD,
                        )
                    }
                }
            };

            // 时段暂停判断（每秒检查，开销可忽略）：休息（quiet，打断式）/ 免打扰（dnd，非打断式）
            // 统一判定，免打扰优先。五分支显式转移表（全取值覆盖，无排除法兜底）：
            //   ① 非 Paused + 命中 → 强制进 Paused（Alerting/Breaking/Waiting 一并打断，本轮作废）
            //   ② Paused + 命中来源 != 当前来源且当前为时段来源 → 来源切换（quiet↔dnd 重叠边界，
            //      不切 phase 不闪窗，panel.rs 以 Paused→Paused 事件按新来源调整窗口形态）
            //   ③ Paused + 全部不命中 + 时段来源 → 作废本轮自动 start_work（quiet/dnd 共用退出）
            //   ④ Paused + Manual → 无操作（手动暂停不被时段接管，也不自动恢复）
            //   ⑤ 非 Paused + 不命中 → 无操作
            let now = now_epoch();
            let hit = active_pause_source(
                quiet_enabled,
                &quiet_periods,
                dnd_enabled,
                &dnd_periods,
                &now_hhmm(),
            );

            // 休息时段「跳过本次提醒」标记清理：一旦不再处于任何启用的 quiet 时段内
            // （时段结束 / quiet 开关关闭 / 配置清空），清空标记——下个时段重新恢复
            // 弹窗提醒（「不影响下次休息时段」）。标记仅在 Paused(Quiet) 可写入。
            if state.quiet_skip_period.is_some()
                && !(quiet_enabled && is_in_periods(&quiet_periods, &now_hhmm()))
            {
                state.quiet_skip_period = None;
            }
            match (state.phase, hit, state.paused_source) {
                // ① 进入：任意非 Paused 阶段强制进 Paused（来源显式传入）
                (phase, Some(src), _) if phase != Phase::Paused => {
                    let prev = state.phase;
                    apply_enter_pause_window(&mut state, src);
                    phase_change_payload = Some(build_payload(&state, Some(prev)));
                }
                // ② 来源切换：仅在时段暂停之间切换（Manual 免疫，落入 ④）
                (Phase::Paused, Some(src), PauseSource::Quiet | PauseSource::Dnd)
                    if src != state.paused_source =>
                {
                    state.paused_source = src;
                    // prev_phase = Some(Paused)：panel.rs 以「Paused→Paused」识别来源切换，
                    // 按新来源单次 sync 窗口形态（dnd 收起 / quiet 弹窗），不闪窗。
                    phase_change_payload = Some(build_payload(&state, Some(Phase::Paused)));
                }
                // ③ 退出：两类时段全部不命中才恢复（重叠时只要任一命中，hit 非 None 不触发）
                (Phase::Paused, None, PauseSource::Quiet | PauseSource::Dnd) => {
                    let prev = state.phase;
                    apply_start_work(&mut state, (work_min as i64) * 60, now);
                    state.save_date = today_string();
                    // 标记「时段结束自动恢复」：panel.rs 据此收起窗口，不显示工作状态。
                    let mut payload = build_payload(&state, Some(prev));
                    payload.auto_resumed = true;
                    phase_change_payload = Some(payload);
                }
                // ④ Manual 暂停 + 命中/不命中 / ⑤ 非 Paused 不命中 / 已同来源命中保持：无操作
                _ => {}
            }

            // phase 推进（若时段判定切到了 Paused 或 Working，下方 match 会自然落到对应分支处理新 phase）
            match state.phase {
                Phase::Working => {
                    // 离开冻结（锁屏/休眠/AFK）：pause_on_idle 开启且系统空闲超阈值时，
                    // 把 target_epoch 重新锚定为 now + remaining，使下方重算保持 remaining 不变 → 冻结工作倒计时。
                    // 用户回来（idle 回落）后停止锚定，倒计时从原进度自然恢复。阈值内短暂思考/阅读照常计时。
                    if pause_on_idle && state.remaining_seconds > 0 {
                        let idle_secs = crate::shared::idle::get_idle_seconds();
                        if idle_secs > idle_pause_threshold {
                            state.target_epoch = now + state.remaining_seconds;
                        }
                    }
                    // 绝对时间机制：按 target_epoch 重算 remaining_seconds
                    state.remaining_seconds = if state.target_epoch > now {
                        state.target_epoch - now
                    } else {
                        0
                    };

                    // 工作倒计时归零 → on_work_done
                    if state.remaining_seconds == 0 {
                        let prev = state.phase;
                        let app_config_state = app.state::<AppConfigState>();
                        match app_config_state.0.lock() {
                            Ok(conn) => {
                                let (whisper_reminder, health_reminder) =
                                    pick_random_reminders(&conn);
                                let rest_confirm = read_rest_confirm(&conn);
                                // 长休息判定统一在归零时刻完成一次（Alerting 与直接 Breaking 共用），
                                // on_work_done 提前写入 is_long_break，Alerting payload 即携带休息类型。
                                let (is_long, total_secs) =
                                    read_break_plan(&conn, state.completed_cycles);
                                let next = apply_on_work_done(
                                    &mut state,
                                    whisper_reminder,
                                    health_reminder,
                                    rest_confirm,
                                    is_long,
                                );

                                if next == Phase::Breaking {
                                    // rest_confirm=N：直接进 Breaking
                                    apply_start_break(&mut state, total_secs, is_long, now);
                                }
                                // Alerting 与 Breaking 都属于非 Working，panel.rs 监听 phase-changed 时唤起窗口
                                phase_change_payload = Some(build_payload(&state, Some(prev)));
                            }
                            Err(e) => {
                                log::warn!("app_config lock poisoned, skip on_work_done: {e}")
                            }
                        }
                    }
                }
                Phase::Breaking => {
                    // 鼠标活动检测：每秒查系统 idle 秒数，
                    // idle < 3s 视为「在用电脑」→ 软暂停（不递减 remaining，不切 phase）。
                    // 启动 4s / 跳过 3s 宽限期避免按钮点击余波误判。
                    let idle_secs = crate::shared::idle::get_idle_seconds();
                    let grace_start = state
                        .break_start_epoch
                        .map(|s| now - s < 4)
                        .unwrap_or(false);
                    let grace_skip = state
                        .last_skip_epoch
                        .map(|s| now - s < 3)
                        .unwrap_or(false);
                    let active = idle_secs < 3.0
                        && state.remaining_seconds > 0
                        && !grace_start
                        && !grace_skip;
                    state.break_paused = active;
                    if !active {
                        if state.remaining_seconds > 0 {
                            state.remaining_seconds -= 1;
                        }
                        if state.remaining_seconds == 0 {
                            let prev = state.phase;
                            // rest_end_confirm=N：跳过 Waiting，直接自动进入 Working
                            // （completed_cycles 后置递增保留轮次语义；emit prev_phase=Breaking，
                            // panel.rs 从休息侧进 Working 自动收起窗口）。
                            let rest_end_confirm = {
                                let app_config_state = app.state::<AppConfigState>();
                                app_config_state
                                    .0
                                    .lock()
                                    .map(|conn| read_rest_end_confirm(&conn))
                                    .unwrap_or(DEFAULT_REST_END_CONFIRM)
                            };
                            if rest_end_confirm {
                                apply_on_break_done(&mut state);
                            } else {
                                state.completed_cycles += 1;
                                apply_start_work(&mut state, (work_min as i64) * 60, now);
                            }
                            phase_change_payload = Some(build_payload(&state, Some(prev)));
                        }
                    }
                }
                Phase::Alerting | Phase::Waiting | Phase::Paused => {
                    // 这些阶段 tick 中不推进：Alerting 等用户确认；Waiting 等用户回来；
                    // Paused 等手动恢复（Manual）或时段全部结束自动 start_work（Quiet/Dnd，见上方转移表）。
                    // remaining 保持当前值。
                }
            }

            (build_payload(&state, None), phase_change_payload)
        };

        emit_timer_tick(&app, tick_payload);

        if let Some(payload) = phase_change_payload {
            emit_phase_changed(&app, payload);
        }
    }
}

// ============================================================
// app-config-changed handler
// ============================================================

// 订阅 "app-config-changed" 事件的 handler：用户在设置页改配置后由 set_app_config 触发。
// M1 仅响应 work_duration：若当前在 Working 阶段，重算 target_epoch + total + remaining，
//   并立即 emit timer-tick 让前端看到新剩余时间（无需等下一秒）。
// 其他 key（break_duration / long_break_* / rest_confirm / quiet_hours / reminders）由 M2/M3 接管。
fn on_app_config_changed(app: AppHandle, inner: Arc<Mutex<TimerInner>>, event: tauri::Event) {
    let key = serde_json::from_str::<serde_json::Value>(event.payload())
        .ok()
        .and_then(|v| {
            v.get("key")
                .and_then(|k| k.as_str())
                .map(|s| s.to_string())
        });

    let Some(key) = key else {
        return;
    };

    match key.as_str() {
        KEY_WORK_DURATION => {
            // 仅 working 阶段响应（重算 target_epoch / total / remaining）
            let tick_payload = {
                let mut state = lock_inner(&inner);
                if state.phase != Phase::Working {
                    return;
                }
                let minutes = {
                    let app_config_state = app.state::<AppConfigState>();
                    match app_config_state.0.lock() {
                        Ok(conn) => read_work_duration_minutes(&conn),
                        Err(e) => {
                            log::warn!("app_config lock failed: {e}");
                            return;
                        }
                    }
                };

                let total_seconds = (minutes as i64) * 60;
                state.total_seconds = total_seconds;
                state.target_epoch = now_epoch() + total_seconds;
                state.remaining_seconds = total_seconds;
                state.save_date = today_string();

                build_payload(&state, None)
            };

            // 立即推送 tick，前端立刻看到新剩余时间
            emit_timer_tick(&app, tick_payload);
        }
        _ => {
            // M2/M3 接管其他 key（break_duration / long_break_* / rest_confirm / quiet_hours / reminders）
        }
    }
}

// ============================================================
// 公开 API：init + get_timer_state
// ============================================================

// 模块初始化入口，在 lib.rs setup 中调用（必须在 shared::app_config::init 之后，因为依赖 AppConfigState）。
// 执行步骤：
//   1. fresh_working 创建初始 Working 状态（重启即重新计时，不读历史）
//   2. app.manage(TimerState) 注册全局状态供 command 访问
//   3. tauri::async_runtime::spawn 启动 1Hz 定时器循环
//   4. app.listen("app-config-changed", ...) 订阅配置变更事件
pub fn init(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    // 重启即重新计时：直接 fresh_working，不读历史状态
    let inner_state = {
        let app_config_state = app.state::<AppConfigState>();
        let conn = app_config_state
            .0
            .lock()
            .map_err(|e| e.to_string())?;
        fresh_working(&conn)
    };

    let inner = Arc::new(Mutex::new(inner_state));
    app.manage(TimerState {
        inner: inner.clone(),
    });

    let app_handle_loop = app.handle().clone();
    let inner_loop = inner.clone();
    tauri::async_runtime::spawn(async move {
        run_timer_loop(app_handle_loop, inner_loop).await;
    });

    let app_handle_listen = app.handle().clone();
    let inner_listen = inner.clone();
    app.handle().listen(
        crate::shared::events::EVENT_APP_CONFIG_CHANGED,
        move |event| {
            on_app_config_changed(
                app_handle_listen.clone(),
                inner_listen.clone(),
                event,
            );
        },
    );

    Ok(())
}

// Tauri command：返回当前完整状态快照。
// 场景：前端 panel/settings 初始化时 invoke("get_timer_state") 主动拉取一次，
//      避免 mount 后到下一秒 timer-tick 之间的 UI 空白；之后由 timer-tick 自动同步。
#[tauri::command]
#[specta::specta]
pub fn get_timer_state(state: State<'_, TimerState>) -> Result<TimerStatePayload, String> {
    let inner = lock_inner(&state.inner);
    Ok(build_payload(&inner, None))
}

// 读取当前 phase：供其他模块判断状态机阶段
// （如 settings 关闭时决定是否恢复 panel 常驻）。锁 poisoned 时回退 Working（保守不唤起）。
pub fn current_phase(app: &AppHandle) -> Phase {
    let state = app.state::<TimerState>();
    match state.inner.lock() {
        Ok(inner) => inner.phase,
        Err(_) => Phase::Working,
    }
}

// 读取当前暂停来源：供 panel.rs 托盘点击判断「免打扰暂停面板可收起」。
// 非 Paused 阶段返回 Manual 惯例值；锁 poisoned 时同样回退 Manual
// （保守：托盘走不可收起分支，改唤起而非误收起）。
pub fn current_pause_source(app: &AppHandle) -> PauseSource {
    let state = app.state::<TimerState>();
    match state.inner.lock() {
        Ok(inner) => inner.paused_source,
        Err(_) => PauseSource::Manual,
    }
}

// 查询「本次休息时段提醒是否已被用户跳过」：panel.rs 的 Quiet 弹窗分支据此抑制自动唤起。
// 标记由 run_timer_loop 每秒清理（离开 quiet 时段即清空），此处不做时段复验；
// timer 未初始化 / 锁 poisoned 保守返回 false（宁可多弹一次，不静默吞提醒）。
pub fn quiet_reminder_skipped(app: &AppHandle) -> bool {
    let Some(state) = app.try_state::<TimerState>() else {
        return false;
    };
    lock_inner(&state.inner)
        .quiet_skip_period
        .is_some()
}

// 托盘菜单「进入休息 / 休息中」项的状态：(menu_text key, 是否可点击)。
//   - Working：可点击，经 read_break_plan（递增前 completed_cycles）预判本次休息类型——
//     与 manual_break 实际执行的是同一函数同一输入，文案与行为编译期保证一致
//     （前端 nextBreakIsLong 同公式同输入，三方天然一致）
//   - Alerting / Breaking：休息已开始（或提醒待确认），置灰显示 休息中/长休息中
//   - Waiting / Paused：manual_break 仅 Working 有效，置灰显示 进入休息/进入长休息
// 锁顺序保持 inner → app_config（与 command 模式一致）。timer 未初始化时保守置灰。
pub fn rest_menu_state(app: &AppHandle) -> (&'static str, bool) {
    let Some(state) = app.try_state::<TimerState>() else {
        return ("restEnter", false);
    };
    let inner = lock_inner(&state.inner);
    match inner.phase {
        Phase::Working => {
            let is_long = {
                let app_config_state = app.state::<AppConfigState>();
                match app_config_state.0.lock() {
                    // read_break_plan 同时返回时长秒数，此处仅取长休息判定位（开销可忽略）。
                    Ok(conn) => read_break_plan(&conn, inner.completed_cycles).0,
                    Err(_) => false,
                }
            };
            (
                if is_long {
                    "restEnterLong"
                } else {
                    "restEnter"
                },
                true,
            )
        }
        Phase::Alerting | Phase::Breaking => (
            if inner.is_long_break {
                "restActiveLong"
            } else {
                "restActive"
            },
            false,
        ),
        Phase::Waiting | Phase::Paused => ("restEnter", false),
    }
}

// ============================================================
// 状态转移 commands（前端按钮 invoke 入口）
// ============================================================
//
// 共同模式：
//   1. 锁 inner（同步块）
//   2. 必要时锁 AppConfigState 读配置（保持 inner → app_config 顺序避免死锁）
//   3. 调 apply_* 纯函数修改状态
//   4. 锁外 emit phase-changed / timer-tick
// 错误策略：锁 poisoned → 返回 Err；读配置失败 → 回退默认值（不返回 Err）。

// 强制开始新一轮工作（任意 phase 都可调用）。
// 场景：用户在 UI 点「重置」之外主动 start，或托盘菜单 start。
#[tauri::command]
#[specta::specta]
pub fn start_work(app: AppHandle, state: State<'_, TimerState>) -> Result<(), String> {
    let payload = {
        let mut inner = lock_inner(&state.inner);
        let prev = inner.phase;
        let now = now_epoch();
        let work_min = {
            let app_config_state = app.state::<AppConfigState>();
            let conn = app_config_state
                .0
                .lock()
                .map_err(|e| e.to_string())?;
            read_work_duration_minutes(&conn)
        };
        apply_start_work(&mut inner, (work_min as i64) * 60, now);
        inner.save_date = today_string();
        build_payload(&inner, Some(prev))
    };
    emit_phase_changed(&app, payload);
    Ok(())
}

// Alerting → Breaking：用户在 alerting UI 点「开始休息」。
// is_long 不在此处重算——on_work_done 归零时刻已提前写入 inner.is_long_break（Alerting 提醒
// 展示的休息类型即实际类型，用户在提醒期间改配置也不会二者不一致），此处只按类型读对应时长。
#[tauri::command]
#[specta::specta]
pub fn confirm_break(app: AppHandle, state: State<'_, TimerState>) -> Result<(), String> {
    let phase_payload = {
        let mut inner = lock_inner(&state.inner);
        if inner.phase != Phase::Alerting {
            return Err("confirm_break only valid in Alerting phase".into());
        }
        let prev = inner.phase;
        let total_secs = {
            let app_config_state = app.state::<AppConfigState>();
            let conn = app_config_state
                .0
                .lock()
                .map_err(|e| e.to_string())?;
            read_break_total_secs(&conn, inner.is_long_break)
        };
        let is_long = inner.is_long_break;
        apply_start_break(&mut inner, total_secs, is_long, now_epoch());
        build_payload(&inner, Some(prev))
    };
    emit_phase_changed(&app, phase_payload);
    Ok(())
}

// Waiting → Working：休息结束用户点「我回来了」，开始新一轮。
#[tauri::command]
#[specta::specta]
pub fn confirm_return(app: AppHandle, state: State<'_, TimerState>) -> Result<(), String> {
    let payload = {
        let mut inner = lock_inner(&state.inner);
        if inner.phase != Phase::Waiting {
            return Err("confirm_return only valid in Waiting phase".into());
        }
        let prev = inner.phase;
        let now = now_epoch();
        let work_min = {
            let app_config_state = app.state::<AppConfigState>();
            let conn = app_config_state
                .0
                .lock()
                .map_err(|e| e.to_string())?;
            read_work_duration_minutes(&conn)
        };
        apply_start_work(&mut inner, (work_min as i64) * 60, now);
        build_payload(&inner, Some(prev))
    };
    emit_phase_changed(&app, payload);
    Ok(())
}

// Working/Breaking ↔ Paused 切换。
// - Working 或 Breaking → Paused（保存 paused_phase）
// - Paused → 原 phase（working 重设 target_epoch = now + remaining；breaking 保持递减）
// - Alerting / Waiting：不操作（这两个阶段无「暂停」语义）
#[tauri::command]
#[specta::specta]
pub fn toggle_pause(app: AppHandle, state: State<'_, TimerState>) -> Result<(), String> {
    let payload_opt = {
        let mut inner = lock_inner(&state.inner);
        let prev = inner.phase;
        let changed = match inner.phase {
            Phase::Working | Phase::Breaking => apply_enter_paused(&mut inner),
            Phase::Paused => apply_resume(&mut inner, now_epoch()),
            _ => false,
        };
        if changed {
            Some(build_payload(&inner, Some(prev)))
        } else {
            None
        }
    };
    if let Some(payload) = payload_opt {
        emit_phase_changed(&app, payload);
    }
    Ok(())
}

// 重置：清 completed_cycles + 重新开始 Working（与参考项目 reset 一致）。
#[tauri::command]
#[specta::specta]
pub fn reset(app: AppHandle, state: State<'_, TimerState>) -> Result<(), String> {
    let payload = {
        let mut inner = lock_inner(&state.inner);
        let prev = inner.phase;
        let work_min = {
            let app_config_state = app.state::<AppConfigState>();
            let conn = app_config_state
                .0
                .lock()
                .map_err(|e| e.to_string())?;
            read_work_duration_minutes(&conn)
        };
        apply_start_work(&mut inner, (work_min as i64) * 60, now_epoch());
        inner.completed_cycles = 0;
        inner.save_date = today_string();
        build_payload(&inner, Some(prev))
    };
    emit_phase_changed(&app, payload);
    Ok(())
}

// 立即休息：Working → Breaking（用户主动「休息」按钮；completed_cycles 判定输入，
// 轮次是否 +1 取决于本次休息是否正常走完 / 被跳过）。
#[tauri::command]
#[specta::specta]
pub fn manual_break(app: AppHandle, state: State<'_, TimerState>) -> Result<(), String> {
    let phase_payload = {
        let mut inner = lock_inner(&state.inner);
        if inner.phase != Phase::Working {
            return Err("manual_break only valid in Working phase".into());
        }
        let prev = inner.phase;
        let (total_secs, is_long, whisper_reminder, health_reminder) = {
            let app_config_state = app.state::<AppConfigState>();
            let conn = app_config_state
                .0
                .lock()
                .map_err(|e| e.to_string())?;
            let (is_long, total_secs) = read_break_plan(&conn, inner.completed_cycles);
            // 与 on_work_done 对齐：手动触发也抽取 reminder，BreakingView 底部显示提醒文案
            let (whisper_reminder, health_reminder) = pick_random_reminders(&conn);
            (
                total_secs,
                is_long,
                whisper_reminder,
                health_reminder,
            )
        };
        inner.current_whisper_reminder = whisper_reminder;
        inner.current_health_reminder = health_reminder;
        apply_start_break(&mut inner, total_secs, is_long, now_epoch());
        build_payload(&inner, Some(prev))
    };
    emit_phase_changed(&app, phase_payload);
    Ok(())
}

// 跳过休息（Alerting/Breaking 阶段）：连点 break_skip_max 次才跳过（防误触；由 break_skip_max 配置控制）。
// - < max：break_skip_count += 1，emit timer-tick 让 UI 显示「跳过 (n/max)」
// - >= max：跳过本次休息，completed_cycles 同步递增（跳过也计入休息轮次），直接 start_work
#[tauri::command]
#[specta::specta]
pub fn skip_break(app: AppHandle, state: State<'_, TimerState>) -> Result<(), String> {
    let (tick_payload, phase_payload) = {
        let mut inner = lock_inner(&state.inner);
        if !matches!(inner.phase, Phase::Alerting | Phase::Breaking) {
            return Err("skip_break only valid in Alerting or Breaking phase".into());
        }
        inner.break_skip_count = inner.break_skip_count.saturating_add(1);
        if inner.phase == Phase::Breaking {
            inner.last_skip_epoch = Some(now_epoch());
        }
        let break_skip_max = {
            let app_config_state = app.state::<AppConfigState>();
            let conn = app_config_state
                .0
                .lock()
                .map_err(|e| e.to_string())?;
            read_break_skip_max(&conn)
        };
        if inner.break_skip_count < break_skip_max {
            (Some(build_payload(&inner, None)), None)
        } else {
            let prev = inner.phase;
            let work_min = {
                let app_config_state = app.state::<AppConfigState>();
                let conn = app_config_state
                    .0
                    .lock()
                    .map_err(|e| e.to_string())?;
                // 真正跳过：今日累计跳过次数 +1 并持久化（与 work_min 共用同一 AppConfigState 锁，
                // 保持 inner→app_config 加锁顺序）。apply_start_work 不触碰 today_skip_count，
                // 故增量保留至 build_payload。
                inner.today_skip_count = inner.today_skip_count.saturating_add(1);
                write_today_skip_count(&conn, inner.today_skip_count);
                read_work_duration_minutes(&conn)
            };
            // 跳过也计入休息轮次（与 on_break_done 后置递增同语义）：apply_start_work 不重置
            // completed_cycles，此处递增后，下次休息判定即读到新值——interval=2 时完成 1 次
            // + 跳过 1 次，下次即长休息。
            inner.completed_cycles = inner.completed_cycles.saturating_add(1);
            apply_start_work(&mut inner, (work_min as i64) * 60, now_epoch());
            (None, Some(build_payload(&inner, Some(prev))))
        }
    };
    if let Some(p) = tick_payload {
        emit_timer_tick(&app, p);
    }
    if let Some(p) = phase_payload {
        emit_phase_changed(&app, p);
    }
    Ok(())
}

// 跳过本次休息时段提醒（Paused + Quiet 弹窗上的「跳过本次提醒」按钮）：
// 记录当前命中的 quiet 时段 (start, end) 到 quiet_skip_period，panel.rs 的 Quiet 弹窗分支
// 据此抑制自动唤起；计时暂停与时段结束自动恢复行为完全不变（只抑制提醒，不跳过暂停）。
// 标记由 tick 在离开 quiet 时段时清空——下个时段照常提醒。与 skip_break（工作休息跳过，
// 会推进轮次）语义完全独立，不共用任何字段。
#[tauri::command]
#[specta::specta]
pub fn skip_quiet_reminder(app: AppHandle, state: State<'_, TimerState>) -> Result<(), String> {
    let mut inner = lock_inner(&state.inner);
    if inner.phase != Phase::Paused || inner.paused_source != PauseSource::Quiet {
        return Err("skip_quiet_reminder only valid in Paused(Quiet) phase".into());
    }
    let period = {
        let app_config_state = app.state::<AppConfigState>();
        let conn = app_config_state
            .0
            .lock()
            .map_err(|e| e.to_string())?;
        find_active_period(&read_quiet_hours(&conn), &now_hhmm())
    };
    match period {
        Some(period) => inner.quiet_skip_period = Some(period),
        None => return Err("no active quiet period at skip time".into()),
    }
    Ok(())
}

// ============================================================
// 单元测试：时段判定与暂停来源转移（纯函数，不依赖 Tauri 运行时）
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn periods(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(s, e)| (s.to_string(), e.to_string()))
            .collect()
    }

    fn working_inner() -> TimerInner {
        TimerInner {
            phase: Phase::Working,
            target_epoch: 1000,
            remaining_seconds: 600,
            total_seconds: 600,
            paused_phase: None,
            paused_source: PauseSource::Manual,
            completed_cycles: 0,
            break_skip_count: 0,
            today_skip_count: 0,
            current_whisper_reminder: String::new(),
            current_health_reminder: String::new(),
            is_long_break: false,
            save_date: today_string(),
            break_paused: false,
            break_start_epoch: None,
            last_skip_epoch: None,
            quiet_skip_period: None,
        }
    }

    // —— is_in_periods：区间判定（同日 / 跨午夜 / 边界含排）——

    #[test]
    fn is_in_periods_same_day_window() {
        let p = periods(&[("12:00", "14:00")]);
        assert!(!is_in_periods(&p, "11:59"));
        assert!(is_in_periods(&p, "12:00")); // start 含入
        assert!(is_in_periods(&p, "13:30"));
        assert!(!is_in_periods(&p, "14:00")); // end 排他
    }

    #[test]
    fn is_in_periods_overnight_window() {
        let p = periods(&[("22:00", "06:00")]);
        assert!(is_in_periods(&p, "23:00"));
        assert!(is_in_periods(&p, "03:00"));
        assert!(!is_in_periods(&p, "06:00"));
        assert!(!is_in_periods(&p, "15:00"));
    }

    #[test]
    fn is_in_periods_empty_list_never_hits() {
        assert!(!is_in_periods(&[], "12:00"));
    }

    // —— active_pause_source：统一判定 + 免打扰优先 + 开关门控 ——

    #[test]
    fn active_pause_source_none_when_both_disabled() {
        let quiet = periods(&[("12:00", "14:00")]);
        let dnd = periods(&[("12:00", "14:00")]);
        assert_eq!(
            active_pause_source(false, &quiet, false, &dnd, "13:00"),
            None
        );
    }

    #[test]
    fn active_pause_source_quiet_when_enabled_and_hit() {
        let quiet = periods(&[("12:00", "14:00")]);
        assert_eq!(
            active_pause_source(true, &quiet, false, &[], "13:00"),
            Some(PauseSource::Quiet)
        );
    }

    #[test]
    fn active_pause_source_dnd_wins_on_overlap() {
        // 重叠时刻：dnd 判定在前 → Some(Dnd)，不弹窗
        let quiet = periods(&[("12:00", "14:00")]);
        let dnd = periods(&[("13:00", "16:00")]);
        assert_eq!(
            active_pause_source(true, &quiet, true, &dnd, "13:30"),
            Some(PauseSource::Dnd)
        );
        // dnd 开关关闭 → 回落 quiet
        assert_eq!(
            active_pause_source(true, &quiet, false, &dnd, "13:30"),
            Some(PauseSource::Quiet)
        );
    }

    // —— apply_enter_pause_window / apply_resume：进入与手动恢复免疫 ——

    #[test]
    fn enter_pause_window_from_any_non_paused_phase() {
        for phase in [
            Phase::Working,
            Phase::Alerting,
            Phase::Breaking,
            Phase::Waiting,
        ] {
            let mut inner = working_inner();
            inner.phase = phase;
            assert!(apply_enter_pause_window(
                &mut inner,
                PauseSource::Dnd
            ));
            assert_eq!(inner.phase, Phase::Paused);
            assert_eq!(inner.paused_source, PauseSource::Dnd);
            assert_eq!(inner.paused_phase, Some(phase));
        }
        // 已 Paused：不重复进入（来源切换由 tick 判定块直写，不经此函数）
        let mut inner = working_inner();
        inner.phase = Phase::Paused;
        assert!(!apply_enter_pause_window(
            &mut inner,
            PauseSource::Quiet
        ));
    }

    #[test]
    fn resume_rejected_for_period_sources_allowed_for_manual() {
        // 时段暂停：手动恢复被拒（等 tick 自动 start_work）
        for source in [PauseSource::Quiet, PauseSource::Dnd] {
            let mut inner = working_inner();
            assert!(apply_enter_pause_window(&mut inner, source));
            assert!(!apply_resume(&mut inner, 2000));
            assert_eq!(inner.phase, Phase::Paused);
        }
        // 手动暂停：恢复回原 phase 并重设 target_epoch
        let mut inner = working_inner();
        assert!(apply_enter_paused(&mut inner));
        assert!(apply_resume(&mut inner, 2000));
        assert_eq!(inner.phase, Phase::Working);
        assert_eq!(inner.target_epoch, 2000 + 600);
    }

    // —— apply_start_work：作废当前轮、来源归位 Manual（时段结束退出的语义基础）——

    #[test]
    fn start_work_resets_pause_source() {
        let mut inner = working_inner();
        assert!(apply_enter_pause_window(
            &mut inner,
            PauseSource::Quiet
        ));
        apply_start_work(&mut inner, 1800, 5000);
        assert_eq!(inner.phase, Phase::Working);
        assert_eq!(inner.paused_source, PauseSource::Manual);
        assert_eq!(inner.remaining_seconds, 1800);
        assert_eq!(inner.target_epoch, 5000 + 1800);
    }

    // —— build_payload：pause_source 序列化（serde 小写传输契约）——

    #[test]
    fn payload_serializes_pause_source_lowercase() {
        let mut inner = working_inner();
        inner.phase = Phase::Paused;
        inner.paused_source = PauseSource::Dnd;
        let json = serde_json::to_string(&build_payload(&inner, None)).unwrap();
        assert!(json.contains("\"pauseSource\":\"dnd\""));
    }
}
