/*
 * main.rs — 程序入口文件
 *
 * 【Rust 项目的入口】
 * 每个 Rust 可执行程序（binary crate）都必须有一个 main() 函数作为入口。
 * 这个文件就是 `cargo run` 或双击运行时，操作系统首先执行的代码。
 *
 * 【文件结构说明】
 * Tauri v2 项目通常把实际逻辑放在 lib.rs 中（作为库 crate），
 * 而 main.rs 只是一个"启动器"，调用 lib.rs 中定义的 run() 函数。
 * 这样做的好处：移动端（iOS/Android）也能复用同一个 lib.rs 中的代码。
 *
 * 【Rust 语法知识】
 * fn  → 定义函数的关键字（function 的缩写）
 * pub → "public" 的缩写，表示公开可见（类似 Java 的 public）
 * ::  → 路径/模块分隔符（类似文件路径的 /）
 * #![] → 全局属性（attribute），作用于整个文件或 crate
 * #[]  → 属性（attribute），作用于紧随其后的项（函数、结构体等）
 */

/* #![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
 *
 * 作用：在 Windows 上隐藏控制台窗口（黑色的 cmd 窗口）。
 * 这是一个全局属性（#![...] 表示作用域是整个文件）。
 *
 * 逐层拆解：
 *   cfg_attr(条件, 属性) → Rust 的条件编译属性
 *     如果"条件"为真，就应用"属性"；否则忽略
 *
 *   not(debug_assertions) → 取反操作
 *     debug_assertions 是 Rust 内置的配置标志：
 *       debug 模式编译时（cargo build）→ debug_assertions = true
 *       release 模式编译时（cargo build --release）→ debug_assertions = false
 *     所以 not(debug_assertions) 在 release 模式下为 true
 *
 *   windows_subsystem = "windows" → Windows 专属属性
 *     告诉 Windows 系统：这个程序是 GUI 程序，不要创建控制台窗口
 *     如果不加这行，release 版本运行时会弹出一个黑色控制台窗口
 *
 * 综合效果：
 *   release 模式 → 隐藏控制台窗口（用户看到的是干净的 GUI）
 *   debug 模式   → 保留控制台窗口（方便开发者看日志输出）
 *
 * 原注释说 "DO NOT REMOVE!!"，因为删掉这行会导致 Windows 用户看到多余的黑窗口
 */
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/**
 * fn main() { ... }
 *
 * 程序的入口函数。
 * 当用户双击应用图标或执行 `cargo run` 时，操作系统从这里开始执行。
 *
 * 这里只做了一件事：调用 lib.rs 中定义的 `run()` 函数。
 * 使用 `ocean_health_tick_lib::` 前缀是因为 lib.rs 被编译为名为
 * `ocean_health_tick_lib` 的库 crate（在 Cargo.toml 的 [lib] 中定义的 name）。
 *
 * 调用关系：
 *   main.rs → ocean_health_tick_lib::run() → lib.rs 中的 pub fn run()
 */
fn main() {
    /* 调用 lib.rs 中定义的 run() 函数来启动 Tauri 应用
     * `ocean_health_tick_lib` 是 crate 名（在 Cargo.toml 的 [lib].name 中定义）
     * `::run()` 是该 crate 中导出的公开函数
     */
    ocean_health_tick_lib::run();
}
