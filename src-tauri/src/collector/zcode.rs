/**
 * @Author: Danson zheng
 * @Date: 2026-10-24
 * @Description: ZCode 模型调用日志解析器。从 ~/.zcode/cli/rollout/model-io-*.jsonl
 * 的 model_io 记录提取单次请求输出 token 与耗时，按 requestId 去重。
 * 只读取 token、时间与耗时字段，不触碰请求或响应正文。
 */

use super::parse_timestamp_ms;
use crate::live::UsageEvent;
use crate::tools::ToolId;
use serde::Deserialize;
use std::collections::HashSet;

/// 单文件内的 ZCode 解析状态
#[derive(Debug, Default)]
pub struct ZcodeParser {
    /// 一次模型调用可能写入多条记录，只统计一次
    seen_request_ids: HashSet<String>,
}

impl ZcodeParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 解析一行；返回 Some 表示产生了一条可计入实时指标的增量事件
    pub fn parse_line(
        &mut self,
        line: &str,
        emit_after_ms: i64,
    ) -> Result<Option<UsageEvent>, String> {
        let record: ZcodeRecord =
            serde_json::from_str(line).map_err(|error| format!("JSON 解析失败: {error}"))?;
        if record.kind.as_deref() != Some("model_io") {
            return Ok(None);
        }

        let Some(request_id) = record.request_id.as_deref() else {
            return Ok(None);
        };
        if !self.seen_request_ids.insert(request_id.to_string()) {
            return Ok(None);
        }

        let output_tokens = record
            .response
            .as_ref()
            .map(ZcodeResponse::output_tokens)
            .unwrap_or_default();
        if output_tokens == 0 {
            return Ok(None);
        }

        let Some(completed_at) = record.completed_at.as_deref() else {
            return Err("model_io 记录缺少 completedAt".to_string());
        };
        let at_ms = parse_timestamp_ms(completed_at)?;

        let duration_ms = record
            .duration_ms
            .filter(|duration| *duration > 0)
            .or_else(|| {
                let started_at = record.started_at.as_deref()?;
                let start_ms = parse_timestamp_ms(started_at).ok()?;
                u64::try_from(at_ms - start_ms).ok().filter(|value| *value > 0)
            })
            .unwrap_or_default();
        if duration_ms == 0 {
            log::debug!(
                "【日志采集】ZCode 记录缺少有效耗时，已跳过 request_id={request_id}"
            );
            return Ok(None);
        }

        if at_ms < emit_after_ms {
            return Ok(None);
        }

        Ok(Some(UsageEvent {
            tool: ToolId::ZCODE,
            task_id: 0,
            at_ms,
            output_tokens,
            duration_ms,
        }))
    }
}

#[derive(Debug, Deserialize)]
struct ZcodeRecord {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default, rename = "requestId")]
    request_id: Option<String>,
    #[serde(default, rename = "startedAt")]
    started_at: Option<String>,
    #[serde(default, rename = "completedAt")]
    completed_at: Option<String>,
    #[serde(default, rename = "durationMs")]
    duration_ms: Option<u64>,
    #[serde(default)]
    response: Option<ZcodeResponse>,
}

#[derive(Debug, Deserialize)]
struct ZcodeResponse {
    #[serde(default)]
    usage: Option<ZcodeUsage>,
    #[serde(default, rename = "providerMetadata")]
    provider_metadata: Option<ZcodeProviderMetadata>,
}

impl ZcodeResponse {
    /// 优先使用统一的 usage 字段，缺失时回退到 Anthropic 供应商口径
    fn output_tokens(&self) -> u64 {
        self.usage
            .as_ref()
            .map(ZcodeUsage::output_tokens)
            .filter(|tokens| *tokens > 0)
            .or_else(|| {
                self.provider_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.anthropic.as_ref())
                    .and_then(|anthropic| anthropic.usage.as_ref())
                    .map(ZcodeUsage::output_tokens)
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Deserialize)]
struct ZcodeProviderMetadata {
    #[serde(default)]
    anthropic: Option<ZcodeAnthropicMetadata>,
}

#[derive(Debug, Deserialize)]
struct ZcodeAnthropicMetadata {
    #[serde(default)]
    usage: Option<ZcodeUsage>,
}

#[derive(Debug, Deserialize)]
struct ZcodeUsage {
    #[serde(default, rename = "outputTokens")]
    output_tokens: u64,
    #[serde(default, rename = "output_tokens")]
    output_tokens_alt: u64,
}

impl ZcodeUsage {
    fn output_tokens(&self) -> u64 {
        self.output_tokens.max(self.output_tokens_alt)
    }
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

