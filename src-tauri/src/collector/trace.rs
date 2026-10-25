/**
 * @Author: Danson zheng
 * @Date: 2026-10-24
 * @Description: WorkBuddy / CodeBuddy trace 解析器。WorkBuddy 使用逐行 JSONL
 * （trace_start / span_end / trace_end），CodeBuddy 使用完整 JSON 文档（trace + spans）。
 * 输出 token 取 modelInfo.totalOutputTokens（totalTokens 含输入与缓存，不能当作输出），
 * 生成耗时优先累加 generation span，避免把工具等待时间计入生成速度。
 */

use super::{parse_timestamp_ms, task_id_for_text, ParseResult};
use crate::live::{StatusEvent, UsageEvent, WorkStatus};
use crate::tools::ToolId;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// 工作中状态的最长有效期；持续活跃的 trace 定期刷新，避免前端显示过期状态
const WORKING_REFRESH_MS: i64 = 60_000;

#[derive(Debug)]
pub struct TraceParser {
    tool: ToolId,
    /// 已上报 token 的 traceId，保证终态只记录一次
    emitted_traces: HashSet<String>,
    /// traceId -> 最近一次上报的状态与时间
    status_by_trace: HashMap<String, (WorkStatus, i64)>,
    /// traceId -> generation span 耗时总和
    generation_ms: HashMap<String, u64>,
    /// 已累加的 generation spanId，重复读取同一文件时不会重复计时
    generation_spans: HashSet<String>,
    /// traceId -> 已按 generation span 上报的 token 总量；终态只补足差额
    span_usage_tokens: HashMap<String, u64>,
}

impl TraceParser {
    pub fn new(tool: ToolId) -> Self {
        Self {
            tool,
            emitted_traces: HashSet::new(),
            status_by_trace: HashMap::new(),
            generation_ms: HashMap::new(),
            generation_spans: HashSet::new(),
            span_usage_tokens: HashMap::new(),
        }
    }

    /// WorkBuddy 每行一条 trace/span 事件，只有 trace_start 与 trace_end 携带汇总
    pub fn parse_line(&mut self, line: &str, cutoff_ms: i64) -> Result<ParseResult, String> {
        let document: TraceLine =
            serde_json::from_str(line).map_err(|error| format!("JSON 解析失败: {error}"))?;
        let mut result = ParseResult::default();

        if let Some(span) = document.span {
            if document.kind.as_deref() == Some("span_end") {
                let is_new_span = self.record_generation_span(&span);
                self.refresh_working_status(&span, cutoff_ms, &mut result);
                if is_new_span && self.tool == ToolId::WORK_BUDDY {
                    self.emit_span_usage(&span, cutoff_ms, &mut result);
                }
            }
            return Ok(result);
        }

        let Some(trace) = document.trace else {
            return Ok(result);
        };
        match document.kind.as_deref() {
            Some("trace_start") | Some("trace_end") | Some("trace") => {}
            _ => return Ok(result),
        }
        self.parse_trace(trace, cutoff_ms, &mut result);
        Ok(result)
    }

    /// CodeBuddy 每个文件是一份完整 trace 文档，spans 用于还原真实生成耗时
    pub fn parse_document(&mut self, text: &str, cutoff_ms: i64) -> Result<ParseResult, String> {
        let document: TraceDocument =
            serde_json::from_str(text).map_err(|error| format!("JSON 解析失败: {error}"))?;
        for span in &document.spans {
            self.record_generation_span(span);
        }
        let mut result = ParseResult::default();
        if let Some(trace) = document.trace {
            self.parse_trace(trace, cutoff_ms, &mut result);
        }
        Ok(result)
    }

