/**
 * @Author: Danson zheng
 * @Date: 2026-10-23
 * @Description: 按本地自然日聚合 token 用量。统计窗口为包含今天在内的 N 个自然日；
 * 非法时间戳会跳过并记录告警，避免污染统计结果。
 */

use crate::models::{DailyUsage, SessionStatus, TimeRangeStats, ToolSession, ToolUsage};
use chrono::{DateTime, Duration, FixedOffset, Local, NaiveDate, Utc};
use std::collections::{BTreeMap, HashMap};

/// 单次查询允许的最大自然日数，避免异常参数触发超大内存分配
const MAX_RANGE_DAYS: u32 = 3660;

#[derive(Debug, Clone, Copy)]
pub struct Aggregator {
    /// 仅在测试或固定时区部署时使用；None 表示跟随系统本地时区
    fixed_offset: Option<FixedOffset>,
}

impl Default for Aggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl Aggregator {
    pub fn new() -> Self {
        Self { fixed_offset: None }
    }

    /// 使用指定时区偏移，便于测试与固定时区部署；不调用时跟随系统本地时区
    pub fn with_offset(offset: FixedOffset) -> Self {
        Self {
            fixed_offset: Some(offset),
        }
    }

    pub fn compute_today_stats(&self, sessions: &[ToolSession]) -> TimeRangeStats {
        self.compute_range_stats(sessions, 1)
    }

    pub fn compute_7day_stats(&self, sessions: &[ToolSession]) -> TimeRangeStats {
        self.compute_range_stats(sessions, 7)
    }

    pub fn compute_30day_stats(&self, sessions: &[ToolSession]) -> TimeRangeStats {
        self.compute_range_stats(sessions, 30)
    }

    /// days 为包含今天在内的自然日数量；0 按 1 天处理，超过 3660 天按上限截断
    pub fn compute_range_stats(&self, sessions: &[ToolSession], days: u32) -> TimeRangeStats {
        self.stats_for_days(sessions, days, self.today())
    }

    /// 返回按日期升序、缺失日期补零的每日总量；days 为 0 时返回空列表
    pub fn compute_daily_breakdown(&self, sessions: &[ToolSession], days: u32) -> Vec<DailyUsage> {
        self.daily_breakdown_for(sessions, days, self.today())
    }

    /// 按总量降序排列，总量相同时按工具名升序，保证输出稳定
    pub fn compute_tool_breakdown(&self, sessions: &[ToolSession]) -> Vec<ToolUsage> {
        let mut tool_map: HashMap<String, ToolUsage> = HashMap::new();

        for session in sessions {
            let tool_name = if session.display_name.trim().is_empty() {
                "未知工具".to_string()
            } else {
                session.display_name.trim().to_string()
            };

            let entry = tool_map.entry(tool_name.clone()).or_insert_with(|| ToolUsage {
                tool_name,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            });

            entry.input_tokens += session.input_tokens;
            entry.output_tokens += session.output_tokens;
            entry.total_tokens += session.total_tokens;
        }

        let mut result: Vec<ToolUsage> = tool_map.into_values().collect();
        result.sort_by(|a, b| {
            b.total_tokens
                .cmp(&a.total_tokens)
                .then_with(|| a.tool_name.cmp(&b.tool_name))
        });
        result
    }

    /// 仅统计正在运行且速率有效的会话；无有效样本时返回 None
    pub fn compute_active_rate(&self, sessions: &[ToolSession]) -> Option<f64> {
        let mut total = 0.0;
        let mut count = 0u32;

        for session in sessions {
            if session.status == SessionStatus::Running
                && session.token_rate.is_finite()
                && session.token_rate > 0.0
            {
                total += session.token_rate;
                count += 1;
            }
        }

        if count == 0 {
            None
        } else {
            Some(total / f64::from(count))
        }
    }

    fn today(&self) -> NaiveDate {
        self.local_date(Utc::now())
    }

    fn local_date(&self, timestamp: DateTime<Utc>) -> NaiveDate {
        match self.fixed_offset {
            Some(offset) => timestamp.with_timezone(&offset).date_naive(),
            None => timestamp.with_timezone(&Local).date_naive(),
        }
    }

    fn clamp_days(&self, days: u32) -> u32 {
        if days > MAX_RANGE_DAYS {
            log::warn!(
                "【聚合统计】统计天数超出上限，已按上限处理 days={days} max_days={MAX_RANGE_DAYS}"
            );
            MAX_RANGE_DAYS
        } else {
            days
        }
    }

    fn stats_for_days(
        &self,
        sessions: &[ToolSession],
        days: u32,
        today: NaiveDate,
    ) -> TimeRangeStats {
        let span = i64::from(self.clamp_days(days).max(1));
        let from = today - Duration::days(span - 1);
        self.filter_and_aggregate(sessions, from, today)
    }

