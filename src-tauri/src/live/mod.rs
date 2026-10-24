/**
 * @Author: Danson zheng
 * @Date: 2026-10-24
 * @Description: 实时 token 速度指标引擎。按工具聚合带生成耗时的增量输出事件，
 * 计算单任务均速、并发吞吐、今日/7 天/30 天累计、活跃任务数、30 分钟分钟序列与峰值。
 */

use crate::tools::ToolId;
use chrono::{DateTime, Days, FixedOffset, Local, NaiveDate, TimeZone, Utc};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// 当前速度窗口：最近 60 秒
pub const CURRENT_WINDOW_MS: i64 = 60_000;
/// 五分钟平均速度窗口
pub const FIVE_MINUTE_WINDOW_MS: i64 = 300_000;
/// 活跃任务统计窗口；只有近期产生过事件的会话才计入任务数
pub const ACTIVE_TASK_WINDOW_MS: i64 = FIVE_MINUTE_WINDOW_MS;
/// 图表分钟桶数量；最后一桶是当前分钟，可能只包含部分时间
pub const CHART_MINUTES: usize = 30;
/// 图表窗口跨度，与 CHART_MINUTES 保持同源，供采集层回填使用
pub const CHART_WINDOW_MS: i64 = CHART_MINUTES as i64 * MINUTE_MS;
const MINUTE_MS: i64 = 60_000;
const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
/// 30 天窗口包含今天在内的自然日数量
pub const THIRTY_DAY_SPAN: u64 = 30;
const SEVEN_DAY_SPAN: u64 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageEvent {
    pub tool: ToolId,
    /// 同一会话文件的稳定标识，用于统计并发任务；由采集层写入
    pub task_id: u64,
    /// 事件完成时间，Unix 毫秒
    pub at_ms: i64,
    /// 本次事件新增的输出 token，不是累计值
    pub output_tokens: u64,
    /// 产生这些 token 的生成耗时，必须大于 0
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSnapshot {
    pub tool: ToolId,
    pub display_name: String,
    pub short_name: String,
    pub accent: String,
    /// 是否已有可靠的日志适配器；false 时界面显示待适配
    pub supported: bool,
    /// 最近 60 秒完成事件的 token 总量 / 单任务生成耗时总量
    pub current_rate: f64,
    /// 最近 60 秒完成事件的 token 总量 / 并发活跃墙钟时间
    pub current_concurrent_rate: f64,
    /// 最近一条事件的 token / 生成耗时
    pub latest_rate: f64,
    /// 最近 5 分钟单任务均速
    pub avg_5m: f64,
    /// 最近 5 分钟并发吞吐
    pub avg_5m_concurrent: f64,
    /// 今日单任务均速
    pub avg_today: f64,
    /// 今日并发吞吐
    pub avg_today_concurrent: f64,
    /// 今日输出 token
    pub output_today: u64,
    /// 近 7 个自然日单任务均速
    pub avg_7d: f64,
    /// 近 7 个自然日并发吞吐
    pub avg_7d_concurrent: f64,
    /// 近 7 个自然日输出 token
    pub output_7d: u64,
    /// 近 30 个自然日单任务均速
    pub avg_30d: f64,
    /// 近 30 个自然日并发吞吐
    pub avg_30d_concurrent: f64,
    /// 近 30 个自然日输出 token
    pub output_30d: u64,
    /// 今日产生过事件的去重会话数
    pub tasks_today: u32,
    /// 近 7 个自然日产生过事件的去重会话数
    pub tasks_7d: u32,
    /// 近 30 个自然日产生过事件的去重会话数
    pub tasks_30d: u32,
    /// 最近 5 分钟产生过事件的会话数
    pub active_tasks: u32,
    /// 最近一条事件的完成时间；无事件时为 None
    pub last_seen_ms: Option<i64>,
    /// 30 分钟分钟序列的最大值（单任务口径）
    pub peak_rate_30m: f64,
    /// 旧到新，长度固定为 CHART_MINUTES
    pub minute_rates: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSnapshot {
    pub as_of_ms: i64,
    pub tools: Vec<ToolSnapshot>,
    /// 当前正在运行的智能体，界面只展示这些卡片
    pub running_tools: Vec<ToolId>,
    /// 本机检测到已安装或使用过的智能体
    pub installed_tools: Vec<ToolId>,
}

impl LiveSnapshot {
    pub fn tool(&self, tool: ToolId) -> Option<&ToolSnapshot> {
        self.tools.iter().find(|snapshot| snapshot.tool == tool)
    }
}

#[derive(Debug, Clone)]
pub struct LiveMetricsEngine {
    fixed_offset: Option<FixedOffset>,
    /// 每个工具按时间升序保存事件；至少保留 30 天窗口需要的数据
    events: HashMap<ToolId, Vec<UsageEvent>>,
}

impl Default for LiveMetricsEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveMetricsEngine {
    pub fn new() -> Self {
        Self {
            fixed_offset: None,
            events: HashMap::new(),
        }
    }

    /// 使用指定时区偏移，便于测试与固定时区部署；不调用时跟随系统本地时区
    pub fn with_offset(offset: FixedOffset) -> Self {
        Self {
            fixed_offset: Some(offset),
            events: HashMap::new(),
        }
    }

    pub fn ingest(&mut self, event: UsageEvent) {
        let valid_timestamp =
            event.at_ms >= 0 && DateTime::<Utc>::from_timestamp_millis(event.at_ms).is_some();
        if !valid_timestamp || event.output_tokens == 0 || event.duration_ms == 0 {
            log::warn!(
                "【实时指标】事件无效，已跳过 tool={} at_ms={} output_tokens={} duration_ms={}",
                event.tool.display_name(),
                event.at_ms,
                event.output_tokens,
                event.duration_ms
            );
            return;
        }

        let events = self.events.entry(event.tool).or_default();
        let index = events.partition_point(|existing| existing.at_ms <= event.at_ms);
        events.insert(index, event);
    }

    pub fn ingest_all<I>(&mut self, events: I)
    where
        I: IntoIterator<Item = UsageEvent>,
    {
        for event in events {
            self.ingest(event);
        }
    }

    pub fn snapshot_now(&mut self) -> LiveSnapshot {
        self.snapshot_at(Utc::now().timestamp_millis())
    }

    pub fn snapshot_at(&mut self, now_ms: i64) -> LiveSnapshot {
        let Some(today) = self.local_date(now_ms) else {
            log::error!("【实时指标】快照时间戳非法，返回空快照 now_ms={now_ms}");
            return LiveSnapshot {
                as_of_ms: now_ms,
                tools: Vec::new(),
                running_tools: Vec::new(),
                installed_tools: Vec::new(),
            };
        };

        let day_start_ms = match self.local_day_start_ms(today) {
            Some(start) => start,
            None => {
                log::warn!(
                    "【实时指标】无法确定本地日起点，今日统计退化为最近 24 小时 date={today}"
                );
                now_ms.saturating_sub(DAY_MS)
            }
        };
        let seven_day_start_ms = self
            .period_start_ms(today, SEVEN_DAY_SPAN)
            .unwrap_or(day_start_ms);
        let thirty_day_start_ms = self
            .period_start_ms(today, THIRTY_DAY_SPAN)
            .unwrap_or(day_start_ms);

        let current_minute_start_ms = now_ms.div_euclid(MINUTE_MS) * MINUTE_MS;
        let chart_start_ms = current_minute_start_ms - (CHART_MINUTES as i64 - 1) * MINUTE_MS;
        let current_start_ms = now_ms.saturating_sub(CURRENT_WINDOW_MS);
        let five_minute_start_ms = now_ms.saturating_sub(FIVE_MINUTE_WINDOW_MS);
        let active_task_start_ms = now_ms.saturating_sub(ACTIVE_TASK_WINDOW_MS);
        let retention_start_ms = thirty_day_start_ms.min(chart_start_ms);

        let mut tools = Vec::with_capacity(ToolId::ALL.len());

        for tool in ToolId::ALL {
            let events = self.events.entry(tool).or_default();
            let cut = events.partition_point(|event| event.at_ms < retention_start_ms);
            if cut > 0 {
                events.drain(..cut);
            }

            tools.push(build_tool_snapshot(
                tool,
                events,
                now_ms,
                day_start_ms,
                seven_day_start_ms,
                thirty_day_start_ms,
                current_start_ms,
                five_minute_start_ms,
                active_task_start_ms,
                chart_start_ms,
            ));
        }

        LiveSnapshot {
            as_of_ms: now_ms,
            tools,
            running_tools: Vec::new(),
            installed_tools: Vec::new(),
        }
    }

    fn local_date(&self, timestamp_ms: i64) -> Option<NaiveDate> {
        let timestamp = DateTime::<Utc>::from_timestamp_millis(timestamp_ms)?;
        Some(match self.fixed_offset {
            Some(offset) => timestamp.with_timezone(&offset).date_naive(),
            None => timestamp.with_timezone(&Local).date_naive(),
        })
    }

    fn local_day_start_ms(&self, date: NaiveDate) -> Option<i64> {
        let midnight = date.and_hms_opt(0, 0, 0)?;
        match self.fixed_offset {
            Some(offset) => offset
                .from_local_datetime(&midnight)
                .earliest()
                .map(|timestamp| timestamp.timestamp_millis()),
            None => Local
                .from_local_datetime(&midnight)
                .earliest()
                .map(|timestamp| timestamp.timestamp_millis()),
        }
    }

    /// 返回包含今天在内、跨度为 span_days 的自然日起点
    fn period_start_ms(&self, today: NaiveDate, span_days: u64) -> Option<i64> {
        let offset_days = span_days.checked_sub(1)?;
        let start_date = today.checked_sub_days(Days::new(offset_days))?;
        self.local_day_start_ms(start_date)
    }
}

#[derive(Default)]
struct PeriodAccumulator {
    tokens: u64,
    duration_ms: u64,
    /// 已完成事件的生成区间，用于合并重叠并计算并发吞吐
    intervals: Vec<(i64, i64)>,
    /// 周期内出现过的会话标识，用于统计任务数
    tasks: HashSet<u64>,
}

impl PeriodAccumulator {
    fn add(&mut self, event: &UsageEvent, window_start_ms: i64) {
        self.tokens = self.tokens.saturating_add(event.output_tokens);
        self.duration_ms = self.duration_ms.saturating_add(event.duration_ms);
        self.tasks.insert(event.task_id);

        let start_ms = event
            .at_ms
            .saturating_sub(i64::try_from(event.duration_ms).unwrap_or(i64::MAX))
            .max(window_start_ms);
        if event.at_ms > start_ms {
            self.intervals.push((start_ms, event.at_ms));
        }
    }

    fn task_count(&self) -> u32 {
        self.tasks.len() as u32
    }

    fn single_rate(&self) -> f64 {
        tokens_per_second(self.tokens, self.duration_ms)
    }

    fn concurrent_rate(&self) -> f64 {
        let merged_ms = merged_duration_ms(&mut self.intervals.clone());
        tokens_per_second(self.tokens, merged_ms)
    }
}

fn merged_duration_ms(intervals: &mut [(i64, i64)]) -> u64 {
    if intervals.is_empty() {
        return 0;
    }
    intervals.sort_unstable_by_key(|(start, _)| *start);

    let mut total_ms = 0u64;
    let (mut current_start, mut current_end) = intervals[0];
    for &(start, end) in intervals.iter().skip(1) {
        if start <= current_end {
            current_end = current_end.max(end);
        } else {
            total_ms = total_ms.saturating_add((current_end - current_start) as u64);
            current_start = start;
            current_end = end;
        }
    }
    total_ms.saturating_add((current_end - current_start) as u64)
}

#[allow(clippy::too_many_arguments)]
fn build_tool_snapshot(
    tool: ToolId,
    events: &[UsageEvent],
    now_ms: i64,
    day_start_ms: i64,
    seven_day_start_ms: i64,
    thirty_day_start_ms: i64,
    current_start_ms: i64,
    five_minute_start_ms: i64,
    active_task_start_ms: i64,
    chart_start_ms: i64,
) -> ToolSnapshot {
    let mut current = PeriodAccumulator::default();
    let mut five_minute = PeriodAccumulator::default();
    let mut today = PeriodAccumulator::default();
    let mut seven_day = PeriodAccumulator::default();
    let mut thirty_day = PeriodAccumulator::default();
    let mut active_tasks = HashSet::new();
    let mut latest: Option<UsageEvent> = None;
    let mut buckets = vec![(0u64, 0u64); CHART_MINUTES];

    for event in events.iter().filter(|event| event.at_ms <= now_ms) {
        latest = Some(*event);

        if event.at_ms >= current_start_ms {
            current.add(event, current_start_ms);
        }

        if event.at_ms >= five_minute_start_ms {
            five_minute.add(event, five_minute_start_ms);
        }

        if event.at_ms >= active_task_start_ms {
            active_tasks.insert(event.task_id);
        }

        if event.at_ms >= day_start_ms {
            today.add(event, day_start_ms);
        }

        if event.at_ms >= seven_day_start_ms {
            seven_day.add(event, seven_day_start_ms);
        }

        if event.at_ms >= thirty_day_start_ms {
            thirty_day.add(event, thirty_day_start_ms);
        }

        if event.at_ms >= chart_start_ms {
            let bucket_index = ((event.at_ms - chart_start_ms) / MINUTE_MS) as usize;
            if bucket_index < CHART_MINUTES {
                let bucket = &mut buckets[bucket_index];
                bucket.0 = bucket.0.saturating_add(event.output_tokens);
                bucket.1 = bucket.1.saturating_add(event.duration_ms);
            }
        }
    }

    let minute_rates: Vec<f64> = buckets
        .iter()
        .map(|(tokens, duration_ms)| tokens_per_second(*tokens, *duration_ms))
        .collect();
    let peak_rate_30m = minute_rates.iter().copied().fold(0.0, f64::max);
    let descriptor = tool.descriptor();

    ToolSnapshot {
        tool,
        display_name: descriptor.display_name.to_string(),
        short_name: descriptor.short_name.to_string(),
        accent: descriptor.accent.to_string(),
        supported: tool.is_supported(),
        current_rate: current.single_rate(),
        current_concurrent_rate: current.concurrent_rate(),
        latest_rate: latest
            .map(|event| tokens_per_second(event.output_tokens, event.duration_ms))
            .unwrap_or(0.0),
        avg_5m: five_minute.single_rate(),
        avg_5m_concurrent: five_minute.concurrent_rate(),
        avg_today: today.single_rate(),
        avg_today_concurrent: today.concurrent_rate(),
        output_today: today.tokens,
        avg_7d: seven_day.single_rate(),
        avg_7d_concurrent: seven_day.concurrent_rate(),
        output_7d: seven_day.tokens,
        avg_30d: thirty_day.single_rate(),
        avg_30d_concurrent: thirty_day.concurrent_rate(),
        output_30d: thirty_day.tokens,
        tasks_today: today.task_count(),
        tasks_7d: seven_day.task_count(),
        tasks_30d: thirty_day.task_count(),
        active_tasks: active_tasks.len() as u32,
        last_seen_ms: latest.map(|event| event.at_ms),
        peak_rate_30m,
        minute_rates,
    }
}

fn tokens_per_second(tokens: u64, duration_ms: u64) -> f64 {
    if duration_ms == 0 {
        0.0
    } else {
        tokens as f64 * 1_000.0 / duration_ms as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_offset() -> FixedOffset {
        FixedOffset::east_opt(8 * 3600).expect("东八区偏移应合法")
    }

    fn engine() -> LiveMetricsEngine {
        LiveMetricsEngine::with_offset(fixed_offset())
    }

    fn local_ms(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
        fixed_offset()
            .with_ymd_and_hms(year, month, day, hour, minute, second)
            .single()
            .expect("测试时间应唯一")
            .timestamp_millis()
    }

    fn usage_in_task(
        tool: ToolId,
        task_id: u64,
        at_ms: i64,
        output_tokens: u64,
        duration_seconds: u64,
    ) -> UsageEvent {
        UsageEvent {
            tool,
            task_id,
            at_ms,
            output_tokens,
            duration_ms: duration_seconds * 1_000,
        }
    }

    fn usage(tool: ToolId, at_ms: i64, output_tokens: u64, duration_seconds: u64) -> UsageEvent {
        usage_in_task(tool, 1, at_ms, output_tokens, duration_seconds)
    }

    fn tool_snapshot<'a>(snapshot: &'a LiveSnapshot, tool: ToolId) -> &'a ToolSnapshot {
        snapshot.tool(tool).expect("快照应包含所有工具")
    }

    fn assert_rate(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn empty_snapshot_contains_all_tools_in_stable_order() {
        let now = local_ms(2026, 10, 23, 12, 0, 0);
        let mut engine = engine();
        let snapshot = engine.snapshot_at(now);

        assert_eq!(snapshot.as_of_ms, now);
        assert_eq!(
            snapshot
                .tools
                .iter()
                .map(|tool| tool.tool)
                .collect::<Vec<_>>(),
            ToolId::ALL
        );

        for tool in &snapshot.tools {
            assert_eq!(tool.current_rate, 0.0);
            assert_eq!(tool.current_concurrent_rate, 0.0);
            assert_eq!(tool.latest_rate, 0.0);
            assert_eq!(tool.avg_5m, 0.0);
            assert_eq!(tool.avg_5m_concurrent, 0.0);
            assert_eq!(tool.avg_today, 0.0);
            assert_eq!(tool.avg_today_concurrent, 0.0);
            assert_eq!(tool.avg_7d, 0.0);
            assert_eq!(tool.avg_30d, 0.0);
            assert_eq!(tool.output_today, 0);
            assert_eq!(tool.output_7d, 0);
            assert_eq!(tool.output_30d, 0);
            assert_eq!(tool.tasks_today, 0);
            assert_eq!(tool.tasks_7d, 0);
            assert_eq!(tool.tasks_30d, 0);
            assert_eq!(tool.active_tasks, 0);
            assert_eq!(tool.last_seen_ms, None);
            assert_eq!(tool.peak_rate_30m, 0.0);
            assert_eq!(tool.minute_rates, vec![0.0; CHART_MINUTES]);
        }

        assert!(snapshot.running_tools.is_empty());
        assert!(snapshot.installed_tools.is_empty());
    }

    #[test]
    fn current_rate_is_not_the_latest_rate() {
        let now = local_ms(2026, 10, 23, 12, 0, 0);
        let mut engine = engine();
        engine.ingest(usage(ToolId::CLAUDE_CODE, now - 50_000, 840, 10));
        engine.ingest(usage(ToolId::CLAUDE_CODE, now - 20_000, 490, 10));

        let snapshot = engine.snapshot_at(now);
        let claude = tool_snapshot(&snapshot, ToolId::CLAUDE_CODE);

        assert_rate(claude.current_rate, 66.5);
        assert_rate(claude.current_concurrent_rate, 66.5);
        assert_rate(claude.latest_rate, 49.0);
        assert_rate(claude.avg_5m, 66.5);
        assert_rate(claude.avg_today, 66.5);
        assert_eq!(claude.output_today, 1_330);
        assert_eq!(claude.active_tasks, 1);
        assert_eq!(claude.last_seen_ms, Some(now - 20_000));
    }

    #[test]
    fn current_window_includes_exactly_sixty_seconds() {
        let now = local_ms(2026, 10, 23, 12, 0, 0);
        let mut engine = engine();
        engine.ingest(usage(ToolId::CLAUDE_CODE, now - 60_000, 600, 10));
        engine.ingest(usage(ToolId::CLAUDE_CODE, now - 60_001, 600, 5));

        let snapshot = engine.snapshot_at(now);
        let claude = tool_snapshot(&snapshot, ToolId::CLAUDE_CODE);

        assert_rate(claude.current_rate, 60.0);
        assert_rate(claude.avg_5m, 80.0);
    }

    #[test]
    fn five_minute_average_excludes_older_events() {
        let now = local_ms(2026, 10, 23, 12, 0, 0);
        let mut engine = engine();
        engine.ingest(usage(ToolId::CLAUDE_CODE, now - 6 * 60_000, 600, 10));
        engine.ingest(usage(ToolId::CLAUDE_CODE, now - 4 * 60_000, 300, 10));

        let snapshot = engine.snapshot_at(now);
        let claude = tool_snapshot(&snapshot, ToolId::CLAUDE_CODE);

        assert_rate(claude.current_rate, 0.0);
        assert_rate(claude.latest_rate, 30.0);
        assert_rate(claude.avg_5m, 30.0);
        assert_rate(claude.avg_today, 45.0);
        assert_eq!(claude.output_today, 900);
        assert_eq!(claude.last_seen_ms, Some(now - 4 * 60_000));
        assert_rate(claude.peak_rate_30m, 60.0);
    }

    #[test]
    fn today_average_uses_active_generation_time() {
        let now = local_ms(2026, 10, 23, 12, 0, 0);
        let mut engine = engine();
        engine.ingest(usage(
            ToolId::CLAUDE_CODE,
            local_ms(2026, 10, 23, 0, 1, 0),
            600,
            10,
        ));

        let snapshot = engine.snapshot_at(now);
        let claude = tool_snapshot(&snapshot, ToolId::CLAUDE_CODE);

        assert_rate(claude.current_rate, 0.0);
        assert_rate(claude.latest_rate, 60.0);
        assert_rate(claude.avg_5m, 0.0);
        assert_rate(claude.avg_today, 60.0);
        assert_eq!(claude.output_today, 600);
        assert_eq!(claude.peak_rate_30m, 0.0);
    }

    #[test]
    fn minute_series_is_ordered_zero_filled_and_reports_peak() {
        let now = local_ms(2026, 10, 23, 12, 30, 30);
        let mut engine = engine();
        engine.ingest(usage(
            ToolId::CLAUDE_CODE,
            local_ms(2026, 10, 23, 12, 1, 0),
            600,
            10,
        ));
        engine.ingest(usage(
            ToolId::CLAUDE_CODE,
            local_ms(2026, 10, 23, 12, 29, 10),
            300,
            10,
        ));
        engine.ingest(usage(
            ToolId::CLAUDE_CODE,
            local_ms(2026, 10, 23, 12, 30, 5),
            1_200,
            10,
        ));

        let snapshot = engine.snapshot_at(now);
        let claude = tool_snapshot(&snapshot, ToolId::CLAUDE_CODE);

        assert_eq!(claude.minute_rates.len(), CHART_MINUTES);
        assert_rate(claude.minute_rates[0], 60.0);
        assert!(claude.minute_rates[1..28].iter().all(|rate| *rate == 0.0));
        assert_rate(claude.minute_rates[28], 30.0);
        assert_rate(claude.minute_rates[29], 120.0);
        assert_rate(claude.peak_rate_30m, 120.0);
        assert_rate(claude.current_rate, 120.0);
        assert_rate(claude.avg_5m, 75.0);
        assert_rate(claude.avg_today, 70.0);
    }

    #[test]
    fn future_events_are_excluded_until_time_advances() {
        let now = local_ms(2026, 10, 23, 12, 0, 0);
        let future = local_ms(2026, 10, 23, 12, 5, 0);
        let mut engine = engine();
        engine.ingest(usage(ToolId::CLAUDE_CODE, future, 600, 10));

        let early = engine.snapshot_at(now);
        let claude_early = tool_snapshot(&early, ToolId::CLAUDE_CODE);
        assert_eq!(claude_early.output_today, 0);
        assert_eq!(claude_early.last_seen_ms, None);
        assert_rate(claude_early.current_rate, 0.0);

        let later = engine.snapshot_at(local_ms(2026, 10, 23, 12, 10, 0));
        let claude_later = tool_snapshot(&later, ToolId::CLAUDE_CODE);
        assert_eq!(claude_later.output_today, 600);
        assert_eq!(claude_later.last_seen_ms, Some(future));
        assert_rate(claude_later.latest_rate, 60.0);
        assert_rate(claude_later.avg_5m, 60.0);
        assert_rate(claude_later.current_rate, 0.0);
    }

    #[test]
    fn out_of_order_events_are_sorted_before_aggregation() {
        let now = local_ms(2026, 10, 23, 12, 0, 0);
        let mut engine = engine();
        engine.ingest(usage(ToolId::CLAUDE_CODE, now - 10_000, 600, 10));
        engine.ingest(usage(ToolId::CLAUDE_CODE, now - 30_000, 300, 10));

        let snapshot = engine.snapshot_at(now);
        let claude = tool_snapshot(&snapshot, ToolId::CLAUDE_CODE);

        assert_rate(claude.current_rate, 45.0);
        assert_rate(claude.latest_rate, 60.0);
        assert_eq!(claude.last_seen_ms, Some(now - 10_000));
    }

    #[test]
    fn midnight_resets_today_but_keeps_chart_window() {
        let now = local_ms(2026, 10, 24, 0, 10, 0);
        let mut engine = engine();
        engine.ingest(usage(
            ToolId::CLAUDE_CODE,
            local_ms(2026, 10, 23, 23, 30, 0),
            600,
            10,
        ));
        engine.ingest(usage(
            ToolId::CLAUDE_CODE,
            local_ms(2026, 10, 23, 23, 50, 0),
            600,
            10,
        ));
        engine.ingest(usage(
            ToolId::CLAUDE_CODE,
            local_ms(2026, 10, 24, 0, 5, 0),
            300,
            10,
        ));

        let snapshot = engine.snapshot_at(now);
        let claude = tool_snapshot(&snapshot, ToolId::CLAUDE_CODE);

        assert_eq!(claude.output_today, 300);
        assert_eq!(claude.output_7d, 1_500);
        assert_rate(claude.avg_today, 30.0);
        assert_rate(claude.avg_5m, 30.0);
        assert_rate(claude.current_rate, 0.0);
        assert_rate(claude.latest_rate, 30.0);
        assert_eq!(claude.last_seen_ms, Some(local_ms(2026, 10, 24, 0, 5, 0)));
        assert_rate(claude.minute_rates[9], 60.0);
        assert_rate(claude.minute_rates[24], 30.0);
        assert_rate(claude.peak_rate_30m, 60.0);
    }

    #[test]
    fn concurrent_tasks_increase_throughput_but_not_single_task_rate() {
        let now = local_ms(2026, 10, 23, 12, 0, 0);
        let mut engine = engine();
        let at_ms = now - 10_000;
        engine.ingest(usage_in_task(ToolId::CLAUDE_CODE, 1, at_ms, 600, 10));
        engine.ingest(usage_in_task(ToolId::CLAUDE_CODE, 2, at_ms, 600, 10));

        let snapshot = engine.snapshot_at(now);
        let claude = tool_snapshot(&snapshot, ToolId::CLAUDE_CODE);

        assert_rate(claude.current_rate, 60.0);
        assert_rate(claude.current_concurrent_rate, 120.0);
        assert_rate(claude.avg_5m, 60.0);
        assert_rate(claude.avg_5m_concurrent, 120.0);
        assert_eq!(claude.active_tasks, 2);
    }

    #[test]
    fn active_tasks_exclude_stale_sessions() {
        let now = local_ms(2026, 10, 23, 12, 0, 0);
        let mut engine = engine();
        engine.ingest(usage_in_task(ToolId::CLAUDE_CODE, 1, now - 30_000, 300, 10));
        engine.ingest(usage_in_task(ToolId::CLAUDE_CODE, 2, now - 7 * 60_000, 300, 10));
        engine.ingest(usage_in_task(ToolId::CLAUDE_CODE, 1, now - 40_000, 300, 10));

        let snapshot = engine.snapshot_at(now);
        let claude = tool_snapshot(&snapshot, ToolId::CLAUDE_CODE);

        assert_eq!(claude.active_tasks, 1);
    }

    #[test]
    fn seven_and_thirty_day_totals_use_natural_days() {
        let now = local_ms(2026, 10, 23, 12, 0, 0);
        let mut engine = engine();
        engine.ingest(usage_in_task(
            ToolId::CLAUDE_CODE,
            1,
            local_ms(2026, 10, 23, 9, 0, 0),
            600,
            10,
        ));
        engine.ingest(usage_in_task(
            ToolId::CLAUDE_CODE,
            2,
            local_ms(2026, 10, 20, 9, 0, 0),
            400,
            10,
        ));
        engine.ingest(usage_in_task(
            ToolId::CLAUDE_CODE,
            3,
            local_ms(2026, 10, 3, 9, 0, 0),
            200,
            10,
        ));
        engine.ingest(usage_in_task(
            ToolId::CLAUDE_CODE,
            4,
            local_ms(2026, 9, 1, 9, 0, 0),
            9_999,
            10,
        ));

        let snapshot = engine.snapshot_at(now);
        let claude = tool_snapshot(&snapshot, ToolId::CLAUDE_CODE);

        assert_eq!(claude.output_today, 600);
        assert_eq!(claude.output_7d, 1_000);
        assert_eq!(claude.output_30d, 1_200);
        assert_eq!(claude.tasks_today, 1);
        assert_eq!(claude.tasks_7d, 2);
        assert_eq!(claude.tasks_30d, 3);
        assert_rate(claude.avg_today, 60.0);
        assert_rate(claude.avg_7d, 50.0);
        assert_rate(claude.avg_30d, 40.0);
    }

    #[test]
    fn invalid_events_are_ignored() {
        let now = local_ms(2026, 10, 23, 12, 0, 0);
        let mut engine = engine();
        engine.ingest(usage(ToolId::CLAUDE_CODE, now - 1_000, 0, 10));
        engine.ingest(usage(ToolId::CLAUDE_CODE, now - 1_000, 600, 0));
        engine.ingest(usage(ToolId::CLAUDE_CODE, -1, 600, 10));
        engine.ingest(usage(ToolId::CLAUDE_CODE, i64::MAX, 600, 10));

        let snapshot = engine.snapshot_at(now);

        assert!(engine
            .events
            .get(&ToolId::CLAUDE_CODE)
            .is_none_or(|events| events.is_empty()));
        for tool in &snapshot.tools {
            assert_eq!(tool.output_today, 0);
            assert_eq!(tool.last_seen_ms, None);
            assert_eq!(tool.minute_rates, vec![0.0; CHART_MINUTES]);
        }
    }
}
