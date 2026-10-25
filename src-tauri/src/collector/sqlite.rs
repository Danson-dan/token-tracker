/**
 * @Author: Danson zheng
 * @Date: 2026-10-24
 * @Description: SQLite 数据源采集器。Qoder 读取旧版 chat_message.token_info，
 * Qoder/Qoder CN 从活跃轮次库读取实时工作状态，Hermes 读取 sessions 汇总 token，
 * Trae / Trae CN 分别读取本地 AI 会话状态；所有查询只访问 token、时间、状态字段，
 * 不读取消息正文。
 */

use super::{task_id_for_text, CollectBatch};
use crate::live::{StatusEvent, UsageEvent, WorkStatus};
use crate::tools::ToolId;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DB_POLL_INTERVAL_MS: i64 = 5_000;
const TRAE_SCAN_INTERVAL_MS: i64 = 5_000;
const HERMES_ACTIVE_WINDOW_MS: i64 = 12 * 60 * 60 * 1_000;
/// 活跃状态心跳间隔；长时间没有新行时定期续期，避免前端状态过期
const STATUS_HEARTBEAT_MS: i64 = 60_000;
/// active_turn_projection_items 只保留进行中的轮次；超过该窗口未更新视为残留
const ACTIVE_TURN_WINDOW_MS: i64 = 5 * 60 * 1_000;

pub struct SqliteCollector {
    qoder_path: Option<PathBuf>,
    hermes_path: Option<PathBuf>,
    trae: TraeTracker,
    trae_cn: TraeTracker,
    active_turns: Vec<ActiveTurnTracker>,
    qoder_last_rowid: i64,
    qoder_request_marks: HashMap<String, i64>,
    hermes_seen: HashSet<String>,
    hermes_active_marks: HashMap<String, i64>,
    last_qoder_poll_ms: i64,
    last_hermes_poll_ms: i64,
    qoder_reported: bool,
    hermes_reported: bool,
}

#[derive(Debug)]
struct TraeTracker {
    roots: Vec<PathBuf>,
    files: HashMap<PathBuf, (u64, u64)>,
    status: Option<(WorkStatus, i64)>,
    last_emit_ms: i64,
    last_poll_ms: i64,
}

impl TraeTracker {
    fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            files: HashMap::new(),
            status: None,
            last_emit_ms: 0,
            last_poll_ms: 0,
        }
    }

    fn poll(&mut self, tool: ToolId, now_ms: i64, cutoff_ms: i64) -> Option<StatusEvent> {
        if !(self.last_poll_ms == 0 || now_ms - self.last_poll_ms >= TRAE_SCAN_INTERVAL_MS) {
            return None;
        }
        self.last_poll_ms = now_ms;

        let files = trae_state_files(&self.roots);
        let mut candidate: Option<(WorkStatus, i64)> = self.status;

        for path in files {
            let Some(fingerprint) = state_file_fingerprint(&path) else {
                continue;
            };
            if self.files.get(&path) == Some(&fingerprint) {
                continue;
            }
            self.files.insert(path.clone(), fingerprint);

            let Ok(connection) = open_readonly(&path) else {
                continue;
            };
            let Ok(mut statement) = connection.prepare(
                "SELECT value FROM ItemTable
                 WHERE key LIKE 'memento/icube-ai-agent-storage%'
                   AND key NOT LIKE '%input-history%'",
            ) else {
                continue;
            };
            let Ok(rows) = statement.query_map([], |row| row.get::<_, String>(0)) else {
                continue;
            };
            for value in rows.flatten() {
                if let Some(next) = trae_status_from_storage(&value, cutoff_ms) {
                    candidate = Some(match candidate {
                        Some(current) if current.1 >= next.1 => current,
                        _ => next,
                    });
                }
            }
        }

        if candidate == self.status {
            let (status, _) = self.status?;
            let needs_heartbeat = matches!(
                status,
                WorkStatus::Working | WorkStatus::NeedsConfirmation
            );
            if !needs_heartbeat || now_ms - self.last_emit_ms < STATUS_HEARTBEAT_MS {
                return None;
            }
            self.last_emit_ms = now_ms;
            return Some(StatusEvent {
                tool,
                task_id: task_id_for_text(tool.id()),
                at_ms: now_ms,
                status,
            });
        }
        let (status, at_ms) = candidate?;
        self.status = Some((status, at_ms));
        self.last_emit_ms = now_ms;
        Some(StatusEvent {
            tool,
            task_id: task_id_for_text(tool.id()),
            at_ms,
            status,
        })
    }
}