    fn model_io(request_id: &str, output_tokens: u64) -> String {
        line(json!({
            "type": "model_io",
            "sessionId": "sess-1",
            "requestId": request_id,
            "startedAt": "2026-10-23T02:00:00.000Z",
            "completedAt": "2026-10-23T02:00:12.000Z",
            "durationMs": 10_000,
            "response": { "usage": { "outputTokens": output_tokens } }
        }))
    }

    #[test]
    fn model_io_becomes_single_deduplicated_event() {
        let mut parser = ZcodeParser::new();
        let event = parser
            .parse_line(&model_io("req-1", 300), 0)
            .expect("model_io 应解析成功")
            .expect("应产生事件");

        assert_eq!(event.tool, ToolId::ZCODE);
        assert_eq!(event.output_tokens, 300);
        assert_eq!(event.duration_ms, 10_000);
        assert_eq!(event.at_ms, timestamp_ms("2026-10-23T02:00:12.000Z"));

        assert_eq!(
            parser
                .parse_line(&model_io("req-1", 300), 0)
                .expect("重复请求应正常忽略"),
            None
        );
    }

    #[test]
    fn missing_duration_falls_back_to_timestamps() {
        let mut parser = ZcodeParser::new();
        let record = line(json!({
            "type": "model_io",
            "requestId": "req-2",
            "startedAt": "2026-10-23T02:00:00.000Z",
            "completedAt": "2026-10-23T02:00:05.000Z",
            "response": { "usage": { "outputTokens": 60 } }
        }));

        let event = parser
            .parse_line(&record, 0)
            .expect("缺少 durationMs 应正常解析")
            .expect("应产生事件");
        assert_eq!(event.duration_ms, 5_000);
    }

    #[test]
    fn anthropic_usage_is_used_when_unified_usage_is_empty() {
        let mut parser = ZcodeParser::new();
        let record = line(json!({
            "type": "model_io",
            "requestId": "req-3",
            "startedAt": "2026-10-23T02:00:00.000Z",
            "completedAt": "2026-10-23T02:00:08.000Z",
            "durationMs": 8_000,
            "response": {
                "usage": {},
                "providerMetadata": {
                    "anthropic": { "usage": { "output_tokens": 120 } }
                }
            }
        }));

        let event = parser
            .parse_line(&record, 0)
            .expect("供应商口径应可解析")
            .expect("应产生事件");
        assert_eq!(event.output_tokens, 120);
    }

    #[test]
    fn empty_usage_and_unknown_records_are_ignored() {
        let mut parser = ZcodeParser::new();
        let empty = line(json!({
            "type": "model_io",
            "requestId": "req-empty",
            "completedAt": "2026-10-23T02:00:08.000Z",
            "response": { "usage": {} }
        }));
        assert_eq!(
            parser.parse_line(&empty, 0).expect("空用量应正常忽略"),
            None
        );

        let other = line(json!({ "type": "other", "requestId": "req-other" }));
        assert_eq!(
            parser.parse_line(&other, 0).expect("其他记录应正常忽略"),
            None
        );
        assert!(parser.parse_line("not-json", 0).is_err());
    }

    #[test]
    fn events_before_cutoff_are_not_emitted() {
        let mut parser = ZcodeParser::new();
        let cutoff = timestamp_ms("2026-10-23T03:00:00.000Z");
        assert_eq!(
            parser
                .parse_line(&model_io("req-old", 10), cutoff)
                .expect("旧事件应正常解析"),
            None
        );
    }
}
