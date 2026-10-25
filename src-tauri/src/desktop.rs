/**
 * @Author: Danson zheng
 * @Date: 2026-10-23
 * @Description: Tauri 桌面壳。后台线程持续采集日志、检测运行中的智能体并刷新共享快照，
 * 前端轮询快照渲染卡片与小窗；同时负责单工具小窗创建与空闲休眠/关机设置。
 */

use crate::collector::LogCollector;
use crate::detection::{self, ToolDetector, DETECTION_INTERVAL_MS};
use crate::live::{LiveMetricsEngine, LiveSnapshot, WorkStatus};
use crate::power::{self, IdleTracker, PowerSettings, PowerSettingsStore};
use crate::tools::ToolId;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Monitor, State, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

/// 采集与快照刷新间隔；与前端轮询节奏一致，保证速度窗口实时
const POLL_INTERVAL_MS: u64 = 1_000;
const MAIN_WINDOW_WIDTH: f64 = 640.0;
const MAIN_WINDOW_HEIGHT: f64 = 600.0;
const COMPACT_WINDOW_SIZE: f64 = 132.0;

/// 紧凑模式需要后端持久化：窗口创建时就要用正确尺寸，避免先展示全尺寸再缩小
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct UiPreferences {
    compact: bool,
}

#[derive(Debug, Clone)]
struct UiPreferencesStore {
    path: Option<PathBuf>,
}

impl UiPreferencesStore {
    fn new() -> Self {
        let path = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| PathBuf::from(home).join(".token-tracker").join("ui.json"));
        Self { path }
    }

    fn load(&self) -> UiPreferences {
        let Some(path) = self.path.as_deref() else {
            return UiPreferences::default();
        };
        match fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|error| {
                log::warn!(
                    "【桌面壳】界面配置解析失败，使用默认值 path={} error={}",
                    path.display(),
                    error
                );
                UiPreferences::default()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                UiPreferences::default()
            }
            Err(error) => {
                log::warn!(
                    "【桌面壳】界面配置读取失败，使用默认值 path={} error={}",
                    path.display(),
                    error
                );
                UiPreferences::default()
            }
        }
    }

    fn save(&self, preferences: UiPreferences) -> Result<(), String> {
        let Some(path) = self.path.as_deref() else {
            return Err("无法定位用户主目录，界面配置未保存".to_string());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("创建配置目录失败: {error}"))?;
        }
        let content = serde_json::to_string_pretty(&preferences)
            .map_err(|error| format!("序列化界面配置失败: {error}"))?;
        fs::write(path, content).map_err(|error| format!("写入界面配置失败: {error}"))
    }
}

