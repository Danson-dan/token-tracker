/**
 * @Author: Danson zheng
 * @Date: 2026-10-24
 * @Description: 空闲电源管理。所有运行中的智能体都没有活跃任务并持续空闲指定分钟后，
 * 按用户设置执行系统休眠或关机。默认关闭，设置持久化到 ~/.token-tracker/power.json。
 */

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// 空闲分钟数下限与上限，避免误填 0 分钟立即休眠或超大值永不触发
const MIN_IDLE_MINUTES: u32 = 1;
const MAX_IDLE_MINUTES: u32 = 1_440;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PowerAction {
    #[default]
    Sleep,
    Shutdown,
}

impl PowerAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sleep => "休眠",
            Self::Shutdown => "关机",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerSettings {
    /// 默认关闭，只有用户显式开启后才会执行电源操作
    pub enabled: bool,
    /// 全部任务完成后的空闲分钟数
    pub idle_minutes: u32,
    pub action: PowerAction,
}

impl Default for PowerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            idle_minutes: 15,
            action: PowerAction::Sleep,
        }
    }
}

impl PowerSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !(MIN_IDLE_MINUTES..=MAX_IDLE_MINUTES).contains(&self.idle_minutes) {
            return Err(format!(
                "空闲分钟数需在 {MIN_IDLE_MINUTES} 到 {MAX_IDLE_MINUTES} 之间"
            ));
        }
        Ok(())
    }
}

/// 电源设置存储；无法定位主目录时只使用内存默认值，保存会返回错误
#[derive(Debug, Clone)]
pub struct PowerSettingsStore {
    path: Option<PathBuf>,
}

impl Default for PowerSettingsStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PowerSettingsStore {
    pub fn new() -> Self {
        Self {
            path: default_home_dir()
                .map(|home| home.join(".token-tracker").join("power.json")),
        }
    }

    /// 指定配置文件路径，供测试使用
    #[cfg(test)]
    pub fn at(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    pub fn load(&self) -> PowerSettings {
        let Some(path) = self.path.as_deref() else {
            log::warn!("【电源设置】无法定位用户主目录，使用默认设置");
            return PowerSettings::default();
        };
        match fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(settings) => settings,
                Err(error) => {
                    log::warn!(
                        "【电源设置】配置文件解析失败，使用默认设置 path={} error={}",
                        path.display(),
                        error
                    );
                    PowerSettings::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                PowerSettings::default()
            }
            Err(error) => {
                log::warn!(
                    "【电源设置】读取配置失败，使用默认设置 path={} error={}",
                    path.display(),
                    error
                );
                PowerSettings::default()
            }
        }
    }

    pub fn save(&self, settings: &PowerSettings) -> Result<(), String> {
        settings.validate()?;
        let Some(path) = self.path.as_deref() else {
            return Err("无法定位用户主目录，电源设置未保存".to_string());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("创建配置目录失败: {error}"))?;
        }
        let content =
            serde_json::to_string_pretty(settings).map_err(|error| format!("序列化配置失败: {error}"))?;
        fs::write(path, content).map_err(|error| format!("写入配置失败: {error}"))
    }
}

/// 空闲计时器。只有至少一个工具在运行且所有工具都没有活跃任务时才开始计时；
/// 任意活跃任务都会重置计时，触发后保持 fired 直到重新出现活动。
#[derive(Debug, Default)]
pub struct IdleTracker {
    idle_since_ms: Option<i64>,
    fired: bool,
}

impl IdleTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 返回是否应当在本轮执行电源操作；内部会维护计时起点与已触发状态
    pub fn observe(
        &mut self,
        now_ms: i64,
        has_running_tool: bool,
        active_tasks: u32,
        settings: &PowerSettings,
    ) -> bool {
        if !settings.enabled || !has_running_tool || active_tasks > 0 {
            self.idle_since_ms = None;
            self.fired = false;
            return false;
        }

        let since_ms = *self.idle_since_ms.get_or_insert(now_ms);
        if self.fired {
            return false;
        }

        let idle_ms = now_ms.saturating_sub(since_ms);
        let threshold_ms = i64::from(settings.idle_minutes) * 60_000;
        if idle_ms >= threshold_ms {
            self.fired = true;
            true
        } else {
            false
        }
    }

    /// 当前连续空闲时长，供测试断言
    #[cfg(test)]
    pub fn idle_ms(&self, now_ms: i64) -> Option<i64> {
        self.idle_since_ms.map(|since_ms| now_ms.saturating_sub(since_ms))
    }
}