/// Qoder / Qoder CN 新版桌面端把当前轮次的流式条目写在 active_turn_projection_items，
/// 条目消失即代表轮次结束；用它补足实时工作状态（该库没有输出 token 字段）。
#[derive(Debug)]
struct ActiveTurnTracker {
    tool: ToolId,
    path: PathBuf,
    active: HashMap<String, u64>,
    /// 启动时发现的历史残留；时间戳不变时持续忽略，出现新条目后自动恢复
    ignored: HashMap<String, i64>,
    last_poll_ms: i64,
    reported: bool,
    started: bool,
}

impl ActiveTurnTracker {
    fn new(tool: ToolId, path: PathBuf) -> Self {
        Self {
            tool,
            path,
            active: HashMap::new(),
            ignored: HashMap::new(),
            last_poll_ms: 0,
            reported: false,
            started: false,
        }
    }

    fn poll(&mut self, now_ms: i64, cutoff_ms: i64) -> Vec<StatusEvent> {
        if !(self.last_poll_ms == 0 || now_ms - self.last_poll_ms >= DB_POLL_INTERVAL_MS) {
            return Vec::new();
        }
        self.last_poll_ms = now_ms;
        let first_poll = !self.started;

        let Ok(connection) = open_readonly(&self.path) else {
            return Vec::new();
        };
        let mut statement = match connection.prepare(
            "SELECT session_id, turn_id, MAX(created_at)
             FROM active_turn_projection_items
             GROUP BY session_id, turn_id",
        ) {
            Ok(statement) => statement,
            Err(error) => {
                log::debug!(
                    "【SQLite 采集】{} 活跃轮次查询失败 error={error}",
                    self.tool.display_name()
                );
                return Vec::new();
            }
        };
        let mut rows = match statement.query([]) {
            Ok(rows) => rows,
            Err(error) => {
                log::debug!(
                    "【SQLite 采集】{} 活跃轮次读取失败 error={error}",
                    self.tool.display_name()
                );
                return Vec::new();
            }
        };
        self.started = true;

        let mut current: HashMap<String, u64> = HashMap::new();
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(row) = rows.next().ok().flatten() {
            let session_id: String = row.get(0).unwrap_or_default();
            let turn_id: String = row.get(1).unwrap_or_default();
            let updated_at: i64 = row.get(2).unwrap_or_default();
            if turn_id.is_empty() || updated_at < cutoff_ms {
                continue;
            }
            let key = format!("{session_id}\u{1f}{turn_id}");
            seen.insert(key.clone());

            // 仅首轮过滤历史残留；运行中的长任务即使几分钟没有新条目也保持工作状态
            if first_poll && now_ms - updated_at > ACTIVE_TURN_WINDOW_MS {
                self.ignored.insert(key, updated_at);
                continue;
            }
            if let Some(ignored_at) = self.ignored.get(&key) {
                if *ignored_at >= updated_at {
                    continue;
                }
                self.ignored.remove(&key);
            }
            current.insert(key, task_id_for_text(&turn_id));
        }
        self.ignored.retain(|key, _| seen.contains(key));

        let mut statuses = Vec::new();
        if !current.is_empty() && !self.reported {
            log::info!(
                "【SQLite 采集】{} 已接入实时工作状态",
                self.tool.display_name()
            );
            self.reported = true;
        }
        for (key, task_id) in &current {
            self.active.insert(key.clone(), *task_id);
            statuses.push(StatusEvent {
                tool: self.tool,
                task_id: *task_id,
                at_ms: now_ms,
                status: WorkStatus::Working,
            });
        }

        let finished: Vec<String> = self
            .active
            .keys()
            .filter(|key| !current.contains_key(*key))
            .cloned()
            .collect();
        for key in finished {
            if let Some(task_id) = self.active.remove(&key) {
                statuses.push(StatusEvent {
                    tool: self.tool,
                    task_id,
                    at_ms: now_ms,
                    status: WorkStatus::Stopped,
                });
            }
        }

        statuses
    }
}

