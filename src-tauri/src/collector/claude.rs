/**
 * @Author: Danson zheng
 * @Date: 2026-10-23
 * @Description: Claude Code 会话日志解析器。从 assistant 记录的 message.usage 提取每次
 * API 响应的输出 token，并按 message.id 合并多 content block；生成耗时优先取同 requestRef
 * 的 api-request 起始时间，缺失时回退到最近一条 user 记录时间。
 */

use super::parse_timestamp_ms;
use crate::live::UsageEvent;
use crate::tools::ToolId;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

/// 单文件内的 Claude 解析状态
#[derive(Debug, Default)]
pub struct ClaudeParser {
    /// 同一 message.id 会拆成多条 assistant 记录，只统计一次
    seen_message_ids: HashSet<String>,
    /// api-request.id -> 请求构造时间，作为生成耗时起点
    request_starts: HashMap<String, i64>,
    /// 缺少 api-request 时，用最近一条 user 消息时间兜底
    latest_user_at_ms: Option<i64>,
}

impl ClaudeParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 解析一行；返回 Some 表示产生了一条可计入实时指标的增量事件
    pub fn parse_line(
        &mut self,
        line: &str,
        emit_after_ms: i64,
    ) -> Result<Option<UsageEvent>, String> {
        let record: ClaudeRecord =
            serde_json::from_str(line).map_err(|error| format!("JSON 解析失败: {error}"))?;

        match record.kind.as_str() {
            "user" => {
                if let Some(timestamp) = record.timestamp.as_deref() {
                    self.latest_user_at_ms = Some(parse_timestamp_ms(timestamp)?);
                }
                Ok(None)
            }
            "api-request" => {
                let (Some(request_id), Some(timestamp)) =
                    (record.id.as_deref(), record.timestamp.as_deref())
                else {
                    return Ok(None);
                };
                let start_ms = parse_timestamp_ms(timestamp)?;
                self.request_starts.insert(request_id.to_string(), start_ms);
                Ok(None)
            }
            "assistant" => self.parse_assistant(record, emit_after_ms),
            _ => Ok(None),
        }
    }

    fn parse_assistant(
        &mut self,
        record: ClaudeRecord,
        emit_after_ms: i64,
    ) -> Result<Option<UsageEvent>, String> {
        let Some(message) = record.message else {
            return Ok(None);
        };
        let Some(message_id) = message.id else {
            return Ok(None);
        };
        if !self.seen_message_ids.insert(message_id.clone()) {
            return Ok(None);
        }

        let output_tokens = message
            .usage
            .map(|usage| usage.output_tokens)
            .unwrap_or_default();
        if output_tokens == 0 {
            return Ok(None);
        }

        let Some(timestamp) = record.timestamp.as_deref() else {
            return Err("assistant 记录缺少 timestamp".to_string());
        };
        let at_ms = parse_timestamp_ms(timestamp)?;

        let start_ms = record
            .request_ref
            .as_deref()
            .and_then(|request_id| self.request_starts.get(request_id).copied())
            .or(self.latest_user_at_ms);

        let Some(start_ms) = start_ms else {
            log::debug!(
                "【日志采集】Claude 助手消息缺少起始时间，已跳过 message_id={message_id}"
            );
            return Ok(None);
        };

        if at_ms <= start_ms {
            log::debug!(
                "【日志采集】Claude 助手消息时间顺序异常，已跳过 message_id={} at_ms={} start_ms={}",
                message_id,
                at_ms,
                start_ms
            );
            return Ok(None);
        }

        if at_ms < emit_after_ms {
            return Ok(None);
        }

        Ok(Some(UsageEvent {
            tool: ToolId::CLAUDE_CODE,
            task_id: 0,
            at_ms,
            output_tokens,
            duration_ms: (at_ms - start_ms) as u64,
        }))
    }
}

#[derive(Debug, Deserialize)]
struct ClaudeRecord {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default, rename = "requestRef")]
    request_ref: Option<String>,
    /// api-request 记录自身的 id
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    message: Option<ClaudeMessage>,
}

#[derive(Debug, Deserialize)]
struct ClaudeMessage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    usage: Option<ClaudeUsage>,
}

#[derive(Debug, Deserialize)]
struct ClaudeUsage {
    #[serde(default)]
    output_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use serde_json::json;