    fn record_generation_span(&mut self, span: &SpanRecord) -> bool {
        let is_generation = span.kind.as_deref() == Some("generation")
            || span.name.as_deref() == Some("generation");
        if !is_generation {
            return false;
        }
        let (Some(trace_id), Some(span_id)) = (span.trace_id.as_deref(), span.span_id.as_deref())
        else {
            return false;
        };
        if !self.generation_spans.insert(span_id.to_string()) {
            return false;
        }
        let duration = span
            .duration
            .filter(|duration| *duration > 0)
            .or_else(|| span_elapsed_ms(span))
            .unwrap_or_default();
        *self.generation_ms.entry(trace_id.to_string()).or_default() += duration;
        true
    }

    /// WorkBuddy 每次模型调用的 span 结束时就带有 usage，可直接形成实时速度事件；
    /// trace_end 的汇总只补足 span 未覆盖的差额，避免重复计数或漏计。
    fn emit_span_usage(
        &mut self,
        span: &SpanRecord,
        cutoff_ms: i64,
        result: &mut ParseResult,
    ) {
        let Some(trace_id) = span.trace_id.as_deref() else {
            return;
        };
        let output_tokens = span
            .tool_output
            .as_ref()
            .map(response_output_tokens)
            .unwrap_or_default();
        let Some(at_ms) = span_at_ms(span).filter(|at_ms| *at_ms >= cutoff_ms) else {
            return;
        };
        let duration_ms = span
            .duration
            .filter(|duration| *duration > 0)
            .or_else(|| span_elapsed_ms(span))
            .unwrap_or_default();
        if output_tokens == 0 || duration_ms == 0 {
            return;
        }

        *self
            .span_usage_tokens
            .entry(trace_id.to_string())
            .or_default() += output_tokens;
        result.event = Some(UsageEvent {
            tool: self.tool,
            task_id: task_id_for_text(trace_id),
            at_ms,
            output_tokens,
            duration_ms,
        });
    }

    /// span 结束时刷新工作中的时间戳，防止长任务因状态过期被判定为停止
    fn refresh_working_status(
        &mut self,
        span: &SpanRecord,
        cutoff_ms: i64,
        result: &mut ParseResult,
    ) {
        let (Some(trace_id), Some(at_ms)) = (span.trace_id.as_deref(), span_at_ms(span)) else {
            return;
        };
        if self.status_by_trace.get(trace_id).is_none()
            || !self.should_emit_status(trace_id, WorkStatus::Working, at_ms)
            || at_ms < cutoff_ms
        {
            return;
        }
        self.status_by_trace
            .insert(trace_id.to_string(), (WorkStatus::Working, at_ms));
        result.status = Some(StatusEvent {
            tool: self.tool,
            task_id: task_id_for_text(trace_id),
            at_ms,
            status: WorkStatus::Working,
        });
    }

    fn parse_trace(&mut self, trace: TraceRecord, cutoff_ms: i64, result: &mut ParseResult) {
        let Some(trace_id) = trace
            .trace_id
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(str::to_string)
        else {
            return;
        };
        let Some(work_status) = trace_work_status(&trace) else {
            return;
        };
        let Some(at_ms) = trace_at_ms(&trace, work_status) else {
            return;
        };

        if self.should_emit_status(&trace_id, work_status, at_ms) && at_ms >= cutoff_ms {
            self.status_by_trace
                .insert(trace_id.clone(), (work_status, at_ms));
            result.status = Some(StatusEvent {
                tool: self.tool,
                task_id: task_id_for_text(&trace_id),
                at_ms,
                status: work_status,
            });
        }

        if work_status != WorkStatus::Stopped {
            return;
        }
        let total_tokens = trace
            .model_info
            .as_ref()
            .and_then(|info| info.total_output_tokens)
            .unwrap_or_default();
        let emitted_tokens = self
            .span_usage_tokens
            .get(&trace_id)
            .copied()
            .unwrap_or_default();
        let output_tokens = total_tokens.saturating_sub(emitted_tokens);
        if output_tokens == 0 || !self.emitted_traces.insert(trace_id.clone()) {
            return;
        }
        let duration_ms = self
            .generation_ms
            .get(&trace_id)
            .copied()
            .filter(|duration| *duration > 0)
            .or_else(|| trace_elapsed_ms(&trace))
            .or(trace.duration)
            .unwrap_or_default();
        if duration_ms == 0 || at_ms < cutoff_ms {
            return;
        }
        result.event = Some(UsageEvent {
            tool: self.tool,
            task_id: task_id_for_text(&trace_id),
            at_ms,
            output_tokens,
            duration_ms,
        });
    }