impl SqliteCollector {
    pub fn new() -> Self {
        let Some(home) = default_home_dir() else {
            return Self::empty();
        };
        let qoder_path = qoder_candidate_paths(&home)
            .into_iter()
            .find(|path| path.is_file());
        let hermes_path = home.join(".hermes").join("state.db");
        let active_turns = active_turn_db_paths(&home)
            .into_iter()
            .map(|(tool, path)| ActiveTurnTracker::new(tool, path))
            .collect();

        Self {
            qoder_path,
            hermes_path: hermes_path.is_file().then_some(hermes_path),
            trae: TraeTracker::new(trae_user_roots(&home, "Trae")),
            trae_cn: TraeTracker::new(trae_user_roots(&home, "Trae CN")),
            active_turns,
            qoder_last_rowid: 0,
            qoder_request_marks: HashMap::new(),
            hermes_seen: HashSet::new(),
            hermes_active_marks: HashMap::new(),
            last_qoder_poll_ms: 0,
            last_hermes_poll_ms: 0,
            qoder_reported: false,
            hermes_reported: false,
        }
    }

    fn empty() -> Self {
        Self {
            qoder_path: None,
            hermes_path: None,
            trae: TraeTracker::new(Vec::new()),
            trae_cn: TraeTracker::new(Vec::new()),
            active_turns: Vec::new(),
            qoder_last_rowid: 0,
            qoder_request_marks: HashMap::new(),
            hermes_seen: HashSet::new(),
            hermes_active_marks: HashMap::new(),
            last_qoder_poll_ms: 0,
            last_hermes_poll_ms: 0,
            qoder_reported: false,
            hermes_reported: false,
        }
    }

    pub fn poll(&mut self, now_ms: i64, cutoff_ms: i64) -> CollectBatch {
        let mut batch = CollectBatch::default();

        if self.last_qoder_poll_ms == 0
            || now_ms - self.last_qoder_poll_ms >= DB_POLL_INTERVAL_MS
        {
            self.last_qoder_poll_ms = now_ms;
            if let Some(path) = self.qoder_path.clone() {
                match self.poll_qoder(&path, cutoff_ms) {
                    Ok((events, statuses)) => {
                        if !events.is_empty() && !self.qoder_reported {
                            log::info!(
                                "【SQLite 采集】Qoder 已接入 token 记录 count={}",
                                events.len()
                            );
                            self.qoder_reported = true;
                        }
                        batch.events.extend(events);
                        batch.statuses.extend(statuses);
                    }
                    Err(error) => log::debug!("【SQLite 采集】Qoder 查询失败 error={error}"),
                }
            }
        }

        if self.last_hermes_poll_ms == 0
            || now_ms - self.last_hermes_poll_ms >= DB_POLL_INTERVAL_MS
        {
            self.last_hermes_poll_ms = now_ms;
            if let Some(path) = self.hermes_path.clone() {
                match self.poll_hermes(&path, now_ms, cutoff_ms) {
                    Ok((events, statuses)) => {
                        if !events.is_empty() && !self.hermes_reported {
                            log::info!(
                                "【SQLite 采集】Hermes 已接入会话汇总 token count={}",
                                events.len()
                            );
                            self.hermes_reported = true;
                        }
                        batch.events.extend(events);
                        batch.statuses.extend(statuses);
                    }
                    Err(error) => log::debug!("【SQLite 采集】Hermes 查询失败 error={error}"),
                }
            }
        }

        for tracker in &mut self.active_turns {
            batch.statuses.extend(tracker.poll(now_ms, cutoff_ms));
        }
        if let Some(status) = self.trae.poll(ToolId::TRAE, now_ms, cutoff_ms) {
            batch.statuses.push(status);
        }
        if let Some(status) = self.trae_cn.poll(ToolId::TRAE_CN, now_ms, cutoff_ms) {
            batch.statuses.push(status);
        }

        batch
    }