    fn line(value: serde_json::Value) -> String {
        value.to_string()
    }

    fn timestamp_ms(value: &str) -> i64 {
        DateTime::parse_from_rfc3339(value)
            .expect("测试时间戳应合法")
            .timestamp_millis()
    }

    #[test]
    fn assistant_usage_becomes_single_event_using_request_start() {
        let mut parser = ClaudeParser::new();

        let user = line(json!({
            "type": "user",
            "timestamp": "2026-10-23T02:00:00.000Z"
        }));
        assert_eq!(parser.parse_line(&user, 0).expect("user 应解析成功"), None);

        let request = line(json!({
            "type": "api-request",
            "id": "req-1",
            "timestamp": "2026-10-23T02:00:01.000Z"
        }));
        assert_eq!(
            parser.parse_line(&request, 0).expect("api-request 应解析成功"),
            None
        );

        let assistant = json!({
            "type": "assistant",
            "timestamp": "2026-10-23T02:00:04.000Z",
            "requestRef": "req-1",
            "message": {
                "id": "msg-1",
                "usage": { "output_tokens": 100 }
            }
        });
        let first = parser
            .parse_line(&line(assistant.clone()), 0)
            .expect("assistant 应解析成功")
            .expect("应产生事件");

        assert_eq!(first.tool, ToolId::CLAUDE_CODE);
        assert_eq!(first.output_tokens, 100);
        assert_eq!(first.duration_ms, 3_000);
        assert_eq!(first.at_ms, timestamp_ms("2026-10-23T02:00:04.000Z"));

        let duplicate = parser
            .parse_line(&line(assistant), 0)
            .expect("重复 assistant 应解析成功");
        assert_eq!(duplicate, None);
    }

    #[test]
    fn duration_falls_back_to_latest_user_record() {
        let mut parser = ClaudeParser::new();
        let user = line(json!({
            "type": "user",
            "timestamp": "2026-10-23T02:00:00.000Z"
        }));
        parser.parse_line(&user, 0).expect("user 应解析成功");

        let assistant = line(json!({
            "type": "assistant",
            "timestamp": "2026-10-23T02:00:02.500Z",
            "message": {
                "id": "msg-fallback",
                "usage": { "output_tokens": 50 }
            }
        }));
        let event = parser
            .parse_line(&assistant, 0)
            .expect("assistant 应解析成功")
            .expect("应产生事件");

        assert_eq!(event.output_tokens, 50);
        assert_eq!(event.duration_ms, 2_500);
    }

    #[test]
    fn zero_output_is_ignored_and_invalid_json_returns_error() {
        let mut parser = ClaudeParser::new();
        let assistant = line(json!({
            "type": "assistant",
            "timestamp": "2026-10-23T02:00:04.000Z",
            "message": {
                "id": "msg-empty",
                "usage": { "output_tokens": 0 }
            }
        }));

        assert_eq!(
            parser.parse_line(&assistant, 0).expect("零输出应正常忽略"),
            None
        );
        assert!(parser.parse_line("not-json", 0).is_err());
    }

    #[test]
    fn assistant_without_start_marker_is_skipped() {
        let mut parser = ClaudeParser::new();
        let assistant = line(json!({
            "type": "assistant",
            "timestamp": "2026-10-23T02:00:02.000Z",
            "message": {
                "id": "msg-no-start",
                "usage": { "output_tokens": 50 }
            }
        }));

        assert_eq!(
            parser
                .parse_line(&assistant, 0)
                .expect("缺少起始时间应正常跳过"),
            None
        );
    }

    #[test]
    fn events_before_cutoff_are_not_emitted() {
        let mut parser = ClaudeParser::new();
        let user = line(json!({
            "type": "user",
            "timestamp": "2026-10-23T02:00:00.000Z"
        }));
        parser.parse_line(&user, 0).expect("user 应解析成功");

        let assistant = line(json!({
            "type": "assistant",
            "timestamp": "2026-10-23T02:00:02.000Z",
            "message": {
                "id": "msg-old",
                "usage": { "output_tokens": 10 }
            }
        }));
        assert_eq!(
            parser
                .parse_line(&assistant, timestamp_ms("2026-10-23T03:00:00.000Z"))
                .expect("assistant 应解析成功"),
            None
        );
    }
}
