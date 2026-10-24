/**
 * @Author: Danson zheng
 * @Date: 2026-10-23
 * @Description: 多智能体会话日志采集层。发现 Claude Code / Codex CLI / ZCode 的 JSONL
 * 会话文件，增量读取新增行，按本地近 30 个自然日回填，并把解析结果送入实时指标引擎。
 * 只提取时间、token、耗时与会话标识，不读取或记录会话正文。
 */

mod claude;
mod codex;
mod jsonl;
mod zcode;

use crate::live::{LiveMetricsEngine, UsageEvent, CHART_WINDOW_MS, THIRTY_DAY_SPAN};
use crate::tools::{AdapterKind, ToolId};
use chrono::{DateTime, Days, FixedOffset, Local, TimeZone, Utc};
use claude::ClaudeParser;
use codex::CodexParser;
use jsonl::JsonlTailer;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zcode::ZcodeParser;

/// 单轮轮询最多打印的解析失败明细，避免坏文件刷屏
const MAX_LOGGED_FAILURES: usize = 5;

#[derive(Debug)]
struct RootSpec {
    path: PathBuf,
    tool: ToolId,
}

#[derive(Debug)]
struct DiscoveredFile {
    path: PathBuf,
    tool: ToolId,
}

#[derive(Debug)]
enum FileParser {
    Claude(ClaudeParser),
    Codex(CodexParser),
    Zcode(ZcodeParser),
}

impl FileParser {
    /// 暂无可靠数据源的适配器返回 None，采集层跳过对应文件
    fn new(tool: ToolId) -> Option<Self> {
        match tool.adapter() {
            AdapterKind::ClaudeCode => Some(Self::Claude(ClaudeParser::new())),
            AdapterKind::CodexCli => Some(Self::Codex(CodexParser::new())),
            AdapterKind::Zcode => Some(Self::Zcode(ZcodeParser::new())),
            AdapterKind::Unsupported => None,
        }
    }

    fn parse_line(
        &mut self,
        line: &str,
        emit_after_ms: i64,
    ) -> Result<Option<UsageEvent>, String> {
        match self {
            Self::Claude(parser) => parser.parse_line(line, emit_after_ms),
            Self::Codex(parser) => parser.parse_line(line, emit_after_ms),
            Self::Zcode(parser) => parser.parse_line(line, emit_after_ms),
        }
    }
}

struct TrackedFile {
    tool: ToolId,
    /// 由会话文件路径派生的稳定任务标识，用于统计并发任务数
    task_id: u64,
    tailer: JsonlTailer,
    parser: FileParser,
}

/// 日志采集器；需要由外层按固定间隔调用 poll
pub struct LogCollector {
    roots: Vec<RootSpec>,
    offset: FixedOffset,
    tracked: HashMap<PathBuf, TrackedFile>,
}

impl Default for LogCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl LogCollector {
    /// 默认监听 Claude Code、Codex CLI、ZCode 的会话目录，时区跟随系统本地
    pub fn new() -> Self {
        let mut roots = Vec::new();
        match default_home_dir() {
            Some(home) => {
                roots.push(RootSpec {
                    path: home.join(".claude").join("projects"),
                    tool: ToolId::CLAUDE_CODE,
                });
                roots.push(RootSpec {
                    path: home.join(".codex").join("sessions"),
                    tool: ToolId::CODEX_CLI,
                });
                roots.push(RootSpec {
                    path: home.join(".zcode").join("cli").join("rollout"),
                    tool: ToolId::ZCODE,
                });
            }
            None => {
                log::warn!("【日志采集】无法定位用户主目录，自动采集已禁用");
            }
        }

        Self::with_specs(roots, *Local::now().offset())
    }

    /// 显式指定日志根目录与时区，供测试或自定义部署使用
    pub fn with_roots(
        claude_projects: PathBuf,
        codex_sessions: PathBuf,
        zcode_rollout: PathBuf,
        offset: FixedOffset,
    ) -> Self {
        Self::with_specs(
            vec![
                RootSpec {
                    path: claude_projects,
                    tool: ToolId::CLAUDE_CODE,
                },
                RootSpec {
                    path: codex_sessions,
                    tool: ToolId::CODEX_CLI,
                },
                RootSpec {
                    path: zcode_rollout,
                    tool: ToolId::ZCODE,
                },
            ],
            offset,
        )
    }

