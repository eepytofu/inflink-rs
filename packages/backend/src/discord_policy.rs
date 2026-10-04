//! Discord Activity 的写入节奏
//!
//! Discord 文档给出的上限是 20 秒 5 次更新。这里用固定的最小间隔而不是"先突发再
//! 限速": 突发用完之后, 新状态可能要等十秒左右才发得出去, 连续切歌时正好撞上;
//! 固定间隔的最坏情况始终是一个间隔, 而且 4 秒的间隔本身就不可能超过 20 秒 5 次。
//! 滑动窗口只是兜底。
//!
//! 能这么做的前提是一首歌的第一张卡片就是完整的 (文字、规格、进度一次到齐),
//! 封面和例行进度都不占写入次数, 所以不需要靠突发来补发。

use std::{
    collections::VecDeque,
    time::{
        Duration,
        Instant,
    },
};

pub const MIN_SPACING: Duration = Duration::from_secs(4);
const WINDOW: Duration = Duration::from_secs(20);
const MAX_PER_WINDOW: usize = 5;

#[derive(Debug, Default)]
pub struct SendPolicy {
    sent: VecDeque<Instant>,
}

impl SendPolicy {
    fn prune(&mut self, now: Instant) {
        while self
            .sent
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= WINDOW)
        {
            self.sent.pop_front();
        }
    }

    /// 还要等多久才能写下一次, 零表示现在就可以
    pub fn wait_for(&mut self, now: Instant) -> Duration {
        self.prune(now);

        let spacing = self.sent.back().map_or(Duration::ZERO, |last| {
            MIN_SPACING.saturating_sub(now.saturating_duration_since(*last))
        });
        let window = if self.sent.len() >= MAX_PER_WINDOW {
            self.sent.front().map_or(Duration::ZERO, |first| {
                WINDOW.saturating_sub(now.saturating_duration_since(*first))
            })
        } else {
            Duration::ZERO
        };

        spacing.max(window)
    }

    pub fn record(&mut self, now: Instant) {
        self.prune(now);
        self.sent.push_back(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_connection_writes_at_once() {
        let mut policy = SendPolicy::default();
        let t0 = Instant::now();
        assert_eq!(policy.wait_for(t0), Duration::ZERO);

        policy.record(t0);
        assert_eq!(policy.wait_for(t0 + MIN_SPACING), Duration::ZERO);
    }

    #[test]
    fn the_next_write_waits_out_the_spacing() {
        let mut policy = SendPolicy::default();
        let t0 = Instant::now();
        policy.record(t0);
        assert_eq!(
            policy.wait_for(t0 + Duration::from_millis(300)),
            Duration::from_millis(3700)
        );
    }

    #[test]
    fn spacing_alone_never_exceeds_the_documented_window() {
        let mut policy = SendPolicy::default();
        let t0 = Instant::now();
        let mut now = t0;
        let mut writes = Vec::new();

        // 一直有新状态要发: 每次都在刚好允许的时刻写
        while now.duration_since(t0) < Duration::from_secs(120) {
            now += policy.wait_for(now);
            policy.record(now);
            writes.push(now);
            now += Duration::from_millis(1);
        }

        for (i, start) in writes.iter().enumerate() {
            let in_window = writes[i..]
                .iter()
                .take_while(|t| t.duration_since(*start) < WINDOW)
                .count();
            assert!(in_window <= MAX_PER_WINDOW, "{in_window} writes in 20 s");
        }
    }

    #[test]
    fn the_window_still_holds_if_writes_arrive_faster_than_the_spacing() {
        let mut policy = SendPolicy::default();
        let t0 = Instant::now();
        // 绕过间隔直接记五次 (例如断开时的清除), 第六次必须等窗口
        for i in 0..5 {
            policy.record(t0 + Duration::from_secs(i));
        }
        assert_eq!(
            policy.wait_for(t0 + Duration::from_secs(9)),
            Duration::from_secs(11)
        );
    }
}
