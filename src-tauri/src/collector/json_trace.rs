/**
 * @Author: Danson zheng
 * @Date: 2026-10-24
 * @Description: WorkBuddy / CodeBuddy trace 采集器。WorkBuddy 增量读取
 * traces/<pid>/trace_*.jsonl，CodeBuddy 按文件指纹解析完整 trace_*.json 文档；
 * 两种格式共享同一个解析器，按 traceId 与 spanId 去重，只提取 token、耗时与状态。
 */

use super::jsonl::JsonlTailer;
use super::trace::TraceParser;
use super::{task_id_for, CollectBatch, MAX_LOGGED_FAILURES};
use crate::tools::ToolId;
use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct JsonTraceCollector {
    tool: ToolId,
    root: PathBuf,
    parser: TraceParser,
    /// CodeBuddy 完整文档按 (修改时间, 长度) 增量解析
    fingerprints: HashMap<PathBuf, (u64, u64)>,
    /// WorkBuddy JSONL 按字节偏移增量读取
    tailers: HashMap<PathBuf, JsonlTailer>,
}

impl JsonTraceCollector {
    pub fn new(tool: ToolId, root: PathBuf) -> Self {
        Self {
            tool,
            root,
            parser: TraceParser::new(tool),
            fingerprints: HashMap::new(),
            tailers: HashMap::new(),
        }
    }

    pub fn poll(&mut self, cutoff_ms: i64) -> CollectBatch {
        let mut batch = CollectBatch::default();
        let mut files = Vec::new();
        collect_trace_files(&self.root, &mut files);

        for path in files {
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            let modified_ms = metadata
                .modified()
                .ok()
                .and_then(system_time_to_ms)
                .unwrap_or_default();
            let task_id = task_id_for(&path);

            if is_jsonl(&path) {
                self.poll_jsonl(&path, modified_ms, cutoff_ms, task_id, &mut batch);
            } else {
                self.poll_document(
                    &path,
                    modified_ms,
                    metadata.len(),
                    cutoff_ms,
                    task_id,
                    &mut batch,
                );
            }
        }

        batch
    }

    fn poll_jsonl(
        &mut self,
        path: &Path,
        modified_ms: i64,
        cutoff_ms: i64,
        task_id: u64,
        batch: &mut CollectBatch,
    ) {
        if !self.tailers.contains_key(path) {
            if modified_ms < cutoff_ms {
                return;
            }
            self.tailers.insert(path.to_path_buf(), JsonlTailer::new());
        }
        let Some(tailer) = self.tailers.get_mut(path) else {
            return;
        };
        let read_result = tailer.read_new_lines(path);
        let lines = match read_result {
            Ok(read) => read.lines,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                self.tailers.remove(path);
                return;
            }
            Err(error) => {
                log::warn!(
                    "【日志采集】读取 trace 文件失败 tool={} file={} error={}",
                    self.tool.display_name(),
                    path.display(),
                    error
                );
                return;
            }
        };

        let mut failures = 0usize;
        for (line_no, line) in lines {
            match self.parser.parse_line(&line, cutoff_ms) {
                Ok(result) => push_result(batch, result, task_id),
                Err(reason) => {
                    failures += 1;
                    if failures <= MAX_LOGGED_FAILURES {
                        log::warn!(
                            "【日志采集】trace 行解析失败 tool={} file={} line={} reason={}",
                            self.tool.display_name(),
                            path.display(),
                            line_no,
                            reason
                        );
                    }
                }
            }
        }
    }

    fn poll_document(
        &mut self,
        path: &Path,
        modified_ms: i64,
        file_len: u64,
        cutoff_ms: i64,
        task_id: u64,
        batch: &mut CollectBatch,
    ) {
        if modified_ms < cutoff_ms {
            return;
        }
        let fingerprint = (modified_ms as u64, file_len);
        if self.fingerprints.get(path) == Some(&fingerprint) {
            return;
        }
        self.fingerprints.insert(path.to_path_buf(), fingerprint);

        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        match self.parser.parse_document(&text, cutoff_ms) {
            Ok(result) => push_result(batch, result, task_id),
            Err(reason) => log::warn!(
                "【日志采集】trace 文档解析失败 tool={} file={} reason={}",
                self.tool.display_name(),
                path.display(),
                reason
            ),
        }
    }
}

