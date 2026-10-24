/**
 * @Author: Danson zheng
 * @Date: 2026-10-23
 * @Description: Codex CLI 会话日志解析器。读取 token_count 事件里的 last_token_usage
 * 增量输出，并用任务开始时间与上一条 token_count 推导生成耗时。
 * item_completed 紧邻 token_count，属于同一次响应内的工具事件，不能作为计时起点。
 */

use super::parse_timestamp_ms;
use crate::live::UsageEvent;
use crate::tools::ToolId;
use serde::Deserialize;

/// 单文件内的 Codex 解析状态
#[derive(Debug, Default)]
pub struct CodexParser {
    /// 最近一次任务开始时间，用于跨轮次时避免把空闲时间算进速度
    turn_started_at_ms: Option<i64>,
    /// 上一条 token_count 的时间，连续生成片段之间用它做起点
    last_usage_at_ms: Option<i64>,
    /// 已消费的累计输出 token，用于识别日志里 total 未推进的重复上报
    last_total_output_tokens: Option<u64>,
}

impl CodexParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 解析一行；返回 Some 表示产生了一条可计入实时指标的增量事件
    pub fn parse_line(
        &mut self,
        line: &str,
        emit_after_ms: i64,
    ) -> Result<Option<UsageEvent>, String> {
        let record: CodexRecord =
            serde_json::from_str(line).map_err(|error| format!("JSON 解析失败: {error}"))?;
        let Some(payload) = record.payload else {
            return Ok(None);
        };

        match payload.kind.as_deref() {
            Some("task_started") => {
                if let Some(timestamp) = record.timestamp.as_deref() {
                    self.turn_started_at_ms = Some(parse_timestamp_ms(timestamp)?);
                }
                Ok(None)
            }
            Some("token_count") => {
                self.parse_token_count(payload, record.timestamp.as_deref(), emit_after_ms)
            }
            _ => Ok(None),
        }
    }

    fn parse_token_count(
        &mut self,
        payload: CodexPayload,
        timestamp: Option<&str>,
        emit_after_ms: i64,
    ) -> Result<Option<UsageEvent>, String> {
        let Some(timestamp) = timestamp else {
            return Err("token_count 记录缺少 timestamp".to_string());
        };
        let at_ms = parse_timestamp_ms(timestamp)?;

        // output_tokens 已包含 reasoning token；累计值可能因上下文压缩而回退
        let last_usage = payload
            .info
            .as_ref()
            .and_then(|info| info.last_token_usage.as_ref())
            .map(|usage| usage.output_tokens)
            .unwrap_or_default();
        let total_usage = payload
            .info
            .as_ref()
            .and_then(|info| info.total_token_usage.as_ref())
            .map(|usage| usage.output_tokens);

        let output_tokens = match total_usage {
            Some(total) => match self.last_total_output_tokens {
                Some(previous) if total > previous => total - previous,
                Some(previous) if total < previous => last_usage,
                Some(_) => {
                    // 累计值未推进说明是重复上报，不能重复计数，也不能重置计时起点
                    log::debug!(
                        "【日志采集】Codex token 累计值未推进，已跳过 at_ms={at_ms} total={total}"
                    );
                    return Ok(None);
                }
                None => last_usage,
            },
            None => last_usage,
        };
        if let Some(total) = total_usage {
            self.last_total_output_tokens = Some(total);
        }

        if output_tokens == 0 {
            return Ok(None);
        }

        let start_ms = match (self.turn_started_at_ms, self.last_usage_at_ms) {
            (Some(turn_start), Some(last_usage)) => Some(turn_start.max(last_usage)),
            (Some(turn_start), None) => Some(turn_start),
            (None, Some(last_usage)) => Some(last_usage),
            (None, None) => None,
        };
        self.last_usage_at_ms = Some(at_ms);

        let Some(start_ms) = start_ms else {
            log::debug!(
                "【日志采集】Codex token 事件缺少起始时间，已跳过 at_ms={at_ms}"
            );
            return Ok(None);
        };

        if at_ms <= start_ms {
            log::debug!(
                "【日志采集】Codex token 事件时间顺序异常，已跳过 at_ms={} start_ms={}",
                at_ms,
                start_ms
            );
            return Ok(None);
        }

        if at_ms < emit_after_ms {
            return Ok(None);
        }

        Ok(Some(UsageEvent {
            tool: ToolId::CODEX_CLI,
            task_id: 0,
            at_ms,
            output_tokens,
            duration_ms: (at_ms - start_ms) as u64,
        }))
    }
}

#[derive(Debug, Deserialize)]
struct CodexRecord {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    payload: Option<CodexPayload>,
}

#[derive(Debug, Deserialize)]
struct CodexPayload {
    /// 会话元数据等 payload 可能没有 type，按未知类型忽略而不是报错
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    info: Option<CodexTokenInfo>,
}

#[derive(Debug, Deserialize)]
struct CodexTokenInfo {
    #[serde(default)]
    last_token_usage: Option<CodexTokenUsage>,
    #[serde(default)]
    total_token_usage: Option<CodexTokenUsage>,
}

#[derive(Debug, Deserialize)]
struct CodexTokenUsage {
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

    fn task_started(timestamp: &str) -> String {
        line(json!({
            "timestamp": timestamp,
            "payload": { "type": "task_started" }
        }))
    }