struct DesktopState {
    snapshot: Mutex<LiveSnapshot>,
    power: Mutex<PowerSettings>,
    idle: Mutex<IdleTracker>,
    store: PowerSettingsStore,
    ui: UiPreferencesStore,
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

/// NSWindow.center 在部分缩放下会按旧尺寸计算位置，这里显式取显示器参数定位
fn centered_position(monitor: &Monitor, width: f64, height: f64) -> LogicalPosition<f64> {
    let scale = monitor.scale_factor();
    let monitor_size = monitor.size().to_logical::<f64>(scale);
    let monitor_position = monitor.position().to_logical::<f64>(scale);
    LogicalPosition::new(
        monitor_position.x + (monitor_size.width - width) / 2.0,
        monitor_position.y + (monitor_size.height - height) / 2.0,
    )
}

/// 主窗在完整面板与 token 图标之间切换；缩小时保持左上角不动，展开后回到屏幕中央
#[tauri::command]
fn set_compact_mode(
    state: State<'_, Arc<DesktopState>>,
    window: WebviewWindow,
    compact: bool,
) -> Result<(), String> {
    let size = if compact {
        LogicalSize::new(COMPACT_WINDOW_SIZE, COMPACT_WINDOW_SIZE)
    } else {
        LogicalSize::new(MAIN_WINDOW_WIDTH, MAIN_WINDOW_HEIGHT)
    };
    window
        .set_size(size)
        .map_err(|error| format!("调整窗口大小失败: {error}"))?;
    if !compact {
        match window.current_monitor() {
            Ok(Some(monitor)) => {
                let position =
                    centered_position(&monitor, MAIN_WINDOW_WIDTH, MAIN_WINDOW_HEIGHT);
                window
                    .set_position(position)
                    .map_err(|error| format!("窗口居中失败: {error}"))?;
            }
            Ok(None) => {
                window
                    .center()
                    .map_err(|error| format!("窗口居中失败: {error}"))?;
            }
            Err(error) => log::warn!("【桌面壳】读取显示器信息失败 error={error}"),
        }
    }
    if let Err(error) = state.ui.save(UiPreferences { compact }) {
        log::warn!("【桌面壳】紧凑状态保存失败 error={error}");
    }
    log::info!(
        "【桌面壳】窗口模式已切换 compact={compact} target={}x{}",
        size.width,
        size.height
    );
    Ok(())
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    log::info!("【桌面壳】收到退出请求，应用即将退出");
    app.exit(0);
}

/// 启动桌面应用；采集失败不会阻塞窗口展示，界面会退化为空闲状态
pub fn run() {
    let started_at = Instant::now();
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

    let ui_store = UiPreferencesStore::new();
    let initial_compact = ui_store.load().compact;
    log::info!("【桌面壳】已加载界面模式 compact={initial_compact}");

    let mut initial_engine = LiveMetricsEngine::new();
    let state = Arc::new(DesktopState {
        snapshot: Mutex::new(initial_engine.snapshot_now()),
        power: Mutex::new(power_settings),
        idle: Mutex::new(IdleTracker::new()),
        store,
        ui: ui_store,
    });

    let worker_state = Arc::clone(&state);
    log::info!(
        "【桌面壳】开始创建窗口 elapsed_ms={}",
        started_at.elapsed().as_millis()
    );
    tauri::Builder::default()
        .manage(state)
        .setup(move |app| {
            log::info!(
                "【桌面壳】窗口初始化耗时 elapsed_ms={}",
                started_at.elapsed().as_millis()
            );
            // 主窗由 setup 按持久化模式直接创建：隐藏窗口上的 set_size 会被忽略，先显示再缩小会闪一下全尺寸
            let Some(config) = app
                .config()
                .app
                .windows
                .iter()
                .find(|window| window.label == "main")
                .cloned()
            else {
                log::error!("【桌面壳】未找到主窗配置，应用将保持不可见");
                return Ok(());
            };
            let size = if initial_compact {
                LogicalSize::new(COMPACT_WINDOW_SIZE, COMPACT_WINDOW_SIZE)
            } else {
                LogicalSize::new(MAIN_WINDOW_WIDTH, MAIN_WINDOW_HEIGHT)
            };
            let position = match app.primary_monitor() {
                Ok(Some(monitor)) => {
                    let scale = monitor.scale_factor();
                    let monitor_size = monitor.size().to_logical::<f64>(scale);
                    log::info!(
                        "【桌面壳】主显示器 position={:?} size={}x{} scale={scale}",
                        monitor.position(),
                        monitor_size.width,
                        monitor_size.height
                    );
                    Some(centered_position(&monitor, size.width, size.height))
                }
                Ok(None) => None,
                Err(error) => {
                    log::warn!("【桌面壳】读取主显示器信息失败 error={error}");
                    None
                }
            };
            let mut builder = match WebviewWindowBuilder::from_config(app.handle(), &config) {
                Ok(builder) => builder
                    .inner_size(size.width, size.height)
                    .visible(true)
                    // 前端以该值为准，避免 WebView 里遗留的 localStorage 与后端偏好分叉
                    .initialization_script(format!(
                        "window.__TT_INITIAL_COMPACT__ = {initial_compact};"
                    )),
                Err(error) => {
                    log::error!("【桌面壳】读取主窗配置失败，应用将保持不可见 error={error}");
                    return Ok(());
                }
            };
            if let Some(position) = position {
                builder = builder.position(position.x, position.y);
            } else {
                builder = builder.center();
            }
            let window = match builder.build() {
                Ok(window) => window,
                Err(error) => {
                    log::error!("【桌面壳】创建主窗失败，应用将保持不可见 error={error}");
                    return Ok(());
                }
            };
            match window.inner_size() {
                Ok(size) => log::info!(
                    "【桌面壳】主窗已显示 compact={initial_compact} size={}x{}",
                    size.width,
                    size.height
                ),
                Err(error) => log::warn!("【桌面壳】主窗已显示，读取尺寸失败 error={error}"),
            }
            // 首次全量扫描较重，放到窗口创建之后，避免与 WebView 启动争抢磁盘和 CPU
            if let Err(error) = thread::Builder::new()
                .name("token-tracker-collector".to_string())
                .spawn(move || collect_loop(worker_state))
            {
                log::error!("【桌面壳】采集线程启动失败，界面将保持空闲 error={error}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_power_settings,
            update_power_settings,
            open_tool_window,
            close_window,
            set_compact_mode,
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

        let added = collector.poll_into(&mut engine, now_ms);
        let mut snapshot = engine.snapshot_at_with_running(now_ms, &running_tools);
        snapshot.running_tools = running_tools.clone();
        snapshot.installed_tools = installed_tools.clone();

        let settings = *lock_or_recover(&state.power);
        let active_tasks: u32 = snapshot
            .running_tools
            .iter()
            .filter_map(|tool| snapshot.tool(*tool))
            .map(|tool| {
                let busy_status =
                    matches!(tool.status, WorkStatus::Working | WorkStatus::NeedsConfirmation);
                tool.active_tasks.max(u32::from(busy_status))
            })
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