    fn poll_qoder(
        &mut self,
        path: &Path,
        cutoff_ms: i64,
    ) -> Result<(Vec<UsageEvent>, Vec<StatusEvent>), String> {
        let connection = open_readonly(path)?;
        let mut statement = connection
            .prepare(
                "SELECT m.rowid, m.session_id, m.request_id, m.gmt_create, m.token_info,
                        r.gmt_create, r.gmt_modified
                 FROM chat_message m
                 LEFT JOIN chat_record r ON r.request_id = m.request_id
                 WHERE m.role = 'assistant'
                   AND m.token_info IS NOT NULL AND m.token_info != ''
                   AND m.rowid > ?1 AND m.gmt_create >= ?2
                 ORDER BY m.rowid ASC",
            )
            .map_err(|error| format!("准备 Qoder 查询失败: {error}"))?;

        let mut rows = statement
            .query(rusqlite::params![self.qoder_last_rowid, cutoff_ms])
            .map_err(|error| format!("执行 Qoder 查询失败: {error}"))?;
        let mut events = Vec::new();

        while let Some(row) = rows
            .next()
            .map_err(|error| format!("读取 Qoder 行失败: {error}"))?
        {
            let rowid: i64 = row.get(0).unwrap_or_default();
            let session_id: String = row.get(1).unwrap_or_default();
            let request_id: String = row.get(2).unwrap_or_default();
            let created_at: i64 = row.get(3).unwrap_or_default();
            let token_info: String = row.get(4).unwrap_or_default();
            let record_start: Option<i64> = row.get(5).ok();
            let record_end: Option<i64> = row.get(6).ok();

            self.qoder_last_rowid = self.qoder_last_rowid.max(rowid);
            let output_tokens = completion_tokens(&token_info);
            let previous = self
                .qoder_request_marks
                .get(&request_id)
                .copied()
                .or(record_start)
                .or(record_end)
                .unwrap_or(created_at);
            self.qoder_request_marks.insert(request_id, created_at);

            if output_tokens == 0 {
                continue;
            }
            let start_ms = previous.max(cutoff_ms);
            if created_at <= start_ms {
                continue;
            }
            events.push(UsageEvent {
                tool: ToolId::QODER,
                task_id: task_id_for_text(&session_id),
                at_ms: created_at,
                output_tokens,
                duration_ms: (created_at - start_ms) as u64,
            });
        }

        Ok((events, Vec::new()))
    }

