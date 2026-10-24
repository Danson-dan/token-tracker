/**
 * @Author: Danson zheng
 * @Date: 2026-10-23
 * @Description: 定义 token 会话与聚合结果模型；时间戳统一为 Unix 秒，速率单位为 token/秒。
 */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSession {
    pub display_name: String,
    /// Unix 秒
    pub start_time: i64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    /// 数据源提供的总 token，作为权威值；分项与总量可能存在口径重叠
    pub total_tokens: u64,
    /// token/秒
    pub token_rate: f64,
    pub status: SessionStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimeRangeStats {
    pub label: String,
    pub total_input: u64,
    pub total_output: u64,
    pub total_cached: u64,
    pub total_reasoning: u64,
    pub total_tokens: u64,
    pub session_count: u64,
    /// 仅统计 token_rate > 0 的有效样本；无样本时为 None
    pub avg_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyUsage {
    /// 本地日期，格式 YYYY-MM-DD
    pub date: String,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUsage {
    pub tool_name: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}