fn push_result(batch: &mut CollectBatch, result: super::ParseResult, task_id: u64) {
    if let Some(mut event) = result.event {
        event.task_id = task_id;
        batch.events.push(event);
    }
    if let Some(mut status) = result.status {
        status.task_id = task_id;
        batch.statuses.push(status);
    }
}

fn is_jsonl(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
}

fn collect_trace_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_trace_files(&path, out);
        } else if file_type.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "json" || extension == "jsonl")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("trace_"))
        {
            out.push(path);
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
    use std::io::Write;

    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn temp_root(name: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "token-tracker-trace-{}-{}-{}",
            std::process::id(),
            name,
            unique
        ));
        fs::create_dir_all(&path).expect("测试目录应可创建");
        path
    }

    fn append(path: &Path, content: &str) {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("测试文件应可追加");
        file.write_all(content.as_bytes())
            .expect("测试内容应追加成功");
    }

    #[test]
    fn workbuddy_jsonl_is_tailed_and_counted_once() {
        let root = temp_root("workbuddy");
        let path = root.join("123").join("trace_demo.jsonl");
        fs::create_dir_all(path.parent().expect("应有父目录")).expect("目录应可创建");
        append(
            &path,
            concat!(
                "{\"type\":\"trace_start\",\"trace\":{\"traceId\":\"t1\",\"startedAt\":\"2026-10-23T02:00:00.000Z\",\"status\":\"running\"}}\n",
                "{\"type\":\"span_end\",\"span\":{\"traceId\":\"t1\",\"spanId\":\"s1\",\"name\":\"generation\",\"type\":\"generation\",\"startedAt\":\"2026-10-23T02:00:01.000Z\",\"endedAt\":\"2026-10-23T02:00:05.000Z\",\"duration\":4000}}\n",
            ),
        );

        let mut collector = JsonTraceCollector::new(ToolId::WORK_BUDDY, root.clone());
        let first = collector.poll(0);
        assert!(first.events.is_empty());
        assert_eq!(
            first.statuses.first().expect("应上报工作状态").status,
            crate::live::WorkStatus::Working
        );

        append(
            &path,
            "{\"type\":\"trace_end\",\"trace\":{\"traceId\":\"t1\",\"startedAt\":\"2026-10-23T02:00:00.000Z\",\"endedAt\":\"2026-10-23T02:00:10.000Z\",\"status\":\"ok\",\"totalTokens\":999,\"modelInfo\":{\"totalOutputTokens\":600}}}\n",
        );
        let second = collector.poll(0);
        let event = second.events.first().expect("应产生 token 事件");
        assert_eq!(event.output_tokens, 600);
        assert_eq!(event.duration_ms, 4_000);
        assert_eq!(event.task_id, task_id_for(&path));
        assert!(collector.poll(0).events.is_empty(), "无新增内容时不应重复计数");

        fs::remove_dir_all(&root).expect("测试目录应可删除");
    }

    #[test]
    fn codebuddy_document_uses_generation_span_duration() {
        let root = temp_root("codebuddy");
        let path = root.join("123").join("trace_doc.json");
        fs::create_dir_all(path.parent().expect("应有父目录")).expect("目录应可创建");
        fs::write(
            &path,
            concat!(
                "{\"trace\":{\"traceId\":\"t2\",\"startedAt\":\"2026-10-23T02:00:00.000Z\",",
                "\"endedAt\":\"2026-10-23T02:01:00.000Z\",\"duration\":60000,",
                "\"status\":\"ok\",\"modelInfo\":{\"totalOutputTokens\":300}},",
                "\"spans\":[{\"traceId\":\"t2\",\"spanId\":\"s2\",\"name\":\"generation\",",
                "\"type\":\"generation\",\"duration\":10000}]}"
            ),
        )
        .expect("测试文档应可写入");

        let mut collector = JsonTraceCollector::new(ToolId::CODE_BUDDY, root.clone());
        let batch = collector.poll(0);
        let event = batch.events.first().expect("应产生 token 事件");
        assert_eq!(event.output_tokens, 300);
        assert_eq!(event.duration_ms, 10_000);
        assert!(collector.poll(0).events.is_empty());

        fs::remove_dir_all(&root).expect("测试目录应可删除");
    }
}