    fn with_specs(roots: Vec<RootSpec>, offset: FixedOffset) -> Self {
        let collector = Self {
            roots,
            offset,
            tracked: HashMap::new(),
        };
        for root in &collector.roots {
            if !root.path.is_dir() {
                log::info!(
                    "【日志采集】日志目录不存在，等待创建 path={}",
                    root.path.display()
                );
            }
        }
        collector
    }

    /// 采集本轮新增事件；回填范围覆盖本地近 30 个自然日
    pub fn poll(&mut self, now_ms: i64) -> Vec<UsageEvent> {
        let cutoff_ms = self.backfill_cutoff_ms(now_ms);
        self.poll_after(cutoff_ms)
    }

    /// 采集并直接写入实时引擎，返回本轮新增事件数
    pub fn poll_into(&mut self, engine: &mut LiveMetricsEngine, now_ms: i64) -> usize {
        let events = self.poll(now_ms);
        let count = events.len();
        engine.ingest_all(events);
        count
    }

    /// 30 天前的本地零点；时间戳异常时退化为最近 30 分钟，保证图表仍可工作
    fn backfill_cutoff_ms(&self, now_ms: i64) -> i64 {
        let chart_start_ms = now_ms.saturating_sub(CHART_WINDOW_MS);
        let Some(now) = DateTime::<Utc>::from_timestamp_millis(now_ms) else {
            return chart_start_ms;
        };
        let local_today = now.with_timezone(&self.offset).date_naive();
        let Some(start_date) =
            local_today.checked_sub_days(Days::new(THIRTY_DAY_SPAN.saturating_sub(1)))
        else {
            return chart_start_ms;
        };
        let Some(midnight) = start_date.and_hms_opt(0, 0, 0) else {
            return chart_start_ms;
        };

        self.offset
            .from_local_datetime(&midnight)
            .earliest()
            .map(|start| start.timestamp_millis().min(chart_start_ms))
            .unwrap_or(chart_start_ms)
    }

    fn poll_after(&mut self, cutoff_ms: i64) -> Vec<UsageEvent> {
        self.discover(cutoff_ms);

        let paths: Vec<PathBuf> = self.tracked.keys().cloned().collect();
        let mut events = Vec::new();
        let mut line_count = 0usize;
        let mut failure_count = 0usize;
        let mut failures = Vec::new();

        for path in paths {
            let read_result = match self.tracked.get_mut(&path) {
                Some(file) => file.tailer.read_new_lines(&path),
                None => continue,
            };

            let batch = match read_result {
                Ok(batch) => batch,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.tracked.remove(&path);
                    log::info!(
                        "【日志采集】会话文件已删除，停止跟踪 file={}",
                        path.display()
                    );
                    continue;
                }
                Err(error) => {
                    log::warn!(
                        "【日志采集】读取会话文件失败 file={} error={}",
                        path.display(),
                        error
                    );
                    continue;
                }
            };

            let Some(tracked) = self.tracked.get_mut(&path) else {
                continue;
            };
            if batch.truncated {
                if let Some(parser) = FileParser::new(tracked.tool) {
                    tracked.parser = parser;
                }
                log::warn!(
                    "【日志采集】会话文件长度回退，已从头重读 file={}",
                    path.display()
                );
            }

            for (line_no, line) in batch.lines {
                line_count += 1;
                match tracked.parser.parse_line(&line, cutoff_ms) {
                    Ok(Some(mut event)) => {
                        event.task_id = tracked.task_id;
                        events.push(event);
                    }
                    Ok(None) => {}
                    Err(reason) => {
                        failure_count += 1;
                        if failures.len() < MAX_LOGGED_FAILURES {
                            failures.push((path.clone(), line_no, reason));
                        }
                    }
                }
            }
        }

        for (path, line_no, reason) in &failures {
            log::warn!(
                "【日志采集】会话行解析失败 file={} line={} reason={}",
                path.display(),
                line_no,
                reason
            );
        }
        if failure_count > failures.len() {
            log::warn!(
                "【日志采集】其余解析失败明细已省略 count={}",
                failure_count - failures.len()
            );
        }

