/**
 * @Author: Danson zheng
 * @Date: 2026-10-24
 * @Description: Grok CLI 会话增量解析器。updates.jsonl 的 _meta.totalTokens 是
 * 上下文累计值，不能当作输出；这里只取 turn_completed 中精确的 usage.outputTokens
 * 与 apiDurationMs，并按 eventId 去重。状态来自流式事件，不读取任何消息正文。
 */

use super::{task_id_for_text, ParseResult};
use crate::live::{StatusEvent, UsageEvent, WorkStatus};
use crate::tools::ToolId;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// 工作中状态的最长有效期；持续流式输出时定期刷新，避免状态过期
const WORKING_REFRESH_MS: i64 = 60_000;

#[derive(Debug, Default)]
pub struct GrokParser {
    /// 已上报 token 的 eventId，避免重复读取时重复计数
    emitted_events: HashSet<String>,
    /// sessionId -> 最近一次上报的状态与时间
    status_by_task: HashMap<u64, (WorkStatus, i64)>,
}

impl GrokParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parse_line(&mut self, line: &str, cutoff_ms: i64) -> Result<ParseResult, String> {
        let value: Value =
            serde_json::from_str(line).map_err(|error| format!("JSON 解析失败: {error}"))?;
        let Some(params) = value.get("params") else {
            return Ok(ParseResult::default());
        };
        let Some(update) = params.get("update") else {
            return Ok(ParseResult::default());
        };
        let Some(kind) = update.get("sessionUpdate").and_then(Value::as_str) else {
            return Ok(ParseResult::default());
        };
        let Some(at_ms) = event_at_ms(&value, params) else {
            return Ok(ParseResult::default());
        };

        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("grok");
        let task_id = task_id_for_text(session_id);
        let mut result = ParseResult::default();

        if kind == "turn_completed" {
            if let Some(event) = self.turn_usage_event(params, update, task_id, at_ms, cutoff_ms) {
                result.event = Some(event);
            }
        }

        if let Some(status) = status_from_update(update) {
            if self.should_emit_status(task_id, status, at_ms) && at_ms >= cutoff_ms {
                self.status_by_task.insert(task_id, (status, at_ms));
                result.status = Some(StatusEvent {
                    tool: ToolId::GROK,
                    task_id,
                    at_ms,
                    status,
                });
            }
        }

        Ok(result)
    }

    fn turn_usage_event(
        &mut self,
        params: &Value,
        update: &Value,
        task_id: u64,
        at_ms: i64,
        cutoff_ms: i64,
    ) -> Option<UsageEvent> {
        let usage = update.get("usage")?;
        let output_tokens = usage.get("outputTokens").and_then(Value::as_u64)?;
        let duration_ms = usage
            .get("apiDurationMs")
            .and_then(Value::as_u64)
            .filter(|duration| *duration > 0)?;
        if output_tokens == 0 || at_ms < cutoff_ms {
            return None;
        }

        let event_id = params
            .get("_meta")
            .and_then(|meta| meta.get("eventId"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("{task_id}-{at_ms}"));
        if !self.emitted_events.insert(event_id) {
            return None;
        }

        Some(UsageEvent {
            tool: ToolId::GROK,
            task_id,
            at_ms,
            output_tokens,
            duration_ms,
        })
    }

    fn should_emit_status(&self, task_id: u64, status: WorkStatus, at_ms: i64) -> bool {
        match self.status_by_task.get(&task_id) {
            None => true,
            Some((previous, previous_at)) => {
                *previous != status
                    || (status == WorkStatus::Working
                        && at_ms.saturating_sub(*previous_at) >= WORKING_REFRESH_MS)
            }
        }
    }
}

fn event_at_ms(value: &Value, params: &Value) -> Option<i64> {
    params
        .get("_meta")
        .and_then(|meta| meta.get("agentTimestampMs"))
        .and_then(Value::as_i64)
        .filter(|at_ms| *at_ms > 0)
        .or_else(|| {
            value
                .get("timestamp")
                .and_then(Value::as_i64)
                .and_then(|seconds| seconds.checked_mul(1_000))
        })
}

fn status_from_update(update: &Value) -> Option<WorkStatus> {
    match update.get("sessionUpdate").and_then(Value::as_str)? {
        "turn_completed" => Some(WorkStatus::Stopped),
        "tool_call" | "agent_message_chunk" | "agent_thought_chunk" | "plan"
        | "retry_state" | "hook_execution" | "subagent_spawned" | "subagent_finished" => {
            Some(WorkStatus::Working)
        }
        "tool_call_update" => match update.get("status").and_then(Value::as_str) {
            Some("in_progress") => Some(WorkStatus::Working),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(update: Value, event_id: &str, at_ms: i64) -> String {
        json!({
            "timestamp": at_ms / 1000,
            "method": "_x.ai/session/update",
            "params": {
                "sessionId": "session-1",
                "update": update,
                "_meta": {
                    "eventId": event_id,
                    "agentTimestampMs": at_ms
                }
            }
        })
        .to_string()
    }

    fn completed(usage: Value, event_id: &str, at_ms: i64) -> String {
        line(
            json!({
                "sessionUpdate": "turn_completed",
                "prompt_id": "prompt-1",
                "stop_reason": "end_turn",
                "usage": usage
            }),
            event_id,
            at_ms,
        )
    }

    #[test]
    fn turn_completed_emits_precise_output_tokens_and_api_duration() {
        let mut parser = GrokParser::new();
        let record = completed(
            json!({
                "inputTokens": 77_234,
                "outputTokens": 1_009,
                "totalTokens": 78_243,
                "cachedReadTokens": 54_528,
                "apiDurationMs": 32_496,
                "modelCalls": 4
            }),
            "event-1",
            1_785_207_014_081,
        );

        let result = parser.parse_line(&record, 0).expect("终态应解析成功");
        let event = result.event.expect("应产生 token 事件");
        assert_eq!(event.output_tokens, 1_009);
        assert_eq!(event.duration_ms, 32_496);
        assert_eq!(result.status.expect("应产生状态").status, WorkStatus::Stopped);

        let duplicate = parser.parse_line(&record, 0).expect("重复事件应忽略");
        assert!(duplicate.event.is_none());
        assert!(duplicate.status.is_none());
    }

    #[test]
    fn cancelled_turn_without_usage_reports_stopped_only() {
        let mut parser = GrokParser::new();
        let record = line(
            json!({"sessionUpdate": "turn_completed", "stop_reason": "cancelled"}),
            "event-2",
            1_785_401_830_518,
        );

        let result = parser.parse_line(&record, 0).expect("取消事件应解析成功");
        assert!(result.event.is_none());
        assert_eq!(result.status.expect("应产生状态").status, WorkStatus::Stopped);
    }

    #[test]
    fn streaming_chunk_reports_working_without_token_event() {
        let mut parser = GrokParser::new();
        let record = line(
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": ""}}),
            "event-3",
            1_785_207_000_000,
        );

        let result = parser.parse_line(&record, 0).expect("流式事件应解析成功");
        assert!(result.event.is_none());
        assert_eq!(result.status.expect("应产生状态").status, WorkStatus::Working);
    }

    #[test]
    fn zero_duration_usage_is_ignored() {
        let mut parser = GrokParser::new();
        let record = completed(
            json!({"outputTokens": 100, "apiDurationMs": 0}),
            "event-4",
            1_785_207_000_000,
        );
        assert!(parser
            .parse_line(&record, 0)
            .expect("零耗时事件应正常解析")
            .event
            .is_none());
    }
}