    fn filter_and_aggregate(
        &self,
        sessions: &[ToolSession],
        from: NaiveDate,
        to: NaiveDate,
    ) -> TimeRangeStats {
        let mut stats = TimeRangeStats {
            label: format!("{from} ~ {to}"),
            total_input: 0,
            total_output: 0,
            total_cached: 0,
            total_reasoning: 0,
            total_tokens: 0,
            session_count: 0,
            avg_rate: None,
        };

        let mut total_rate = 0.0;
        let mut rate_samples = 0u32;

        for (index, session) in sessions.iter().enumerate() {
            let Some(session_date) = self.session_date(session, index) else {
                continue;
            };

            if !(from..=to).contains(&session_date) {
                continue;
            }

            stats.total_input += session.input_tokens;
            stats.total_output += session.output_tokens;
            stats.total_cached += session.cached_tokens;
            stats.total_reasoning += session.reasoning_tokens;
            stats.total_tokens += session.total_tokens;
            stats.session_count += 1;

            if session.token_rate.is_finite() && session.token_rate > 0.0 {
                total_rate += session.token_rate;
                rate_samples += 1;
            }
        }

        if rate_samples > 0 {
            stats.avg_rate = Some(total_rate / f64::from(rate_samples));
        }

        stats
    }

    fn daily_breakdown_for(
        &self,
        sessions: &[ToolSession],
        days: u32,
        today: NaiveDate,
    ) -> Vec<DailyUsage> {
        if days == 0 {
            return Vec::new();
        }

        let days = self.clamp_days(days);
        let mut daily: BTreeMap<NaiveDate, u64> = (0..days)
            .rev()
            .map(|offset| (today - Duration::days(i64::from(offset)), 0))
            .collect();

        for (index, session) in sessions.iter().enumerate() {
            let Some(session_date) = self.session_date(session, index) else {
                continue;
            };

            if let Some(entry) = daily.get_mut(&session_date) {
                *entry += session.total_tokens;
            }
        }

        daily
            .into_iter()
            .map(|(date, total_tokens)| DailyUsage {
                date: date.format("%Y-%m-%d").to_string(),
                total_tokens,
            })
            .collect()
    }