    fn poll_hermes(
        &mut self,
        path: &Path,
        now_ms: i64,
        cutoff_ms: i64,
    ) -> Result<(Vec<UsageEvent>, Vec<StatusEvent>), String> {
        let connection = open_readonly(path)?;
        let mut events = Vec::new();
        let mut statuses = Vec::new();

        {
            let mut statement = connection
                .prepare(
                    "SELECT id, started_at, ended_at, output_tokens
                     FROM sessions
                     WHERE ended_at IS NOT NULL AND output_tokens > 0
                       AND CAST(ended_at * 1000 AS INTEGER) >= ?1
                     ORDER BY ended_at ASC",
                )
                .map_err(|error| format!("准备 Hermes 查询失败: {error}"))?;
            let mut rows = statement
                .query(rusqlite::params![cutoff_ms])
                .map_err(|error| format!("执行 Hermes 查询失败: {error}"))?;
            while let Some(row) = rows
                .next()
                .map_err(|error| format!("读取 Hermes 行失败: {error}"))?
            {
                let id: String = row.get(0).unwrap_or_default();
                let started_at: f64 = row.get(1).unwrap_or_default();
                let ended_at: f64 = row.get(2).unwrap_or_default();
                let output_tokens: i64 = row.get(3).unwrap_or_default();
                self.hermes_active_marks.remove(&id);
                if !self.hermes_seen.insert(id.clone()) {
                    continue;
                }
                let at_ms = (ended_at * 1_000.0) as i64;
                let duration_ms = ((ended_at - started_at) * 1_000.0).max(0.0) as u64;
                if output_tokens <= 0 || duration_ms == 0 {
                    continue;
                }
                let task_id = task_id_for_text(&id);
                events.push(UsageEvent {
                    tool: ToolId::HERMES,
                    task_id,
                    at_ms,
                    output_tokens: output_tokens as u64,
                    duration_ms,
                });
                statuses.push(StatusEvent {
                    tool: ToolId::HERMES,
                    task_id,
                    at_ms,
                    status: WorkStatus::Stopped,
                });
            }
        }

        {
            let mut statement = connection
                .prepare(
                    "SELECT id, started_at FROM sessions
                     WHERE ended_at IS NULL AND started_at * 1000 >= ?1",
                )
                .map_err(|error| format!("准备 Hermes 活跃查询失败: {error}"))?;
            let mut rows = statement
                .query(rusqlite::params![now_ms - HERMES_ACTIVE_WINDOW_MS])
                .map_err(|error| format!("执行 Hermes 活跃查询失败: {error}"))?;
            while let Some(row) = rows
                .next()
                .map_err(|error| format!("读取 Hermes 活跃行失败: {error}"))?
            {
                let id: String = row.get(0).unwrap_or_default();
                let started_at: f64 = row.get(1).unwrap_or_default();
                if started_at <= 0.0 {
                    continue;
                }
                let previous = self.hermes_active_marks.get(&id).copied();
                if previous.is_some_and(|last| now_ms - last < STATUS_HEARTBEAT_MS) {
                    continue;
                }
                self.hermes_active_marks.insert(id.clone(), now_ms);
                statuses.push(StatusEvent {
                    tool: ToolId::HERMES,
                    task_id: task_id_for_text(&id),
                    at_ms: now_ms,
                    status: WorkStatus::Working,
                });
            }
        }

        Ok((events, statuses))
    }

}

fn open_readonly(path: &Path) -> Result<Connection, String> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("打开数据库失败 path={} error={error}", path.display()))?;
    let _ = connection.busy_timeout(std::time::Duration::from_millis(250));
    Ok(connection)
}

/// VS Code 状态库可能使用 WAL；主文件未变时也要感知 -wal/-shm 的更新
fn state_file_fingerprint(path: &Path) -> Option<(u64, u64)> {
    let metadata = fs::metadata(path).ok()?;
    let mut modified = metadata
        .modified()
        .ok()
        .and_then(system_time_to_ms)
        .unwrap_or_default();
    let mut length = metadata.len();
    for suffix in ["-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let Ok(sidecar) = fs::metadata(PathBuf::from(name)) else {
            continue;
        };
        let sidecar_modified = sidecar
            .modified()
            .ok()
            .and_then(system_time_to_ms)
            .unwrap_or_default();
        modified = modified.max(sidecar_modified);
        length = length.saturating_add(sidecar.len());
    }
    Some((modified, length))
}

