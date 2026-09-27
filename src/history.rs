//! [`InputEvent`] の列を直近の一定時間だけ保持し、試行単位に区切って読み出す。
//!
//! 区切りは成否の判定ではなく表示の都合である。すべての入力が離れた状態が
//! `trial_gap_us` 続いたら、そこまでを 1 試行として固定する。何か押されている間は
//! どれだけ時間が経っても区切らない。押しっぱなしの途中で表示が切り替わると、
//! 直前の試行を読んでいる最中に画面が動く。
//!
//! 書き込み側は 250Hz、読み出し側は 60Hz を想定する。排他はここでは持たない。
//! スレッドの配線は呼び出し側の責務で、ここに `Mutex` を埋めると両方の側が
//! ロックの粒度を選べなくなる。

use std::collections::VecDeque;

use crate::timeline::{EventKind, InputEvent, Target};

/// 保持する長さの既定値。30 秒あれば直近 5 試行は残る。
pub const DEFAULT_RETAIN_US: u64 = 30_000_000;

/// 試行を区切る無入力時間の既定値。設計で決めた 300ms。
pub const DEFAULT_TRIAL_GAP_US: u64 = 300_000;

/// 読み出し用の 1 試行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trial {
    /// 最初の押下の時刻。ピアノロールの 0F になる。そのイベントが保持から外れても動かない。
    pub origin_us: u64,
    /// 確定した試行では最後のイベントの時刻、進行中の試行では最後に受け取った `now_us`。
    pub last_us: u64,
    /// この試行に属するイベント。古い順に並ぶ。保持から外れたものは入らない。
    pub events: Vec<InputEvent>,
}

/// 押されている入力の組が変わらない区間。ピアノロールの 1 列にあたる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// この状態になった時刻。
    pub at_us: u64,
    /// 次の状態に変わる時刻。最後の行では [`Trial::last_us`]。
    pub end_us: u64,
    /// この区間で押されている入力。押した順に並ぶ。空の区間も行として残す。
    pub held: Vec<Target>,
    /// この状態を作ったイベントの直前に取りこぼしがあったか。
    pub gap_before: bool,
}

/// 試行ごとに区切ったイベントの ring buffer。
///
/// 保持は件数ではなく時間で決まる。`now_us` より `retain_us` 以上古いイベントは
/// [`History::push`] のたびに落ちる。入力が来ないときも `push` を空で呼べば
/// 時刻が進み、退避と試行の確定が進む。
#[derive(Debug)]
pub struct History {
    retain_us: u64,
    trial_gap_us: u64,
    now_us: u64,
    trials: VecDeque<TrialBuffer>,
    /// いま押されている入力。押した順に並ぶ。退避では変えない。物理の状態を表すため。
    held: Vec<Target>,
    /// 取り込んだ最後のイベントの時刻。退避でイベントが消えても残す。
    last_event_us: u64,
}

/// 試行 1 件分の溜め場。
#[derive(Debug)]
struct TrialBuffer {
    /// この試行を始めた押下の時刻。退避では変えない。
    origin_us: u64,
    events: VecDeque<InputEvent>,
    finalized: bool,
}

impl History {
    /// `retain_us` は保持する長さ、`trial_gap_us` は試行を区切る無入力時間。
    /// 既定値は [`DEFAULT_RETAIN_US`] と [`DEFAULT_TRIAL_GAP_US`] にあるが、
    /// 動きを決めるのは引数だけである。
    pub fn new(retain_us: u64, trial_gap_us: u64) -> Self {
        Self {
            retain_us,
            trial_gap_us,
            now_us: 0,
            trials: VecDeque::new(),
            held: Vec::new(),
            last_event_us: 0,
        }
    }

    /// 1 レポート分のイベントを取り込み、現在時刻を `now_us` に進める。
    ///
    /// `events` が空でも呼ぶ。時刻だけが進む呼び出しで、無入力による試行の確定と
    /// 古いイベントの退避が起きる。
    pub fn push(&mut self, events: &[InputEvent], now_us: u64) {
        self.now_us = now_us;

        for event in events {
            // 1 バッチの中でも区切る。取りこぼしの後などに、長い無入力を挟んだ押下が
            // 同じバッチに入ることがある。
            if self.starts_new_trial(event) {
                self.finalize();
            }
            self.apply(*event);
        }

        if self.is_idle_at(now_us) {
            self.finalize();
        }

        self.evict();
    }