    /// 状态变化才上报；仍在工作中的 trace 每分钟刷新一次，避免状态过期
    fn should_emit_status(&self, trace_id: &str, status: WorkStatus, at_ms: i64) -> bool {
        match self.status_by_trace.get(trace_id) {
            None => true,
            Some((previous, previous_at)) => {
                *previous != status
                    || (status == WorkStatus::Working
                        && at_ms.saturating_sub(*previous_at) >= WORKING_REFRESH_MS)
            }
        }
    }
}

fn trace_work_status(trace: &TraceRecord) -> Option<WorkStatus> {
    let Some(status) = trace.status.as_deref() else {
        // 缺少状态但已有结束时间时按终态处理
        return trace.ended_at.as_ref().map(|_| WorkStatus::Stopped);
    };
    match status.to_ascii_lowercase().as_str() {
        "running" | "in_progress" | "started" => Some(WorkStatus::Working),
        "ok" | "success" | "completed" | "error" | "failed" | "cancelled" | "canceled"
        | "stopped" | "timeout" => Some(WorkStatus::Stopped),
        _ => None,
    }
}

fn trace_at_ms(trace: &TraceRecord, status: WorkStatus) -> Option<i64> {
    let started = parse_optional_ms(trace.started_at.as_deref());
    let ended = parse_optional_ms(trace.ended_at.as_deref());
    if status == WorkStatus::Working {
        started.or(ended)
    } else {
        ended.or(started)
    }
}

fn trace_elapsed_ms(trace: &TraceRecord) -> Option<u64> {
    let started = parse_optional_ms(trace.started_at.as_deref())?;
    let ended = parse_optional_ms(trace.ended_at.as_deref())?;
    (ended > started).then_some((ended - started) as u64)
}

fn span_elapsed_ms(span: &SpanRecord) -> Option<u64> {
    let started = parse_optional_ms(span.started_at.as_deref())?;
    let ended = parse_optional_ms(span.ended_at.as_deref())?;
    (ended > started).then_some((ended - started) as u64)
}

fn span_at_ms(span: &SpanRecord) -> Option<i64> {
    parse_optional_ms(span.ended_at.as_deref()).or_else(|| parse_optional_ms(span.started_at.as_deref()))
}

fn parse_optional_ms(value: Option<&str>) -> Option<i64> {
    value.and_then(|text| parse_timestamp_ms(text).ok())
}

/// WorkBuddy 的 generation toolOutput 是模型响应列表；只读取 usage，
/// 按响应 id 去重，避免流式重放或多 choice 造成重复计数。
fn response_output_tokens(value: &Value) -> u64 {
    let mut total = 0u64;
    let mut seen = HashSet::new();
    collect_response_usage(value, 0, &mut total, &mut seen);
    total
}

fn collect_response_usage(
    value: &Value,
    depth: u8,
    total: &mut u64,
    seen: &mut HashSet<String>,
) {
    if depth > 3 {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                collect_response_usage(item, depth + 1, total, seen);
            }
        }
        Value::Object(response) => {
            if let Some(usage) = response.get("usage").and_then(Value::as_object) {
                let tokens = usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .or_else(|| usage.get("output_tokens").and_then(Value::as_u64))
                    .unwrap_or_default();
                let unique = response
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| seen.insert(id.to_string()))
                    .unwrap_or(true);
                if unique {
                    *total += tokens;
                }
            }
        }
        Value::String(inner) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(inner) {
                collect_response_usage(&parsed, depth + 1, total, seen);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Deserialize)]
struct TraceLine {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    trace: Option<TraceRecord>,
    #[serde(default)]
    span: Option<SpanRecord>,
}