fn system_time_to_ms(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

fn completion_tokens(token_info: &str) -> u64 {
    serde_json::from_str::<Value>(token_info)
        .ok()
        .and_then(|value| {
            value
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .or_else(|| value.get("output_tokens").and_then(Value::as_u64))
        })
        .unwrap_or_default()
}

/// Trae 的 memento JSON 只用于读取会话状态，不读取消息内容
fn trae_status_from_storage(text: &str, cutoff_ms: i64) -> Option<(WorkStatus, i64)> {
    let value: Value = serde_json::from_str(text).ok()?;
    let sessions = value.get("list")?.as_array()?;
    let mut best: Option<(WorkStatus, i64)> = None;

    for session in sessions {
        if session.get("isCurrent").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let Some(updated_at) = session.get("updatedAt").and_then(Value::as_i64) else {
            continue;
        };
        if updated_at < cutoff_ms {
            continue;
        }
        let Some(messages) = session.get("messages").and_then(Value::as_array) else {
            continue;
        };
        let status = messages
            .iter()
            .rev()
            .find_map(|message| message.get("status").and_then(Value::as_str));
        let Some(status) = status.and_then(trae_work_status) else {
            continue;
        };
        let candidate = (status, updated_at);
        best = Some(match best {
            Some(current) if current.1 >= candidate.1 => current,
            _ => candidate,
        });
    }

    best
}

fn trae_work_status(status: &str) -> Option<WorkStatus> {
    match status.to_ascii_lowercase().as_str() {
        "need_confirm" | "waiting_confirm" => Some(WorkStatus::NeedsConfirmation),
        "in_progress" | "working" | "running" | "generating" | "streaming" => {
            Some(WorkStatus::Working)
        }
        "completed" | "success" | "failed" | "error" | "canceled" | "cancelled"
        | "exited" | "skipped" => Some(WorkStatus::Stopped),
        _ => None,
    }
}

/// Trae / Trae CN 的 VS Code 风格用户目录；状态库位于 User 下的 globalStorage 与 workspaceStorage
fn trae_user_roots(home: &Path, app_name: &str) -> Vec<PathBuf> {
    data_bases(home)
        .into_iter()
        .map(|base| base.join(app_name).join("User"))
        .filter(|path| path.is_dir())
        .collect()
}

/// 各平台的应用数据根目录
fn data_bases(home: &Path) -> Vec<PathBuf> {
    let mut bases: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    bases.push(home.join("Library").join("Application Support"));
    #[cfg(target_os = "windows")]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        bases.push(PathBuf::from(appdata));
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    bases.push(home.join(".config"));
    bases
}

/// Qoder / Qoder CN 新版桌面端的活跃轮次缓冲库
fn active_turn_db_paths(home: &Path) -> Vec<(ToolId, PathBuf)> {
    let mut paths = Vec::new();
    for base in data_bases(home) {
        for (app, tool) in [
            ("com.qoder.app.stable", ToolId::QODER),
            ("com.qodercn.app.stable", ToolId::QODER_CN),
        ] {
            let path = base
                .join(app)
                .join("chat-session-turn-payload-buffer.sqlite");
            if path.is_file() {
                paths.push((tool, path));
            }
        }
    }
    paths
}

fn trae_state_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in roots {
        let global = root.join("globalStorage").join("state.vscdb");
        if global.is_file() {
            files.push(global);
        }
        let Ok(workspaces) = fs::read_dir(root.join("workspaceStorage")) else {
            continue;
        };
        for workspace in workspaces.flatten() {
            let candidate = workspace.path().join("state.vscdb");
            if candidate.is_file() {
                files.push(candidate);
            }
        }
    }
    files
}