    /// 新しい順に最大 `n` 件の試行を返す。進行中の試行があれば先頭に来る。
    pub fn recent_trials(&self, n: usize) -> Vec<Trial> {
        self.trials
            .iter()
            .rev()
            .take(n)
            .map(|buffer| self.snapshot(buffer))
            .collect()
    }

    /// 最後に受け取った `now_us`。
    pub fn now_us(&self) -> u64 {
        self.now_us
    }

    /// このイベントの前で試行を区切るか。
    ///
    /// 何か押されている間は区切らない。区切るのは、すべて離れた状態が `trial_gap_us` 以上
    /// 続いた後の押下の直前だけである。
    fn starts_new_trial(&self, event: &InputEvent) -> bool {
        event.kind == EventKind::Press
            && self.has_active()
            && self.held.is_empty()
            && event.at_us.saturating_sub(self.last_event_us) >= self.trial_gap_us
    }

    /// すべて離れた状態が `trial_gap_us` 以上続いているか。
    fn is_idle_at(&self, now_us: u64) -> bool {
        self.has_active()
            && self.held.is_empty()
            && now_us.saturating_sub(self.last_event_us) >= self.trial_gap_us
    }

    fn has_active(&self) -> bool {
        self.trials.back().is_some_and(|trial| !trial.finalized)
    }

    /// イベントを押下状態に反映し、進行中の試行に足す。
    fn apply(&mut self, event: InputEvent) {
        match event.kind {
            EventKind::Press => {
                if !self.held.contains(&event.target) {
                    self.held.push(event.target);
                }
            }
            EventKind::Release => self.held.retain(|target| *target != event.target),
        }

        self.last_event_us = event.at_us;

        if !self.has_active() {
            // 進行中の試行がないときの離しは、押下が確定済みの試行に入っている離れ者である。
            // ここで試行を作ると、押下のない試行ができて origin_us が離しの時刻になる。
            if event.kind == EventKind::Release {
                return;
            }
            self.trials.push_back(TrialBuffer {
                origin_us: event.at_us,
                events: VecDeque::new(),
                finalized: false,
            });
        }
        if let Some(active) = self.trials.back_mut() {
            active.events.push_back(event);
        }
    }

    fn finalize(&mut self) {
        if let Some(active) = self.trials.back_mut() {
            active.finalized = true;
        }
    }

    /// `retain_us` より古いイベントを落とす。境界ちょうどのものは残す。
    ///
    /// 進行中の試行は、イベントが 1 件も残らなくなっても枠だけ残す。消すと押しっぱなしの
    /// 途中で試行が入れ替わり、後から来た離しが新しい試行を作る。確定した試行は、
    /// 残るイベントがなくなった時点で表示するものがないので消す。
    fn evict(&mut self) {
        let oldest_us = self.now_us.saturating_sub(self.retain_us);

        for trial in &mut self.trials {
            while trial
                .events
                .front()
                .is_some_and(|event| event.at_us < oldest_us)
            {
                trial.events.pop_front();
            }
        }
        self.trials
            .retain(|trial| !trial.finalized || !trial.events.is_empty());
    }

    fn snapshot(&self, buffer: &TrialBuffer) -> Trial {
        let events: Vec<InputEvent> = buffer.events.iter().copied().collect();
        let last_us = match events.last() {
            // 確定した試行は最後のイベントで終わる。進行中の試行は現在時刻まで伸びる。
            Some(last) if buffer.finalized => last.at_us,
            _ => self.now_us,
        };

        Trial {
            origin_us: buffer.origin_us,
            last_us,
            events,
        }
    }
}

impl Trial {
    /// イベント列を、押されている組が変わらない区間の列に畳む。
    ///
    /// 同じ時刻のイベントは 1 行にまとまる。方向を切り替えたレポートは古い方向の離しと
    /// 新しい方向の押下を同じ時刻で出すので、まとめないと長さ 0 の行が挟まる。
    pub fn rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = Vec::new();
        let mut held: Vec<Target> = Vec::new();