    fn session_date(&self, session: &ToolSession, index: usize) -> Option<NaiveDate> {
        match DateTime::<Utc>::from_timestamp(session.start_time, 0) {
            Some(timestamp) => Some(self.local_date(timestamp)),
            None => {
                log::warn!(
                    "【聚合统计】会话时间戳非法，已跳过 session_index={} display_name={} start_time={}",
                    index,
                    session.display_name,
                    session.start_time
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_offset() -> FixedOffset {
        FixedOffset::east_opt(8 * 3600).expect("东八区偏移应合法")
    }

    fn aggregator() -> Aggregator {
        Aggregator::with_offset(fixed_offset())
    }

    fn local_ts(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
        fixed_offset()
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("测试时间应唯一")
            .timestamp()
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("测试日期应合法")
    }

    fn session(display_name: &str, start_time: i64, total_tokens: u64) -> ToolSession {
        ToolSession {
            display_name: display_name.to_string(),
            start_time,
            input_tokens: total_tokens,
            output_tokens: 0,
            cached_tokens: 0,
            reasoning_tokens: 0,
            total_tokens,
            token_rate: 0.0,
            status: SessionStatus::Completed,
        }
    }

    #[test]
    fn stats_window_contains_exact_number_of_calendar_days() {
        let today = date(2026, 10, 23);
        let sessions = vec![
            session("today", local_ts(2026, 10, 23, 9, 0), 1),
            session("six-days-ago", local_ts(2026, 10, 17, 23, 59), 2),
            session("seven-days-ago", local_ts(2026, 10, 16, 23, 59), 4),
            session("future", local_ts(2026, 10, 24, 0, 0), 8),
        ];

        let stats = aggregator().stats_for_days(&sessions, 7, today);

        assert_eq!(stats.session_count, 2);
        assert_eq!(stats.total_tokens, 3);
        assert_eq!(stats.label, "2026-10-17 ~ 2026-10-23");
    }

    #[test]
    fn thirty_day_window_excludes_thirtieth_day_ago() {
        let today = date(2026, 10, 23);
        let sessions = vec![
            session("29-days-ago", local_ts(2026, 9, 24, 9, 0), 16),
            session("30-days-ago", local_ts(2026, 9, 23, 9, 0), 32),
        ];

        let stats = aggregator().stats_for_days(&sessions, 30, today);

        assert_eq!(stats.session_count, 1);
        assert_eq!(stats.total_tokens, 16);
        assert_eq!(stats.label, "2026-09-24 ~ 2026-10-23");
    }

    #[test]
    fn date_attribution_uses_local_offset() {
        let today = date(2026, 10, 23);
        let sessions = vec![
            session("after-local-midnight", local_ts(2026, 10, 23, 0, 30), 10),
            session("before-local-midnight", local_ts(2026, 10, 22, 23, 30), 20),
        ];

        let stats = aggregator().stats_for_days(&sessions, 1, today);

        assert_eq!(stats.session_count, 1);
        assert_eq!(stats.total_tokens, 10);
    }

    #[test]
    fn daily_breakdown_is_chronological_and_zero_filled() {
        let today = date(2026, 1, 1);
        let sessions = vec![
            session("cross-year", local_ts(2025, 12, 30, 12, 0), 7),
            session("today", local_ts(2026, 1, 1, 12, 0), 3),
            session("invalid", i64::MAX, 99),
        ];

        let daily = aggregator().daily_breakdown_for(&sessions, 3, today);

        assert_eq!(
            daily,
            vec![
                DailyUsage {
                    date: "2025-12-30".to_string(),
                    total_tokens: 7,
                },
                DailyUsage {
                    date: "2025-12-31".to_string(),
                    total_tokens: 0,
                },
                DailyUsage {
                    date: "2026-01-01".to_string(),
                    total_tokens: 3,
                },
            ]
        );
    }

    #[test]
    fn zero_days_daily_breakdown_is_empty() {
        let daily = aggregator().daily_breakdown_for(&[], 0, date(2026, 10, 23));

        assert!(daily.is_empty());
    }

    #[test]
    fn invalid_timestamps_are_skipped() {
        let today = date(2026, 10, 23);
        let sessions = vec![
            session("valid", local_ts(2026, 10, 23, 12, 0), 5),
            session("out-of-range", i64::MAX, 100),
            session("out-of-range", i64::MIN, 200),
        ];

        let stats = aggregator().stats_for_days(&sessions, 1, today);

        assert_eq!(stats.session_count, 1);
        assert_eq!(stats.total_tokens, 5);
        assert_eq!(stats.avg_rate, None);
    }

    #[test]
    fn rate_averages_only_valid_samples() {
        let today = date(2026, 10, 23);
        let start = local_ts(2026, 10, 23, 12, 0);
        let mut sessions = Vec::new();

        for rate in [10.0, 20.0, 0.0, -5.0, f64::NAN, f64::INFINITY] {
            let mut item = session("tool", start, 0);
            item.token_rate = rate;
            sessions.push(item);
        }

        let stats = aggregator().stats_for_days(&sessions, 1, today);

        assert_eq!(stats.avg_rate, Some(15.0));
    }

    #[test]
    fn active_rate_requires_valid_running_sessions() {
        let start = local_ts(2026, 10, 23, 12, 0);
        let mut running_a = session("a", start, 0);
        running_a.status = SessionStatus::Running;
        running_a.token_rate = 2.0;

        let mut running_b = session("b", start, 0);
        running_b.status = SessionStatus::Running;
        running_b.token_rate = 4.0;

        let mut completed = session("c", start, 0);
        completed.token_rate = 100.0;

        let agg = aggregator();
        assert_eq!(
            agg.compute_active_rate(&[running_a, running_b, completed]),
            Some(3.0)
        );
        assert_eq!(agg.compute_active_rate(&[]), None);
    }

    #[test]
    fn excessive_days_are_clamped() {
        let daily = aggregator().daily_breakdown_for(&[], u32::MAX, date(2026, 10, 23));

        assert_eq!(daily.len(), MAX_RANGE_DAYS as usize);
    }

    #[test]
    fn tool_breakdown_is_sorted_and_normalizes_names() {
        let sessions = vec![
            session(" tool-a ", local_ts(2026, 10, 23, 9, 0), 10),
            session("tool-b", local_ts(2026, 10, 23, 10, 0), 30),
            session("   ", local_ts(2026, 10, 23, 11, 0), 20),
        ];

        let tools = aggregator().compute_tool_breakdown(&sessions);
        let summary: Vec<(&str, u64)> = tools
            .iter()
            .map(|tool| (tool.tool_name.as_str(), tool.total_tokens))
            .collect();

        assert_eq!(
            summary,
            vec![("tool-b", 30), ("未知工具", 20), ("tool-a", 10)]
        );
    }

    #[test]
    fn zero_days_stats_falls_back_to_today() {
        let today = date(2026, 10, 23);
        let sessions = vec![
            session("today", local_ts(2026, 10, 23, 12, 0), 5),
            session("yesterday", local_ts(2026, 10, 22, 12, 0), 7),
        ];

        let stats = aggregator().stats_for_days(&sessions, 0, today);

        assert_eq!(stats.session_count, 1);
        assert_eq!(stats.total_tokens, 5);
    }
}