/// 执行系统电源操作；仅支持固定的休眠与关机动作，不接受任意命令
pub fn execute(action: PowerAction) -> Result<(), String> {
    log::warn!("【自动电源】开始执行系统{}", action.label());
    match action {
        PowerAction::Sleep => platform_sleep(),
        PowerAction::Shutdown => platform_shutdown(),
    }
}

#[cfg(target_os = "macos")]
fn platform_sleep() -> Result<(), String> {
    run_command(
        "osascript",
        &["-e", "tell application \"System Events\" to sleep"],
    )
}

#[cfg(target_os = "macos")]
fn platform_shutdown() -> Result<(), String> {
    run_command(
        "osascript",
        &["-e", "tell application \"System Events\" to shut down"],
    )
}

#[cfg(target_os = "windows")]
fn platform_sleep() -> Result<(), String> {
    run_command(
        "rundll32.exe",
        &["powrprof.dll,SetSuspendState", "0,1,0"],
    )
}

#[cfg(target_os = "windows")]
fn platform_shutdown() -> Result<(), String> {
    run_command("shutdown", &["/s", "/t", "30"])
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_sleep() -> Result<(), String> {
    Err("当前系统暂不支持自动休眠".to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_shutdown() -> Result<(), String> {
    Err("当前系统暂不支持自动关机".to_string())
}

fn run_command(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("调用 {program} 失败: {error}"))?;
    if output.status.success() {
        log::info!("【自动电源】系统电源命令执行成功 program={program}");
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "系统电源命令失败 program={program} status={} stderr={}",
            output.status,
            stderr.trim()
        ))
    }
}

fn default_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "token-tracker-power-{}-{}",
            std::process::id(),
            name
        ))
    }

    #[test]
    fn default_settings_are_disabled_and_valid() {
        let settings = PowerSettings::default();
        assert!(!settings.enabled);
        assert_eq!(settings.action, PowerAction::Sleep);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn idle_minutes_are_validated() {
        let mut settings = PowerSettings::default();
        settings.idle_minutes = 0;
        assert!(settings.validate().is_err());
        settings.idle_minutes = MAX_IDLE_MINUTES + 1;
        assert!(settings.validate().is_err());
        settings.idle_minutes = MAX_IDLE_MINUTES;
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn settings_round_trip_through_json_file() {
        let path = temp_path("settings.json");
        let store = PowerSettingsStore::at(path.clone());
        let settings = PowerSettings {
            enabled: true,
            idle_minutes: 30,
            action: PowerAction::Shutdown,
        };

        store.save(&settings).expect("设置应保存成功");
        assert!(path.is_file());
        assert_eq!(store.load(), settings);

        fs::remove_file(&path).expect("测试文件应可删除");
    }

    #[test]
    fn invalid_settings_are_not_written() {
        let path = temp_path("invalid.json");
        let store = PowerSettingsStore::at(path.clone());
        let settings = PowerSettings {
            enabled: true,
            idle_minutes: 0,
            action: PowerAction::Sleep,
        };
        assert!(store.save(&settings).is_err());
        assert!(!path.is_file());
    }

    #[test]
    fn idle_timer_triggers_once_after_threshold() {
        let settings = PowerSettings {
            enabled: true,
            idle_minutes: 5,
            action: PowerAction::Sleep,
        };
        let mut tracker = IdleTracker::new();
        let start = 1_000_000;
        let threshold = 5 * 60_000;

        assert!(!tracker.observe(start, true, 0, &settings));
        assert!(!tracker.observe(start + threshold - 1, true, 0, &settings));
        assert!(tracker.observe(start + threshold, true, 0, &settings));
        assert!(!tracker.observe(start + threshold + 60_000, true, 0, &settings));

        // 出现新活动后重新计时
        assert!(!tracker.observe(start + threshold + 120_000, true, 1, &settings));
        assert_eq!(tracker.idle_ms(start + threshold + 120_000), None);
        assert!(!tracker.observe(start + threshold + 180_000, true, 0, &settings));
        assert!(tracker.observe(start + 2 * threshold + 180_000, true, 0, &settings));
    }

    #[test]
    fn disabled_or_without_running_tools_never_triggers() {
        let mut tracker = IdleTracker::new();
        let disabled = PowerSettings::default();
        assert!(!tracker.observe(0, true, 0, &disabled));
        assert_eq!(tracker.idle_ms(10 * 60_000), None);

        let enabled = PowerSettings {
            enabled: true,
            idle_minutes: 1,
            action: PowerAction::Sleep,
        };
        assert!(!tracker.observe(0, false, 0, &enabled));
        assert!(!tracker.observe(10 * 60_000, false, 0, &enabled));
        assert_eq!(tracker.idle_ms(10 * 60_000), None);
    }
}