        for event in &self.events {
            let same_timestamp = rows.last().is_some_and(|row| row.at_us == event.at_us);

            match event.kind {
                EventKind::Press => {
                    if !held.contains(&event.target) {
                        held.push(event.target);
                    }
                }
                EventKind::Release => held.retain(|target| *target != event.target),
            }

            match rows.last_mut() {
                Some(row) if same_timestamp => row.held.clone_from(&held),
                _ => rows.push(Row {
                    at_us: event.at_us,
                    // 次の行が決まってから埋める。
                    end_us: event.at_us,
                    held: held.clone(),
                    gap_before: event.gap_before,
                }),
            }
        }

        let mut end_us = self.last_us;
        for row in rows.iter_mut().rev() {
            row.end_us = end_us;
            end_us = row.at_us;
        }

        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report_decode::{Buttons, Direction};

    /// 1F。
    const FRAME_US: u64 = 16_667;

    /// テストで使う区切り時間。
    const GAP_US: u64 = DEFAULT_TRIAL_GAP_US;

    fn history() -> History {
        History::new(DEFAULT_RETAIN_US, GAP_US)
    }

    fn press(target: Target, at_us: u64) -> InputEvent {
        InputEvent {
            kind: EventKind::Press,
            target,
            at_us,
            gap_before: false,
        }
    }

    fn release(target: Target, at_us: u64) -> InputEvent {
        InputEvent {
            kind: EventKind::Release,
            ..press(target, at_us)
        }
    }

    fn button(button: Buttons) -> Target {
        Target::Button(button)
    }

    /// 押下と離しを 1 件ずつ、時刻を指定して流す。離した時刻を返す。
    fn tap(history: &mut History, target: Target, at_us: u64, hold_us: u64) -> u64 {
        history.push(&[press(target, at_us)], at_us);
        history.push(&[release(target, at_us + hold_us)], at_us + hold_us);
        at_us + hold_us
    }

    #[test]
    fn empty_push_advances_now_and_finalizes_after_the_gap() {
        let mut history = history();
        let released_us = tap(&mut history, button(Buttons::L1), 0, FRAME_US);

        // 区切り時間に 1µs 足りない間は進行中のまま。進行中の試行は now_us まで伸びる。
        history.push(&[], released_us + GAP_US - 1);
        assert_eq!(
            history.recent_trials(1)[0].last_us,
            released_us + GAP_US - 1
        );

        history.push(&[], released_us + GAP_US);
        // 確定した試行は最後のイベントで終わる。以後 now_us が進んでも伸びない。
        assert_eq!(history.recent_trials(1)[0].last_us, released_us);

        history.push(&[], released_us + 10 * GAP_US);
        assert_eq!(history.recent_trials(1)[0].last_us, released_us);
        assert_eq!(history.now_us(), released_us + 10 * GAP_US);
    }

    /// 押しっぱなしの間はどれだけ時間が経っても区切らない。
    #[test]
    fn held_target_blocks_the_split_forever() {
        let mut history = history();
        history.push(&[press(button(Buttons::L1), 0)], 0);

        for step in 1..=50u64 {
            let now_us = step * GAP_US;
            history.push(&[], now_us);
            let trials = history.recent_trials(5);
            assert_eq!(trials.len(), 1, "{step} 回目");
            // 進行中なので末尾が now_us に追従する。
            assert_eq!(trials[0].last_us, now_us, "{step} 回目");
        }

        // 離して初めて区切れる。
        let released_us = 50 * GAP_US + FRAME_US;
        history.push(&[release(button(Buttons::L1), released_us)], released_us);
        history.push(&[], released_us + GAP_US);

        assert_eq!(history.recent_trials(1)[0].last_us, released_us);
    }

    /// 1 つでも押されたままなら、他の入力が離れても区切らない。
    #[test]
    fn partial_release_does_not_split() {
        let mut history = history();
        history.push(&[press(button(Buttons::L1), 0)], 0);
        history.push(&[press(button(Buttons::R1), FRAME_US)], FRAME_US);
        history.push(&[release(button(Buttons::R1), 2 * FRAME_US)], 2 * FRAME_US);

        let now_us = 2 * FRAME_US + 5 * GAP_US;
        history.push(&[], now_us);

        let trials = history.recent_trials(5);
        assert_eq!(trials.len(), 1);
        assert_eq!(trials[0].last_us, now_us);
    }

