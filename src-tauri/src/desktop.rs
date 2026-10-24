/**
 * @Author: Danson zheng
 * @Date: 2026-10-23
 * @Description: Tauri 桌面壳。后台线程持续采集日志、检测运行中的智能体并刷新共享快照，
 * 前端轮询快照渲染卡片与小窗；同时负责单工具小窗创建与空闲休眠/关机设置。
 */

use crate::collector::LogCollector;
use crate::detection::{self, ToolDetector, DETECTION_INTERVAL_MS};
use crate::live::{LiveMetricsEngine, LiveSnapshot};
use crate::power::{self, IdleTracker, PowerSettings, PowerSettingsStore};
use crate::tools::ToolId;
use chrono::Utc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// 采集与快照刷新间隔；与前端轮询节奏一致，保证速度窗口实时
const POLL_INTERVAL_MS: u64 = 1_000;

struct DesktopState {
    snapshot: Mutex<LiveSnapshot>,
    power: Mutex<PowerSettings>,
    idle: Mutex<IdleTracker>,
    store: PowerSettingsStore,
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[tauri::command]
fn get_snapshot(state: State<'_, Arc<DesktopState>>) -> LiveSnapshot {
    lock_or_recover(&state.snapshot).clone()
}

#[tauri::command]
fn get_power_settings(state: State<'_, Arc<DesktopState>>) -> PowerSettings {
    *lock_or_recover(&state.power)
}

#[tauri::command]
fn update_power_settings(
    state: State<'_, Arc<DesktopState>>,
    settings: PowerSettings,
) -> Result<PowerSettings, String> {
    settings.validate()?;
    state.store.save(&settings)?;
    *lock_or_recover(&state.power) = settings;
    *lock_or_recover(&state.idle) = IdleTracker::new();
    log::info!(
        "【电源设置】已更新 enabled={} idle_minutes={} action={}",
        settings.enabled,
        settings.idle_minutes,
        settings.action.label()
    );
    Ok(settings)
}

#[tauri::command]
fn open_tool_window(app: AppHandle, tool_id: String) -> Result<(), String> {
    let Some(tool) = ToolId::from_id(&tool_id) else {
        return Err(format!("未知工具: {tool_id}"));
    };
    let label = format!("mini-{}", tool.id());
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }

    let url = WebviewUrl::App(format!("index.html?tool={}&mini=1", tool.id()).into());
    WebviewWindowBuilder::new(&app, &label, url)
        .title(tool.display_name())
        .inner_size(340.0, 280.0)
        .min_inner_size(300.0, 250.0)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .center()
        .build()
        .map_err(|error| format!("创建小窗失败: {error}"))?;

    log::info!("【桌面壳】已打开单工具小窗 tool={}", tool.display_name());
    Ok(())
}

#[tauri::command]
fn close_window(window: WebviewWindow) -> Result<(), String> {
    window.close().map_err(|error| format!("关闭窗口失败: {error}"))
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    log::info!("【桌面壳】收到退出请求，应用即将退出");
    app.exit(0);
}

/// 启动桌面应用；采集失败不会阻塞窗口展示，界面会退化为空闲状态
pub fn run() {
    if let Err(error) = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .try_init()
    {
        eprintln!("【桌面壳】日志初始化失败 error={error}");
    }

    let store = PowerSettingsStore::new();
    let power_settings = store.load();
    log::info!(
        "【电源设置】已加载 enabled={} idle_minutes={} action={}",
        power_settings.enabled,
        power_settings.idle_minutes,
        power_settings.action.label()
    );

    let mut initial_engine = LiveMetricsEngine::new();
    let state = Arc::new(DesktopState {
        snapshot: Mutex::new(initial_engine.snapshot_now()),
        power: Mutex::new(power_settings),
        idle: Mutex::new(IdleTracker::new()),
        store,
    });

    let worker_state = Arc::clone(&state);
    if let Err(error) = thread::Builder::new()
        .name("token-tracker-collector".to_string())
        .spawn(move || collect_loop(worker_state))
    {
        log::error!("【桌面壳】采集线程启动失败，界面将保持空闲 error={error}");
    }

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_power_settings,
            update_power_settings,
            open_tool_window,
            close_window,
            quit_app
        ])
        .run(tauri::generate_context!())
        .expect("Tauri 应用启动失败");
}

fn collect_loop(state: Arc<DesktopState>) {
    let mut collector = LogCollector::new();
    let mut engine = LiveMetricsEngine::new();
    let detector = ToolDetector::new();
    let installed_tools = detector.installed_tools();
    let mut running_tools: Vec<ToolId> = Vec::new();
    let mut last_detection_ms = 0i64;

    log::info!(
        "【工具检测】启动时检测到已安装 {} 个工具: {:?}",
        installed_tools.len(),
        detection::tool_names(&installed_tools)
    );

    loop {
        let now_ms = Utc::now().timestamp_millis();
        let added = collector.poll_into(&mut engine, now_ms);
        let mut snapshot = engine.snapshot_at(now_ms);

        if last_detection_ms == 0 || now_ms - last_detection_ms >= DETECTION_INTERVAL_MS {
            last_detection_ms = now_ms;
            let detected = detector.running_tools();
            if detected != running_tools {
                log::info!(
                    "【工具检测】运行中工具发生变化 count={} tools={:?}",
                    detected.len(),
                    detection::tool_names(&detected)
                );
                running_tools = detected;
            }
        }
        snapshot.running_tools = running_tools.clone();
        snapshot.installed_tools = installed_tools.clone();

        let settings = *lock_or_recover(&state.power);
        let active_tasks: u32 = snapshot
            .running_tools
            .iter()
            .filter_map(|tool| snapshot.tool(*tool))
            .map(|tool| tool.active_tasks)
            .sum();
        let has_running_tool = !snapshot.running_tools.is_empty();
        let should_trigger = lock_or_recover(&state.idle).observe(
            now_ms,
            has_running_tool,
            active_tasks,
            &settings,
        );
        if should_trigger {
            log::warn!(
                "【自动电源】所有任务已完成并空闲 {} 分钟，准备{}",
                settings.idle_minutes,
                settings.action.label()
            );
            thread::spawn(move || {
                if let Err(error) = power::execute(settings.action) {
                    log::error!("【自动电源】执行失败 error={error}");
                }
            });
        }

        if added > 0 {
            log::info!("【桌面壳】实时快照已刷新新增事件数={added}");
        }
        *lock_or_recover(&state.snapshot) = snapshot;
        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}
