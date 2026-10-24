/*
 * @Author: Danson zheng
 * @Date: 2026-10-23
 * @Description: 桌面应用入口，调用库中的 Tauri 启动逻辑。
 */

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    token_tracker_lib::run();
}