fn default_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Qoder 桌面版旧版 token 库；按平台依次探测，取第一个存在的数据库。
/// 国内版新桌面端没有输出 token 字段，因此不接入旧库，只使用活跃轮次状态。
fn qoder_candidate_paths(home: &Path) -> Vec<PathBuf> {
    data_bases(home)
        .into_iter()
        .map(|base| {
            base.join("Qoder")
                .join("SharedClientCache")
                .join("cache")
                .join("db")
                .join("local.db")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use serde_json::json;

    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn temp_db(name: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "token-tracker-sqlite-{}-{}-{}.db",
            std::process::id(),
            name,
            unique
        ))
    }

    #[test]
    fn completion_tokens_supports_both_field_names() {
        assert_eq!(
            completion_tokens(r#"{"completion_tokens":1227}"#),
            1_227
        );
        assert_eq!(completion_tokens(r#"{"output_tokens":42}"#), 42);
        assert_eq!(completion_tokens("not-json"), 0);
    }

    #[test]
    fn qoder_assistant_messages_become_incremental_events() {
        let path = temp_db("qoder");
        let connection = Connection::open(&path).expect("测试数据库应可创建");
        connection
            .execute_batch(
                "CREATE TABLE chat_message (
                     id TEXT PRIMARY KEY,
                     session_id TEXT,
                     request_id TEXT,
                     role TEXT,
                     token_info TEXT,
                     gmt_create INTEGER
                 );
                 CREATE TABLE chat_record (
                     request_id TEXT PRIMARY KEY,
                     gmt_create INTEGER,
                     gmt_modified INTEGER
                 );",
            )
            .expect("测试表应可创建");
        connection
            .execute(
                "INSERT INTO chat_record (request_id, gmt_create, gmt_modified)
                 VALUES ('req-1', 1_789_900_000_000, 1_789_900_056_000)",
                [],
            )
            .expect("请求记录应可写入");
        for (id, created_at, tokens) in [
            ("msg-1", 1_789_900_010_000i64, 110u64),
            ("msg-2", 1_789_900_056_000i64, 2_427u64),
        ] {
            connection
                .execute(
                    "INSERT INTO chat_message
                     (id, session_id, request_id, role, token_info, gmt_create)
                     VALUES (?1, 'session-q', 'req-1', 'assistant', ?2, ?3)",
                    params![
                        id,
                        json!({
                            "prompt_tokens": 105_012,
                            "completion_tokens": tokens,
                            "cached_tokens": 104_540
                        })
                        .to_string(),
                        created_at
                    ],
                )
                .expect("assistant 消息应可写入");
        }
        drop(connection);

        let mut collector = SqliteCollector::empty();
        let (events, statuses) = collector.poll_qoder(&path, 0).expect("Qoder 查询应成功");
        assert!(statuses.is_empty());
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].output_tokens, 110);
        assert_eq!(events[0].duration_ms, 10_000);
        assert_eq!(events[1].output_tokens, 2_427);
        assert_eq!(events[1].duration_ms, 46_000);
        assert!(events.iter().all(|event| event.tool == ToolId::QODER));

        let (again, _) = collector.poll_qoder(&path, 0).expect("重复查询应成功");
        assert!(again.is_empty(), "rowid 水位线应避免重复计数");

        fs::remove_file(&path).expect("测试数据库应可删除");
    }

    #[test]
    fn qoder_active_turns_emit_working_and_stopped() {
        let path = temp_db("qoder-active");
        let connection = Connection::open(&path).expect("测试数据库应可创建");
        connection
            .execute_batch(
                "CREATE TABLE active_turn_projection_items (
                     session_id TEXT NOT NULL,
                     turn_id TEXT NOT NULL,
                     item_id TEXT NOT NULL,
                     created_at INTEGER NOT NULL
                 );",
            )
            .expect("测试表应可创建");
        let now = 1_789_900_100_000i64;
        for (item, created_at) in [("item-1", now - 4_000), ("item-2", now - 1_000)] {
            connection
                .execute(
                    "INSERT INTO active_turn_projection_items
                     (session_id, turn_id, item_id, created_at)
                     VALUES ('session-q', 'turn-q', ?1, ?2)",
                    params![item, created_at],
                )
                .expect("活跃条目应可写入");
        }
        drop(connection);

        let mut tracker = ActiveTurnTracker::new(ToolId::QODER_CN, path.clone());
        let working = tracker.poll(now, 0);
        assert_eq!(working.len(), 1);
        assert_eq!(working[0].tool, ToolId::QODER_CN);
        assert_eq!(working[0].status, WorkStatus::Working);
        assert_eq!(working[0].at_ms, now);
        assert_eq!(working[0].task_id, task_id_for_text("turn-q"));

        let connection = Connection::open(&path).expect("测试数据库应可再次打开");
        connection
            .execute("DELETE FROM active_turn_projection_items", [])
            .expect("活跃条目应可删除");
        drop(connection);

        let stopped = tracker.poll(now + 5_000, 0);
        assert_eq!(stopped.len(), 1);
        assert_eq!(stopped[0].status, WorkStatus::Stopped);
        assert_eq!(stopped[0].task_id, task_id_for_text("turn-q"));

        fs::remove_file(&path).expect("测试数据库应可删除");
    }

    #[test]
    fn qoder_stale_active_turn_is_ignored() {
        let path = temp_db("qoder-stale");
        let connection = Connection::open(&path).expect("测试数据库应可创建");
        connection
            .execute_batch(
                "CREATE TABLE active_turn_projection_items (
                     session_id TEXT NOT NULL,
                     turn_id TEXT NOT NULL,
                     item_id TEXT NOT NULL,
                     created_at INTEGER NOT NULL
                 );
                 INSERT INTO active_turn_projection_items
                 VALUES ('session-q', 'turn-old', 'item-1', 1000);",
            )
            .expect("测试数据应可写入");
        drop(connection);

        let mut tracker = ActiveTurnTracker::new(ToolId::QODER, path.clone());
        assert!(tracker.poll(ACTIVE_TURN_WINDOW_MS + 2_000, 0).is_empty());
        assert!(
            tracker.poll(ACTIVE_TURN_WINDOW_MS + 7_000, 0).is_empty(),
            "启动时忽略的残留轮次不应在后续轮询中复活"
        );

        fs::remove_file(&path).expect("测试数据库应可删除");
    }

    #[test]
    fn qoder_long_running_turn_stays_working_after_window() {
        let path = temp_db("qoder-long-turn");
        let connection = Connection::open(&path).expect("测试数据库应可创建");
        connection
            .execute_batch(
                "CREATE TABLE active_turn_projection_items (
                     session_id TEXT NOT NULL,
                     turn_id TEXT NOT NULL,
                     item_id TEXT NOT NULL,
                     created_at INTEGER NOT NULL
                 );",
            )
            .expect("测试表应可创建");
        let now = 1_789_900_100_000i64;
        connection
            .execute(
                "INSERT INTO active_turn_projection_items
                 VALUES ('session-q', 'turn-long', 'item-1', ?1)",
                params![now - 4 * 60_000],
            )
            .expect("活跃条目应可写入");
        drop(connection);

        let mut tracker = ActiveTurnTracker::new(ToolId::QODER_CN, path.clone());
        let working = tracker.poll(now, 0);
        assert_eq!(working.len(), 1);
        assert_eq!(working[0].status, WorkStatus::Working);

        let still_working = tracker.poll(now + 10 * 60_000, 0);
        assert_eq!(still_working.len(), 1);
        assert_eq!(still_working[0].status, WorkStatus::Working);
        assert_eq!(still_working[0].at_ms, now + 10 * 60_000);

        fs::remove_file(&path).expect("测试数据库应可删除");
    }

    #[test]
    fn state_file_fingerprint_tracks_wal_sidecar() {
        let dir = std::env::temp_dir().join(format!(
            "token-tracker-fingerprint-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("测试目录应可创建");
        let path = dir.join("state.vscdb");
        fs::write(&path, b"main").expect("主文件应可写入");
        let before = state_file_fingerprint(&path).expect("应可读取指纹");

        fs::write(dir.join("state.vscdb-wal"), b"wal-update").expect("WAL 文件应可写入");
        let after = state_file_fingerprint(&path).expect("应可读取指纹");
        assert_ne!(before, after, "WAL 更新应改变状态库指纹");

        fs::remove_dir_all(&dir).expect("测试目录应可删除");
    }

    #[test]
    fn trae_current_session_status_is_recognized() {
        let storage = json!({
            "list": [{
                "isCurrent": true,
                "updatedAt": 1_789_900_000_000i64,
                "messages": [{"status": "completed"}, {"status": "need_confirm"}]
            }, {
                "isCurrent": false,
                "updatedAt": 1_789_900_100_000i64,
                "messages": [{"status": "in_progress"}]
            }]
        })
        .to_string();

        let (status, at_ms) =
            trae_status_from_storage(&storage, 0).expect("应识别当前会话状态");
        assert_eq!(status, WorkStatus::NeedsConfirmation);
        assert_eq!(at_ms, 1_789_900_000_000);
    }

    #[test]
    fn trae_stale_session_is_ignored() {
        let storage = json!({
            "list": [{
                "isCurrent": true,
                "updatedAt": 1_000,
                "messages": [{"status": "in_progress"}]
            }]
        })
        .to_string();
        assert!(trae_status_from_storage(&storage, 2_000).is_none());
    }
}