    /// 同じバッチの中に長い無入力を挟んだ押下が入っていたら、その押下の前で切る。
    #[test]
    fn splits_inside_one_batch_before_a_late_press() {
        let mut history = history();
        let late_us = 2 * GAP_US;
        history.push(
            &[
                press(button(Buttons::L1), 0),
                release(button(Buttons::L1), FRAME_US),
                press(button(Buttons::R1), late_us),
            ],
            late_us,
        );

        let trials = history.recent_trials(5);
        assert_eq!(trials.len(), 2);
        // 新しい順に並ぶ。進行中の試行が先頭に来る。
        assert_eq!(trials[0].origin_us, late_us);
        assert_eq!(trials[0].events.len(), 1);
        assert_eq!(trials[1].last_us, FRAME_US);

        // 先頭だけが now_us に追従する。後ろは確定している。
        history.push(&[], late_us + FRAME_US);
        let trials = history.recent_trials(5);
        assert_eq!(trials[0].last_us, late_us + FRAME_US);
        assert_eq!(trials[1].last_us, FRAME_US);
    }

    /// 区切りの閾値は「以上」で効く。ちょうど `trial_gap_us` で切れる。
    #[test]
    fn splits_at_exactly_the_gap_and_not_one_microsecond_earlier() {
        for (gap_us, expected) in [(GAP_US - 1, 1), (GAP_US, 2)] {
            let mut history = history();
            let released_us = tap(&mut history, button(Buttons::L1), 0, FRAME_US);
            let next_us = released_us + gap_us;
            history.push(&[press(button(Buttons::R1), next_us)], next_us);

            assert_eq!(history.recent_trials(5).len(), expected, "{gap_us}µs");
        }
    }

    /// 長く押しっぱなしにした後の離しは、その進行中の試行に入る。
    /// 区切り判定は押下だけを見るため、離しは試行を作らない。
    #[test]
    fn a_release_after_a_long_hold_stays_in_the_same_trial() {
        let mut history = history();
        history.push(&[press(button(Buttons::L1), 0)], 0);
        let late_us = 5 * GAP_US;
        history.push(&[release(button(Buttons::L1), late_us)], late_us);

        let trials = history.recent_trials(5);
        assert_eq!(trials.len(), 1);
        assert_eq!(trials[0].origin_us, 0);
        assert_eq!(trials[0].events.len(), 2);
    }

    /// 進行中の試行がないときに来た離しは、試行を作らない。
    /// 作ると押下のない試行ができて、起点が離しの時刻になる。
    #[test]
    fn an_orphan_release_does_not_start_a_trial() {
        let mut history = history();
        history.push(&[release(button(Buttons::L1), 0)], 0);
        assert!(history.recent_trials(5).is_empty());

        // 確定済みの試行しかない状態でも同じ。
        let start_us = 10 * GAP_US;
        let released_us = tap(&mut history, button(Buttons::CROSS), start_us, FRAME_US);
        history.push(&[], released_us + GAP_US);
        let orphan_us = released_us + 2 * GAP_US;
        history.push(&[release(button(Buttons::L1), orphan_us)], orphan_us);

        let trials = history.recent_trials(5);
        assert_eq!(trials.len(), 1);
        assert_eq!(trials[0].origin_us, start_us);
        assert_eq!(trials[0].events.len(), 2);
    }

    #[test]
    fn recent_trials_returns_newest_first_and_caps_at_n() {
        let mut history = history();
        let mut at_us = 0;
        for _ in 0..4 {
            at_us = tap(&mut history, button(Buttons::CROSS), at_us, FRAME_US) + 2 * GAP_US;
        }
        // 最後の tap の後も 2 * GAP_US 進んでいるので、4 件すべて確定している。
        history.push(&[], at_us);

        let trials = history.recent_trials(2);
        assert_eq!(trials.len(), 2);
        assert!(trials[0].origin_us > trials[1].origin_us);
        assert_eq!(history.recent_trials(10).len(), 4);
        assert!(history.recent_trials(0).is_empty());
    }