        if !events.is_empty() {
            log::info!(
                "【日志采集】本轮采集完成 events={} lines={} tracked_files={}",
                events.len(),
                line_count,
                self.tracked.len()
            );
        }

        events
    }

    fn discover(&mut self, cutoff_ms: i64) {
        let mut discovered = Vec::new();
        for root in &self.roots {
            collect_jsonl_files(&root.path, root.tool, &mut discovered);
        }

        for file in discovered {
            if self.tracked.contains_key(&file.path) {
                continue;
            }
            let Some(parser) = FileParser::new(file.tool) else {
                continue;
            };
            let Ok(metadata) = fs::metadata(&file.path) else {
                continue;
            };
            let modified_ms = metadata
                .modified()
                .ok()
                .and_then(system_time_to_ms)
                .unwrap_or_default();
            if modified_ms < cutoff_ms {
                continue;
            }

            self.tracked.insert(
                file.path.clone(),
                TrackedFile {
                    tool: file.tool,
                    task_id: task_id_for(&file.path),
                    tailer: JsonlTailer::new(),
                    parser,
                },
            );
            log::info!(
                "【日志采集】发现会话文件 tool={} file={}",
                file.tool.display_name(),
                file.path.display()
            );
        }
    }
}

/// 由绝对路径派生任务标识；同一会话文件在进程内始终对应同一任务
fn task_id_for(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish().max(1)
}

pub(crate) fn parse_timestamp_ms(value: &str) -> Result<i64, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.timestamp_millis())
        .map_err(|_| format!("时间戳格式非法: {value}"))
}

fn default_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn collect_jsonl_files(root: &Path, tool: ToolId, out: &mut Vec<DiscoveredFile>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_jsonl_files(&path, tool, out);
        } else if file_type.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
        {
            out.push(DiscoveredFile { path, tool });
        }
    }
}