#[derive(Debug, Deserialize)]
struct TraceDocument {
    #[serde(default)]
    trace: Option<TraceRecord>,
    #[serde(default)]
    spans: Vec<SpanRecord>,
}

#[derive(Debug, Deserialize)]
struct TraceRecord {
    #[serde(default, rename = "traceId")]
    trace_id: Option<String>,
    #[serde(default, rename = "startedAt")]
    started_at: Option<String>,
    #[serde(default, rename = "endedAt")]
    ended_at: Option<String>,
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default, rename = "modelInfo")]
    model_info: Option<ModelInfo>,
}

#[derive(Debug, Deserialize)]
struct ModelInfo {
    #[serde(default, rename = "totalOutputTokens")]
    total_output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct SpanRecord {
    #[serde(default, rename = "traceId")]
    trace_id: Option<String>,
    #[serde(default, rename = "spanId")]
    span_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default, rename = "startedAt")]
    started_at: Option<String>,
    #[serde(default, rename = "endedAt")]
    ended_at: Option<String>,
    #[serde(default)]
    duration: Option<u64>,
    /// WorkBuddy generation span 的模型响应，只用于提取 usage，不保留正文
    #[serde(default, rename = "toolOutput")]
    tool_output: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn timestamp_ms(value: &str) -> i64 {
        parse_timestamp_ms(value).expect("测试时间戳应合法")
    }

    #[test]
    fn workbuddy_jsonl_uses_output_tokens_and_generation_duration() {
        let mut parser = TraceParser::new(ToolId::WORK_BUDDY);
        let start = json!({
            "type": "trace_start",
            "trace": {
                "traceId": "trace-1",
                "startedAt": "2026-10-13T03:54:47.019Z",
                "status": "running",
                "totalTokens": 0
            }
        })
        .to_string();
        let first = parser.parse_line(&start, 0).expect("trace_start 应解析成功");
        assert!(first.event.is_none());
        assert_eq!(
            first.status.expect("应上报工作状态").status,
            WorkStatus::Working
        );

        for (span, duration) in [("span-1", 12_307u64), ("span-2", 4_670u64)] {
            let line = json!({
                "type": "span_end",
                "span": {
                    "traceId": "trace-1",
                    "spanId": span,
                    "name": "generation",
                    "type": "generation",
                    "startedAt": "2026-10-13T03:54:47.568Z",
                    "endedAt": "2026-10-13T03:54:59.875Z",
                    "duration": duration
                }
            })
            .to_string();
            assert!(parser
                .parse_line(&line, 0)
                .expect("span_end 应解析成功")
                .event
                .is_none());
        }

        let end = json!({
            "type": "trace_end",
            "trace": {
                "traceId": "trace-1",
                "startedAt": "2026-10-13T03:54:47.019Z",
                "endedAt": "2026-10-13T04:05:16.987Z",
                "duration": 629_968,
                "status": "ok",
                "totalTokens": 429_232,
                "modelInfo": {
                    "totalInputTokens": 428_507,
                    "totalOutputTokens": 725,
                    "totalCachedTokens": 426_368
                }
            }
        })
        .to_string();
        let terminal = parser.parse_line(&end, 0).expect("trace_end 应解析成功");
        let event = terminal.event.expect("应产生 token 事件");
        assert_eq!(event.output_tokens, 725, "只统计输出 token");
        assert_eq!(event.duration_ms, 16_977, "累加 generation span 耗时");
        assert_eq!(
            terminal.status.expect("应上报停止状态").status,
            WorkStatus::Stopped
        );

        let duplicate = parser.parse_line(&end, 0).expect("重复行应被忽略");
        assert!(duplicate.event.is_none());
        assert!(duplicate.status.is_none());
    }

    #[test]
    fn running_trace_without_end_never_emits_tokens() {
        let mut parser = TraceParser::new(ToolId::WORK_BUDDY);
        let start = json!({
            "type": "trace_start",
            "trace": {
                "traceId": "trace-2",
                "startedAt": "2026-10-23T02:00:00.000Z",
                "status": "running"
            }
        })
        .to_string();
        let result = parser.parse_line(&start, 0).expect("running 应解析成功");
        assert!(result.event.is_none());
        assert_eq!(result.status.expect("应产生状态").status, WorkStatus::Working);
    }

    #[test]
    fn workbuddy_generation_span_usage_is_emitted_immediately() {
        let mut parser = TraceParser::new(ToolId::WORK_BUDDY);
        let start = json!({
            "type": "trace_start",
            "trace": {
                "traceId": "trace-live",
                "startedAt": "2026-10-24T15:00:00.000Z",
                "status": "running",
                "totalTokens": 0
            }
        })
        .to_string();
        parser.parse_line(&start, 0).expect("trace_start 应解析成功");

        let span = json!({
            "type": "span_end",
            "span": {
                "traceId": "trace-live",
                "spanId": "span-live",
                "name": "generation",
                "type": "generation",
                "startedAt": "2026-10-24T15:00:01.000Z",
                "endedAt": "2026-10-24T15:00:05.000Z",
                "duration": 4000,
                "status": "ok",
                "toolOutput": json!([{
                    "id": "resp-live",
                    "usage": {
                        "prompt_tokens": 9_000,
                        "completion_tokens": 250,
                        "prompt_tokens_details": {"cached_tokens": 8_000},
                        "completion_tokens_details": {"reasoning_tokens": 120}
                    }
                }])
                .to_string()
            }
        })
        .to_string();

        let result = parser.parse_line(&span, 0).expect("span_end 应解析成功");
        let event = result.event.expect("span 结束时应立即产生 token 事件");
        assert_eq!(event.output_tokens, 250);
        assert_eq!(event.duration_ms, 4_000);
        assert_eq!(event.at_ms, timestamp_ms("2026-10-24T15:00:05.000Z"));
        assert!(parser
            .parse_line(&span, 0)
            .expect("重复 span 应正常解析")
            .event
            .is_none());

        let end = json!({
            "type": "trace_end",
            "trace": {
                "traceId": "trace-live",
                "startedAt": "2026-10-24T15:00:00.000Z",
                "endedAt": "2026-10-24T15:00:10.000Z",
                "duration": 10000,
                "status": "ok",
                "modelInfo": {"totalOutputTokens": 250}
            }
        })
        .to_string();
        let terminal = parser.parse_line(&end, 0).expect("trace_end 应解析成功");
        assert!(
            terminal.event.is_none(),
            "span 已上报用量后，终态汇总不应重复计数"
        );
        assert_eq!(
            terminal.status.expect("应上报停止状态").status,
            WorkStatus::Stopped
        );
    }

    #[test]
    fn workbuddy_terminal_only_tops_up_unreported_tokens() {
        let mut parser = TraceParser::new(ToolId::WORK_BUDDY);
        let span = json!({
            "type": "span_end",
            "span": {
                "traceId": "trace-top-up",
                "spanId": "span-top-up",
                "name": "generation",
                "type": "generation",
                "startedAt": "2026-10-24T15:00:01.000Z",
                "endedAt": "2026-10-24T15:00:05.000Z",
                "duration": 4000,
                "toolOutput": json!([{"id": "resp-top-up", "usage": {"completion_tokens": 250}}])
                    .to_string()
            }
        })
        .to_string();
        let event = parser
            .parse_line(&span, 0)
            .expect("span_end 应解析成功")
            .event
            .expect("span 结束时应立即产生 token 事件");
        assert_eq!(event.output_tokens, 250);

        let end = json!({
            "type": "trace_end",
            "trace": {
                "traceId": "trace-top-up",
                "startedAt": "2026-10-24T15:00:00.000Z",
                "endedAt": "2026-10-24T15:00:10.000Z",
                "status": "ok",
                "modelInfo": {"totalOutputTokens": 400}
            }
        })
        .to_string();
        let terminal = parser
            .parse_line(&end, 0)
            .expect("trace_end 应解析成功")
            .event
            .expect("终态应补报 span 未覆盖的 token");
        assert_eq!(terminal.output_tokens, 150);
        assert_eq!(terminal.duration_ms, 4_000);
        assert!(parser
            .parse_line(&end, 0)
            .expect("重复 trace_end 应正常解析")
            .event
            .is_none());
    }

    #[test]
    fn workbuddy_tool_output_deduplicates_repeated_responses() {
        let responses = json!([
            {"id": "resp-1", "usage": {"completion_tokens": 120}},
            {"id": "resp-1", "usage": {"completion_tokens": 120}},
            {"id": "resp-2", "usage": {"completion_tokens": 80}}
        ]);
        assert_eq!(response_output_tokens(&responses), 200);
        assert_eq!(response_output_tokens(&json!("not-json")), 0);
        assert_eq!(
            response_output_tokens(&json!([{"usage": {"output_tokens": 7}}])),
            7
        );
        assert_eq!(
            response_output_tokens(&Value::String(responses.to_string())),
            200
        );
    }

    #[test]
    fn codebuddy_document_ignores_broken_trace_duration() {
        let mut parser = TraceParser::new(ToolId::CODE_BUDDY);
        let document = json!({
            "trace": {
                "traceId": "trace-3",
                "startedAt": "2026-07-15T13:01:27.033Z",
                "endedAt": "2026-07-15T13:01:50.000Z",
                "duration": 143_457_615u64,
                "status": "ok",
                "totalTokens": 46_635,
                "modelInfo": {
                    "totalInputTokens": 223_444,
                    "totalOutputTokens": 354,
                    "totalCachedTokens": 44_800
                }
            },
            "spans": [
                {
                    "traceId": "trace-3",
                    "spanId": "span-1",
                    "name": "generation",
                    "type": "generation",
                    "duration": 11_770
                },
                {
                    "traceId": "trace-3",
                    "spanId": "span-2",
                    "name": "generation",
                    "type": "generation",
                    "duration": 33
                }
            ]
        })
        .to_string();

        let result = parser
            .parse_document(&document, 0)
            .expect("文档应解析成功");
        let event = result.event.expect("应产生事件");
        assert_eq!(event.output_tokens, 354);
        assert_eq!(event.duration_ms, 11_803);
        let status = result.status.expect("应产生状态");
        assert_eq!(status.status, WorkStatus::Stopped);
        assert_eq!(status.at_ms, timestamp_ms("2026-07-15T13:01:50.000Z"));
    }

    #[test]
    fn zero_token_terminal_trace_still_reports_status() {
        let mut parser = TraceParser::new(ToolId::WORK_BUDDY);
        let line = json!({
            "type": "trace_end",
            "trace": {
                "traceId": "trace-4",
                "startedAt": "2026-10-23T02:00:00.000Z",
                "endedAt": "2026-10-23T02:00:10.000Z",
                "status": "error",
                "totalTokens": 0
            }
        })
        .to_string();

        let result = parser.parse_line(&line, 0).expect("零 token 应正常解析");
        assert!(result.event.is_none());
        assert_eq!(result.status.expect("应产生状态").status, WorkStatus::Stopped);
    }

    #[test]
    fn terminal_trace_without_model_info_has_no_token_event() {
        let mut parser = TraceParser::new(ToolId::CODE_BUDDY);
        let document = json!({
            "trace": {
                "traceId": "trace-5",
                "startedAt": "2026-10-23T02:00:00.000Z",
                "endedAt": "2026-10-23T02:00:10.000Z",
                "status": "ok"
            },
            "spans": []
        })
        .to_string();

        let result = parser
            .parse_document(&document, 0)
            .expect("文档应解析成功");
        assert!(result.event.is_none());
        assert_eq!(result.status.expect("应产生状态").status, WorkStatus::Stopped);
    }
}