    /// 進行中の試行は先頭に来て、確定済みがその後ろに続く。
    #[test]
    fn active_trial_comes_first() {
        let mut history = history();
        let released_us = tap(&mut history, button(Buttons::L1), 0, FRAME_US);
        let next_us = released_us + 2 * GAP_US;
        history.push(&[press(button(Buttons::R1), next_us)], next_us);

        let trials = history.recent_trials(5);
        // 先頭は進行中なので now_us、後ろは確定して最後のイベントで止まる。
        assert_eq!(trials[0].last_us, next_us);
        assert_eq!(trials[1].last_us, released_us);
    }

    /// 起点は最初の押下の時刻。
    #[test]
    fn origin_us_is_the_first_press() {
        let mut history = history();
        let start_us = 12_345;
        history.push(&[press(button(Buttons::L1), start_us)], start_us);
        history.push(
            &[press(button(Buttons::R1), start_us + FRAME_US)],
            start_us + FRAME_US,
        );

        assert_eq!(history.recent_trials(1)[0].origin_us, start_us);
    }

    /// 同じ時刻のイベントは 1 行に畳む。方向の切り替えで長さ 0 の行を作らない。
    #[test]
    fn folds_events_that_share_a_timestamp_into_one_row() {
        let mut history = history();
        history.push(&[press(Target::Dpad(Direction::N), 0)], 0);
        let change_us = 4 * FRAME_US;
        history.push(
            &[
                release(Target::Dpad(Direction::N), change_us),
                press(Target::Dpad(Direction::E), change_us),
            ],
            change_us,
        );
        let now_us = 5 * FRAME_US;
        history.push(&[], now_us);

        let rows = history.recent_trials(1)[0].rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].held, vec![Target::Dpad(Direction::N)]);
        assert_eq!(rows[0].at_us, 0);
        assert_eq!(rows[0].end_us, change_us);
        assert_eq!(rows[1].held, vec![Target::Dpad(Direction::E)]);
        assert_eq!(rows[1].at_us, change_us);
        // 進行中の試行の最後の行は現在時刻まで伸びる。
        assert_eq!(rows[1].end_us, now_us);
    }

    /// 同時押しは 1 行になる。両方が押されている行が 1 つだけ立つ。
    #[test]
    fn simultaneous_presses_make_one_row_holding_both() {
        let mut history = history();
        history.push(
            &[press(button(Buttons::L1), 0), press(button(Buttons::R1), 0)],
            0,
        );
        history.push(&[], FRAME_US);

        let rows = history.recent_trials(1)[0].rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].held, vec![button(Buttons::L1), button(Buttons::R1)]);
        assert_eq!(rows[0].at_us, 0);
        assert_eq!(rows[0].end_us, FRAME_US);
    }

    /// 入力の合間の空の行を残す。ピアノロールの隙間はここから描く。
    #[test]
    fn keeps_empty_rows_between_inputs() {
        let mut history = history();
        let released_us = tap(&mut history, button(Buttons::L1), 0, FRAME_US);
        // 区切り時間には足りない間隔で次を押す。同じ試行に入る。
        let next_us = released_us + GAP_US / 2;
        let last_us = tap(&mut history, button(Buttons::R1), next_us, FRAME_US);
        history.push(&[], last_us + GAP_US);

        let rows = history.recent_trials(1)[0].rows();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].held, vec![button(Buttons::L1)]);
        assert!(rows[1].held.is_empty());
        assert_eq!(rows[1].at_us, released_us);
        assert_eq!(rows[1].end_us, next_us);
        assert_eq!(rows[2].held, vec![button(Buttons::R1)]);
        assert!(rows[3].held.is_empty());
        // 確定した試行の最後の行は最後のイベントで終わる。
        assert_eq!(rows[3].at_us, last_us);
        assert_eq!(rows[3].end_us, last_us);
    }

    /// 行の終わりは次の行の始まりに繋がる。
    #[test]
    fn row_end_chains_to_the_next_row_start() {
        let mut history = history();
        history.push(&[press(button(Buttons::L1), 0)], 0);
        history.push(&[press(button(Buttons::R1), FRAME_US)], FRAME_US);
        history.push(&[release(button(Buttons::L1), 2 * FRAME_US)], 2 * FRAME_US);
        history.push(&[], 3 * FRAME_US);

        let rows = history.recent_trials(1)[0].rows();
        for pair in rows.windows(2) {
            assert_eq!(pair[0].end_us, pair[1].at_us);
        }
        assert_eq!(rows.last().map(|row| row.end_us), Some(3 * FRAME_US));
    }

    /// 押されている入力は押した順に並ぶ。離して押し直したものは末尾に回る。
    #[test]
    fn held_order_follows_press_order() {
        let mut history = history();
        history.push(
            &[
                press(button(Buttons::L1), 0),
                press(button(Buttons::R1), 0),
                press(Target::Stick(Direction::N), 0),
            ],
            0,
        );
        history.push(
            &[
                release(button(Buttons::L1), FRAME_US),
                press(button(Buttons::L1), FRAME_US),
            ],
            FRAME_US,
        );
        history.push(&[], 2 * FRAME_US);

        let rows = history.recent_trials(1)[0].rows();
        assert_eq!(
            rows[0].held,
            vec![
                button(Buttons::L1),
                button(Buttons::R1),
                Target::Stick(Direction::N),
            ]
        );
        assert_eq!(
            rows[1].held,
            vec![
                button(Buttons::R1),
                Target::Stick(Direction::N),
                button(Buttons::L1),
            ]
        );
    }

    /// 取りこぼしの印は、その時刻の最初のイベントから引き継ぐ。
    #[test]
    fn carries_gap_before_from_the_first_event_of_the_group() {
        let mut history = history();
        history.push(&[press(button(Buttons::L1), 0)], 0);
        let at_us = 4 * FRAME_US;
        history.push(
            &[
                InputEvent {
                    gap_before: true,
                    ..release(button(Buttons::L1), at_us)
                },
                InputEvent {
                    gap_before: true,
                    ..press(button(Buttons::R1), at_us)
                },
            ],
            at_us,
        );
        history.push(&[], at_us + FRAME_US);

        let rows = history.recent_trials(1)[0].rows();
        assert!(!rows[0].gap_before);
        assert!(rows[1].gap_before);
        assert_eq!(rows[1].held, vec![button(Buttons::R1)]);
    }

    /// 保持より古いイベントだけを落とす。境界ちょうどは残す。
    #[test]
    fn evicts_strictly_older_events() {
        let retain_us = 1_000_000;
        let mut history = History::new(retain_us, GAP_US);
        history.push(&[press(button(Buttons::L1), 0)], 0);
        history.push(&[press(button(Buttons::R1), 10)], 10);

        // 境界は now_us - retain_us。0 はちょうど境界なので残る。
        history.push(&[], retain_us);
        assert_eq!(history.recent_trials(1)[0].events.len(), 2);

        // 1µs 進めると 0 のイベントだけが落ちる。
        history.push(&[], retain_us + 1);
        let trial = &history.recent_trials(1)[0];
        assert_eq!(trial.events.len(), 1);
        assert_eq!(trial.events[0].target, button(Buttons::R1));
    }

    /// すべてのイベントが落ちた試行は消える。
    #[test]
    fn drops_trials_whose_events_all_expired() {
        let retain_us = 10_000_000;
        let mut history = History::new(retain_us, GAP_US);
        let mut at_us = 0;
        for _ in 0..3 {
            at_us = tap(&mut history, button(Buttons::CROSS), at_us, FRAME_US) + 2 * GAP_US;
        }
        let last_trial_start_us = at_us;
        tap(&mut history, button(Buttons::CROSS), at_us, FRAME_US);

        assert_eq!(history.recent_trials(10).len(), 4);

        // 最後の試行だけが保持窓に残る位置まで進める。
        history.push(&[], last_trial_start_us + retain_us);

        let trials = history.recent_trials(10);
        assert_eq!(trials.len(), 1);
        assert_eq!(trials[0].origin_us, last_trial_start_us);
    }

    /// 押しっぱなしのまま保持を超えても、進行中の試行は残る。
    /// イベントは落ちるが起点は動かず、後から来た離しも同じ試行に入る。
    #[test]
    fn an_active_trial_survives_losing_all_of_its_events() {
        let retain_us = 1_000_000;
        let mut history = History::new(retain_us, GAP_US);
        history.push(&[press(button(Buttons::L1), 0)], 0);

        history.push(&[], retain_us + 1);
        let trials = history.recent_trials(10);
        assert_eq!(trials.len(), 1);
        assert!(trials[0].events.is_empty());
        assert_eq!(trials[0].origin_us, 0);
        assert_eq!(trials[0].last_us, retain_us + 1);

        let release_us = retain_us + 2;
        history.push(&[release(button(Buttons::L1), release_us)], release_us);
        let trials = history.recent_trials(10);
        assert_eq!(trials.len(), 1);
        assert_eq!(trials[0].origin_us, 0);
        assert_eq!(trials[0].events.len(), 1);
    }

    /// 起点は試行を始めた押下の時刻に固定される。
    /// その押下が保持から外れても、後の押下の時刻には移らない。
    #[test]
    fn origin_stays_put_after_partial_eviction() {
        let retain_us = 3 * FRAME_US;
        let mut history = History::new(retain_us, GAP_US);
        history.push(&[press(button(Buttons::L1), 0)], 0);
        let second_us = 2 * FRAME_US;
        history.push(&[press(button(Buttons::R1), second_us)], second_us);

        // 境界は now_us - retain_us = FRAME_US。最初の押下だけが落ちる。
        history.push(&[], 4 * FRAME_US);

        let trial = &history.recent_trials(1)[0];
        assert_eq!(trial.events.len(), 1);
        assert_eq!(trial.events[0].target, button(Buttons::R1));
        assert_eq!(trial.origin_us, 0);
    }

    /// 履歴が空のうちは何も返さない。
    #[test]
    fn returns_nothing_before_any_event() {
        let mut history = history();
        history.push(&[], 0);
        history.push(&[], 10 * GAP_US);

        assert!(history.recent_trials(5).is_empty());
        assert_eq!(history.now_us(), 10 * GAP_US);
    }

    /// 保持 0 では現在時刻のイベントしか残らない。境界の判定が破綻しないことを見る。
    #[test]
    fn zero_retention_keeps_only_events_at_now() {
        let mut history = History::new(0, GAP_US);
        history.push(&[press(button(Buttons::L1), 0)], 0);
        assert_eq!(history.recent_trials(1)[0].events.len(), 1);

        history.push(&[], 1);
        let trials = history.recent_trials(1);
        assert_eq!(trials.len(), 1);
        assert!(trials[0].events.is_empty());

        // 離して区切ると、イベントの残らない試行は消える。
        history.push(&[release(button(Buttons::L1), 2)], 2);
        history.push(&[], 2 + GAP_US);
        assert!(history.recent_trials(1).is_empty());
    }

    /// 区切り時間 0 では、離した時点で確定する。
    #[test]
    fn zero_gap_finalizes_on_release() {
        let mut history = History::new(DEFAULT_RETAIN_US, 0);
        let released_us = tap(&mut history, button(Buttons::L1), 0, FRAME_US);

        let trials = history.recent_trials(5);
        assert_eq!(trials.len(), 1);
        assert_eq!(trials[0].last_us, released_us);

        history.push(&[press(button(Buttons::R1), released_us)], released_us);
        assert_eq!(history.recent_trials(5).len(), 2);
    }

    /// 時刻が進まない連続呼び出しでも壊れない。
    #[test]
    fn tolerates_repeated_pushes_at_the_same_time() {
        let mut history = history();
        for _ in 0..3 {
            history.push(&[press(button(Buttons::L1), 0)], 0);
        }
        history.push(&[], FRAME_US);

        let trial = &history.recent_trials(1)[0];
        assert_eq!(trial.events.len(), 3);
        // 同じ入力を押し直しても押下状態は 1 件。行も 1 行にまとまる。
        let rows = trial.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].held, vec![button(Buttons::L1)]);
    }

    /// 保持を超える長さの試行では、残ったイベントだけで行を組む。
    /// 先頭が離しになっても panic しない。
    #[test]
    fn rows_survive_a_partially_evicted_trial() {
        let retain_us = 3 * FRAME_US;
        let mut history = History::new(retain_us, GAP_US);
        history.push(&[press(button(Buttons::L1), 0)], 0);
        let release_us = 4 * FRAME_US;
        history.push(&[release(button(Buttons::L1), release_us)], release_us);

        let trial = &history.recent_trials(1)[0];
        assert_eq!(trial.events.len(), 1);
        let rows = trial.rows();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].held.is_empty());
        assert_eq!(rows[0].at_us, release_us);
    }
}