fn system_time_to_ms(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_root(name: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "token-tracker-collector-{}-{}-{}",
            std::process::id(),
            name,
            unique
        ));
        fs::create_dir_all(&path).expect("测试目录应可创建");
        path
    }

    fn fixed_offset() -> FixedOffset {
        FixedOffset::east_opt(8 * 3600).expect("东八区偏移应合法")
    }

    fn collector_at(root: &Path) -> LogCollector {
        LogCollector::with_roots(
            root.join("claude"),
            root.join("codex"),
            root.join("zcode"),
            fixed_offset(),
        )
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("测试目录应可创建");
        }
        fs::write(path, content).expect("测试文件应可写入");
    }

    fn append_file(path: &Path, content: &str) {
        let mut file = OpenOptions::new()
            .append(true)
            .open(path)
            .expect("测试文件应可追加");
        file.write_all(content.as_bytes())
            .expect("测试内容应追加成功");
    }

    const CLAUDE_FIXTURE: &str = concat!(
        "{\"type\":\"user\",\"timestamp\":\"2026-10-23T02:00:00.000Z\"}\n",
        "{\"type\":\"api-request\",\"id\":\"req-1\",\"timestamp\":\"2026-10-23T02:00:01.000Z\"}\n",
        "{\"type\":\"assistant\",\"timestamp\":\"2026-10-23T02:00:06.000Z\",\"requestRef\":\"req-1\",\"message\":{\"id\":\"msg-1\",\"usage\":{\"output_tokens\":100}}}\n",
    );

    const CODEX_FIXTURE: &str = concat!(
        "{\"timestamp\":\"2026-10-23T02:00:00.000Z\",\"payload\":{\"type\":\"task_started\"}}\n",
        "{\"timestamp\":\"2026-10-23T02:00:10.000Z\",\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{\"output_tokens\":200}}}}\n",
    );

    const ZCODE_FIXTURE: &str = concat!(
        "{\"type\":\"model_io\",\"sessionId\":\"sess-z\",\"requestId\":\"zreq-1\",\"startedAt\":\"2026-10-23T02:00:00.000Z\",\"completedAt\":\"2026-10-23T02:00:12.000Z\",\"durationMs\":10000,\"response\":{\"usage\":{\"outputTokens\":300}}}\n",
    );

    #[test]
    fn collects_events_from_supported_tools() {
        let root = temp_root("all");
        let claude_file = root.join("claude").join("project").join("session.jsonl");
        let codex_file = root
            .join("codex")
            .join("2026")
            .join("10")
            .join("23")
            .join("rollout.jsonl");
        let zcode_file = root.join("zcode").join("model-io-sess-z.jsonl");
        write_file(&claude_file, CLAUDE_FIXTURE);
        write_file(&codex_file, CODEX_FIXTURE);
        write_file(&zcode_file, ZCODE_FIXTURE);

        let mut collector = collector_at(&root);
        let events = collector.poll_after(0);
        assert_eq!(events.len(), 3);

        let claude = events
            .iter()
            .find(|event| event.tool == ToolId::CLAUDE_CODE)
            .expect("应采集到 Claude 事件");
        assert_eq!(claude.output_tokens, 100);
        assert_eq!(claude.duration_ms, 5_000);

        let codex = events
            .iter()
            .find(|event| event.tool == ToolId::CODEX_CLI)
            .expect("应采集到 Codex 事件");
        assert_eq!(codex.output_tokens, 200);
        assert_eq!(codex.duration_ms, 10_000);

        let zcode = events
            .iter()
            .find(|event| event.tool == ToolId::ZCODE)
            .expect("应采集到 ZCode 事件");
        assert_eq!(zcode.output_tokens, 300);
        assert_eq!(zcode.duration_ms, 10_000);

        let task_ids: std::collections::HashSet<u64> =
            events.iter().map(|event| event.task_id).collect();
        assert_eq!(task_ids.len(), 3, "不同会话文件应具有不同任务标识");

        fs::remove_dir_all(&root).expect("测试目录应可删除");
    }

    #[test]
    fn incremental_poll_does_not_duplicate_and_waits_for_newline() {
        let root = temp_root("incremental");
        let claude_file = root.join("claude").join("project").join("session.jsonl");
        write_file(&claude_file, CLAUDE_FIXTURE);

        let mut collector = collector_at(&root);
        assert_eq!(collector.poll_after(0).len(), 1);
        assert!(collector.poll_after(0).is_empty());

        let next_user = "{\"type\":\"user\",\"timestamp\":\"2026-10-23T03:00:00.000Z\"}\n";
        let next_request = "{\"type\":\"api-request\",\"id\":\"req-2\",\"timestamp\":\"2026-10-23T03:00:01.000Z\"}\n";
        let next_assistant = "{\"type\":\"assistant\",\"timestamp\":\"2026-10-23T03:00:04.000Z\",\"requestRef\":\"req-2\",\"message\":{\"id\":\"msg-2\",\"usage\":{\"output_tokens\":60}}}";
        append_file(&claude_file, next_user);
        append_file(&claude_file, next_request);
        append_file(&claude_file, next_assistant);

        assert!(collector.poll_after(0).is_empty());

        append_file(&claude_file, "\n");
        let events = collector.poll_after(0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].output_tokens, 60);
        assert_eq!(events[0].duration_ms, 3_000);

        fs::remove_dir_all(&root).expect("测试目录应可删除");
    }

    #[test]
    fn backfill_cutoff_covers_thirty_natural_days() {
        let root = temp_root("cutoff");
        let collector = collector_at(&root);

        let noon = fixed_offset()
            .with_ymd_and_hms(2026, 10, 23, 12, 0, 0)
            .single()
            .expect("正午时间应唯一")
            .timestamp_millis();
        let start = fixed_offset()
            .with_ymd_and_hms(2026, 9, 24, 0, 0, 0)
            .single()
            .expect("窗口起点应唯一")
            .timestamp_millis();
        assert_eq!(collector.backfill_cutoff_ms(noon), start);

        let after_midnight = fixed_offset()
            .with_ymd_and_hms(2026, 10, 23, 0, 10, 0)
            .single()
            .expect("凌晨时间应唯一")
            .timestamp_millis();
        assert_eq!(collector.backfill_cutoff_ms(after_midnight), start);

        fs::remove_dir_all(&root).expect("测试目录应可删除");
    }
}