    fn token_count(timestamp: &str, output_tokens: u64) -> String {
        line(json!({
            "timestamp": timestamp,
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": { "output_tokens": output_tokens }
                }
            }
        }))
    }

    fn token_count_with_total(timestamp: &str, output_tokens: u64, total_tokens: u64) -> String {
        line(json!({
            "timestamp": timestamp,
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": { "output_tokens": output_tokens },
                    "total_token_usage": { "output_tokens": total_tokens }
                }
            }
        }))
    }

    #[test]
    fn token_delta_uses_task_start_then_previous_usage() {
        let mut parser = CodexParser::new();
        parser
            .parse_line(&task_started("2026-10-23T02:00:00.000Z"), 0)
            .expect("task_started 应解析成功");

        let first = parser
            .parse_line(&token_count("2026-10-23T02:00:10.000Z", 200), 0)
            .expect("token_count 应解析成功")
            .expect("应产生事件");
        assert_eq!(first.tool, ToolId::CODEX_CLI);
        assert_eq!(first.output_tokens, 200);
        assert_eq!(first.duration_ms, 10_000);
        assert_eq!(first.at_ms, timestamp_ms("2026-10-23T02:00:10.000Z"));

        let second = parser
            .parse_line(&token_count("2026-10-23T02:00:15.000Z", 100), 0)
            .expect("token_count 应解析成功")
            .expect("应产生事件");
        assert_eq!(second.output_tokens, 100);
        assert_eq!(second.duration_ms, 5_000);
    }

    #[test]
    fn item_completion_does_not_shrink_generation_duration() {
        let mut parser = CodexParser::new();
        parser
            .parse_line(&task_started("2026-10-23T02:00:00.000Z"), 0)
            .expect("task_started 应解析成功");
        parser
            .parse_line(&token_count("2026-10-23T02:00:10.000Z", 100), 0)
            .expect("token_count 应解析成功");

        // 真实日志中 item_completed 紧邻 token_count，若当作起点会把耗时压到毫秒级
        let tool = line(json!({
            "timestamp": "2026-10-23T02:00:30.000Z",
            "payload": {
                "type": "item_completed",
                "completed_at_ms": timestamp_ms("2026-10-23T02:00:30.000Z"),
                "item": { "type": "CommandExecution" }
            }
        }));
        parser.parse_line(&tool, 0).expect("工具完成应解析成功");

        let event = parser
            .parse_line(&token_count("2026-10-23T02:00:40.000Z", 250), 0)
            .expect("token_count 应解析成功")
            .expect("应产生事件");
        assert_eq!(event.output_tokens, 250);
        assert_eq!(event.duration_ms, 30_000);
    }

    #[test]
    fn duplicate_cumulative_total_is_not_counted_twice() {
        let mut parser = CodexParser::new();
        parser
            .parse_line(&task_started("2026-10-23T02:00:00.000Z"), 0)
            .expect("task_started 应解析成功");

        let first = parser
            .parse_line(
                &token_count_with_total("2026-10-23T02:00:10.000Z", 100, 100),
                0,
            )
            .expect("token_count 应解析成功")
            .expect("应产生事件");
        assert_eq!(first.duration_ms, 10_000);

        let duplicate = parser
            .parse_line(
                &token_count_with_total("2026-10-23T02:00:12.000Z", 100, 100),
                0,
            )
            .expect("重复 token_count 应正常忽略");
        assert_eq!(duplicate, None);

        let next = parser
            .parse_line(
                &token_count_with_total("2026-10-23T02:00:15.000Z", 50, 150),
                0,
            )
            .expect("token_count 应解析成功")
            .expect("应产生事件");
        assert_eq!(next.output_tokens, 50);
        assert_eq!(next.duration_ms, 5_000);
    }

    #[test]
    fn cumulative_total_reset_falls_back_to_last_delta() {
        let mut parser = CodexParser::new();
        parser
            .parse_line(&task_started("2026-10-23T02:00:00.000Z"), 0)
            .expect("task_started 应解析成功");
        parser
            .parse_line(
                &token_count_with_total("2026-10-23T02:00:10.000Z", 100, 500),
                0,
            )
            .expect("token_count 应解析成功");

        let reset = parser
            .parse_line(
                &token_count_with_total("2026-10-23T02:00:20.000Z", 40, 20),
                0,
            )
            .expect("累计值回退应正常处理")
            .expect("应产生事件");
        assert_eq!(reset.output_tokens, 40);

        let next = parser
            .parse_line(
                &token_count_with_total("2026-10-23T02:00:30.000Z", 20, 40),
                0,
            )
            .expect("token_count 应解析成功")
            .expect("应产生事件");
        assert_eq!(next.output_tokens, 20);
    }

    #[test]
    fn zero_output_is_ignored_without_resetting_baseline() {
        let mut parser = CodexParser::new();
        parser
            .parse_line(&task_started("2026-10-23T02:00:00.000Z"), 0)
            .expect("task_started 应解析成功");

        assert_eq!(
            parser
                .parse_line(&token_count("2026-10-23T02:00:10.000Z", 0), 0)
                .expect("零输出应正常忽略"),
            None
        );

        let event = parser
            .parse_line(&token_count("2026-10-23T02:00:15.000Z", 120), 0)
            .expect("token_count 应解析成功")
            .expect("应产生事件");
        assert_eq!(event.duration_ms, 15_000);
    }

    #[test]
    fn payload_without_type_is_ignored_without_error() {
        let mut parser = CodexParser::new();
        let record = line(json!({
            "timestamp": "2026-10-23T02:00:00.000Z",
            "payload": { "id": "session-meta", "cwd": "/tmp" }
        }));
        assert_eq!(
            parser.parse_line(&record, 0).expect("缺少 type 的 payload 应正常忽略"),
            None
        );
    }

    #[test]
    fn invalid_json_returns_error() {
        let mut parser = CodexParser::new();
        assert!(parser.parse_line("not-json", 0).is_err());
    }
}
