/**
 * @Author: Danson zheng
 * @Date: 2026-10-24
 * @Description: Kimi Code 事件日志解析器。turn.step.completed 提供输出 token 与
 * LLM 流式耗时；work_changed / approval 事件提供工作中与等待确认状态。
 * 仅访问结构字段，不读取消息正文。
 */

use super::{parse_timestamp_ms, task_id_for_text, ParseResult};
use crate::live::{StatusEvent, UsageEvent, WorkStatus};
use crate::tools::ToolId;
use serde_json::Value;
use std::collections::HashMap;

/// 工作中状态的最长有效期；事件持续到来时定期刷新，避免状态过期
const WORKING_REFRESH_MS: i64 = 60_000;

#[derive(Debug, Default)]
pub struct KimiParser {
    /// 任务 -> 最近一次上报的状态与时间
    status_by_task: HashMap<u64, (WorkStatus, i64)>,
}

impl KimiParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parse_line(&mut self, line: &str, cutoff_ms: i64) -> Result<ParseResult, String> {
        let value: Value =
            serde_json::from_str(line).map_err(|error| format!("JSON 解析失败: {error}"))?;
        if value.get("kind").and_then(Value::as_str) != Some("event") {
            return Ok(ParseResult::default());
        }
        let Some(envelope) = value.get("envelope") else {
            return Ok(ParseResult::default());
        };
        let Some(kind) = envelope.get("type").and_then(Value::as_str) else {
            return Ok(ParseResult::default());
        };
        let Some(payload) = envelope.get("payload") else {
            return Ok(ParseResult::default());
        };
        let Some(at_ms) = envelope_at_ms(envelope, payload) else {
            return Ok(ParseResult::default());
        };

        let task_key = envelope
            .get("session_id")
            .and_then(Value::as_str)
            .or_else(|| payload.get("sessionId").and_then(Value::as_str))
            .unwrap_or("kimi");
        let task_id = task_id_for_text(task_key);

        let mut result = ParseResult::default();
        if kind == "turn.step.completed" {
            let output_tokens = payload
                .get("usage")
                .and_then(|usage| usage.get("output"))
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let duration_ms = payload
                .get("llmStreamDurationMs")
                .and_then(Value::as_u64)
                .filter(|duration| *duration > 0);
            if output_tokens > 0 && at_ms >= cutoff_ms {
                if let Some(duration_ms) = duration_ms {
                    result.event = Some(UsageEvent {
                        tool: ToolId::KIMI_CODE,
                        task_id,
                        at_ms,
                        output_tokens,
                        duration_ms,
                    });
                }
            }
        }

        let status = match kind {
            "event.session.work_changed" => {
                let pending = payload.get("pending_interaction").and_then(Value::as_str);
                let busy = payload.get("busy").and_then(Value::as_bool).unwrap_or(false);
                let active = payload
                    .get("main_turn_active")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if pending == Some("approval") {
                    Some(WorkStatus::NeedsConfirmation)
                } else if busy && active {
                    Some(WorkStatus::Working)
                } else if !busy {
                    Some(WorkStatus::Stopped)
                } else {
                    None
                }
            }
            "permission.approval.requested" | "event.approval.requested" => {
                Some(WorkStatus::NeedsConfirmation)
            }
            "permission.approval.resolved" | "event.approval.resolved" => {
                Some(WorkStatus::Working)
            }
            "turn.started" | "turn.step.started" | "prompt.started" | "prompt.submitted"
            | "turn.step.completed" | "tool.call.started" | "tool.result" => {
                Some(WorkStatus::Working)
            }
            "turn.ended" | "prompt.completed" | "prompt.aborted" | "turn.step.interrupted" => {
                Some(WorkStatus::Stopped)
            }
            _ => None,
        };

        if let Some(status) = status {
            let should_emit = match self.status_by_task.get(&task_id) {
                None => true,
                Some((previous, previous_at)) => {
                    *previous != status
                        || (status == WorkStatus::Working
                            && at_ms.saturating_sub(*previous_at) >= WORKING_REFRESH_MS)
                }
            };
            if should_emit && at_ms >= cutoff_ms {
                self.status_by_task.insert(task_id, (status, at_ms));
                result.status = Some(StatusEvent {
                    tool: ToolId::KIMI_CODE,
                    task_id,
                    at_ms,
                    status,
                });
            }
        }

        Ok(result)
    }
}

fn envelope_at_ms(envelope: &Value, payload: &Value) -> Option<i64> {
    if let Some(text) = envelope.get("timestamp").and_then(Value::as_str) {
        if let Ok(at_ms) = parse_timestamp_ms(text) {
            return Some(at_ms);
        }
    }
    if let Some(at_ms) = envelope.get("timestamp").and_then(Value::as_i64) {
        return Some(normalize_epoch_ms(at_ms));
    }
    payload
        .get("time")
        .and_then(Value::as_i64)
        .map(normalize_epoch_ms)
}

/// 兼容秒、毫秒、微秒三种 epoch 精度
fn normalize_epoch_ms(value: i64) -> i64 {
    match value {
        0..=9_999_999_999 => value.saturating_mul(1_000),
        10_000_000_000..=9_999_999_999_999 => value,
        _ => value / 1_000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(kind: &str, timestamp: &str, payload: Value) -> String {
        json!({
            "kind": "event",
            "seq": 1,
            "envelope": {
                "type": kind,
                "session_id": "session-kimi",
                "timestamp": timestamp,
                "payload": payload
            }
        })
        .to_string()
    }

    #[test]
    fn step_completed_emits_output_tokens_and_duration() {
        let mut parser = KimiParser::new();
        let record = line(
            "turn.step.completed",
            "2026-10-23T02:00:03.717Z",
            json!({"usage": {"output": 210}, "llmStreamDurationMs": 3717}),
        );
        let event = parser
            .parse_line(&record, 0)
            .expect("事件应解析成功")
            .event
            .expect("应产生 token 事件");
        assert_eq!(event.output_tokens, 210);
        assert_eq!(event.duration_ms, 3_717);
    }

    #[test]
    fn approval_changes_status_to_needs_confirmation() {
        let mut parser = KimiParser::new();
        let record = line(
            "event.session.work_changed",
            "2026-10-23T02:00:02.000Z",
            json!({
                "busy": true,
                "main_turn_active": true,
                "pending_interaction": "approval"
            }),
        );
        let result = parser.parse_line(&record, 0).expect("事件应解析成功");
        assert_eq!(
            result.status.expect("应产生状态").status,
            WorkStatus::NeedsConfirmation
        );

        let resolved = line(
            "event.session.work_changed",
            "2026-10-23T02:00:05.000Z",
            json!({
                "busy": true,
                "main_turn_active": true,
                "pending_interaction": "none"
            }),
        );
        let result = parser.parse_line(&resolved, 0).expect("事件应解析成功");
        assert_eq!(
            result.status.expect("应产生状态").status,
            WorkStatus::Working
        );
    }

    #[test]
    fn idle_work_changed_reports_stopped() {
        let mut parser = KimiParser::new();
        let record = line(
            "event.session.work_changed",
            "2026-10-23T02:00:05.000Z",
            json!({"busy": false, "main_turn_active": false, "pending_interaction": "none"}),
        );
        let result = parser.parse_line(&record, 0).expect("事件应解析成功");
        assert_eq!(result.status.expect("应产生状态").status, WorkStatus::Stopped);
    }
}
