use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Idle,
    GradientDetect,
    Switching,
    Penalty,
    Frozen,
}

pub struct PenaltyLock {
    pub bond_id: String,
    pub until: Instant,
    pub cooldown_secs: u64,
}

impl PenaltyLock {
    pub fn new(bond_id: &str, cooldown_secs: u64) -> Self {
        Self {
            bond_id: bond_id.to_string(),
            until: Instant::now() + Duration::from_secs(cooldown_secs),
            cooldown_secs,
        }
    }

    pub fn active(&self) -> bool {
        Instant::now() < self.until
    }

    pub fn next_cooldown(prev: u64) -> u64 {
        (prev * 2).min(300)
    }
}

/// 日用防抖：下切 3–5s / 上切 5–10s，滑动窗口中位数
pub struct Debouncer {
    window: VecDeque<f32>,
    capacity: usize,
    started: Option<Instant>,
    need: Duration,
}

impl Debouncer {
    pub fn downswitch() -> Self {
        Self {
            window: VecDeque::new(),
            capacity: 5,
            started: None,
            need: Duration::from_secs(4), // 3–5s 取中
        }
    }

    pub fn upswitch() -> Self {
        Self {
            window: VecDeque::new(),
            capacity: 8,
            started: None,
            need: Duration::from_secs(7), // 5–10s 取中
        }
    }

    pub fn set_need_secs(&mut self, secs: u64) {
        self.need = Duration::from_secs(secs.max(1));
    }

    pub fn reset(&mut self) {
        self.window.clear();
        self.started = None;
    }

    /// 推入样本；防抖时间够且中位数仍满足 predicate 则 true
    pub fn push_and_ready(&mut self, sample: f32, still_bad: impl Fn(f32) -> bool) -> bool {
        if self.started.is_none() {
            self.started = Some(Instant::now());
        }
        self.window.push_back(sample);
        while self.window.len() > self.capacity {
            self.window.pop_front();
        }
        let elapsed = self.started.map(|t| t.elapsed()).unwrap_or_default();
        if elapsed < self.need || self.window.len() < 3 {
            return false;
        }
        let mut v: Vec<f32> = self.window.iter().copied().collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = v[v.len() / 2];
        still_bad(mid)
    }
}

pub struct StateMachine {
    pub state: State,
    pub penalty: Option<PenaltyLock>,
    down_deb: Debouncer,
    up_deb: Debouncer,
    /// 非 preferred 驻留时，上次对侧嗅探
    pub last_peer_probe: Option<Instant>,
}

impl StateMachine {
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            penalty: None,
            down_deb: Debouncer::downswitch(),
            up_deb: Debouncer::upswitch(),
            last_peer_probe: None,
        }
    }

    pub fn in_penalty(&self) -> bool {
        self.penalty.as_ref().map(|p| p.active()).unwrap_or(false)
    }

    pub fn penalty_remaining_secs(&self) -> u64 {
        self.penalty
            .as_ref()
            .filter(|p| p.active())
            .map(|p| p.until.saturating_duration_since(Instant::now()).as_secs())
            .unwrap_or(0)
    }

    /// eco=true → 下切 7s / 上切 12s；否则日用 4s / 7s
    pub fn apply_eco(&mut self, eco: bool) {
        if eco {
            self.down_deb.set_need_secs(7);
            self.up_deb.set_need_secs(12);
        } else {
            self.down_deb.set_need_secs(4);
            self.up_deb.set_need_secs(7);
        }
    }

    pub fn enter_penalty(&mut self, bond_id: &str) {
        let cool = self
            .penalty
            .as_ref()
            .map(|p| PenaltyLock::next_cooldown(p.cooldown_secs))
            .unwrap_or(30);
        log::warn!("state_machine: PENALTY {bond_id} cool={cool}s");
        self.penalty = Some(PenaltyLock::new(bond_id, cool));
        self.state = State::Penalty;
        self.down_deb.reset();
        self.up_deb.reset();
    }

    pub fn clear_penalty_if_due(&mut self) {
        if let Some(p) = &self.penalty {
            if !p.active() {
                log::info!("state_machine: penalty expired");
                self.penalty = None;
                self.state = State::Idle;
            }
        }
    }

    /// 根据健康度推进；返回是否应尝试下切 / 上切
    /// 下切仅当在首选频段（5G→2.4G），上切仅当在非首选频段（2.4G→5G）
    pub fn on_score(
        &mut self,
        score: f32,
        switch_th: f32,
        detect_th: f32,
        on_preferred_band: bool,
    ) -> SwitchHint {
        self.clear_penalty_if_due();
        if self.in_penalty() {
            self.state = State::Penalty;
            return SwitchHint::None;
        }
        if matches!(self.state, State::Switching) {
            return SwitchHint::None;
        }

        if on_preferred_band && score < switch_th {
            // 在首选频段且健康度跌破切换阈值 → 准备下切到非首选
            self.state = State::GradientDetect;
            if self
                .down_deb
                .push_and_ready(score, |m| m < switch_th)
            {
                self.down_deb.reset();
                self.state = State::Switching;
                return SwitchHint::Downswitch;
            }
            return SwitchHint::None;
        }

        if !on_preferred_band && score >= detect_th {
            // 在非首选频段且健康度恢复 → 准备上切回首选
            self.down_deb.reset();
            if self
                .up_deb
                .push_and_ready(score, |m| m >= detect_th)
            {
                self.up_deb.reset();
                self.state = State::Switching;
                return SwitchHint::Upswitch;
            }
            self.state = State::GradientDetect;
            return SwitchHint::None;
        }

        // 中间区域：仅梯度检测，不触发切换
        self.state = State::GradientDetect;
        self.down_deb.reset();
        self.up_deb.reset();
        SwitchHint::None
    }

    pub fn finish_switch_ok(&mut self) {
        self.state = State::Idle;
        self.down_deb.reset();
        self.up_deb.reset();
    }

    /// 用户手动切网或外部打断：回 Idle，清防抖（保留 penalty）
    pub fn reset_soft(&mut self) {
        self.state = State::Idle;
        self.down_deb.reset();
        self.up_deb.reset();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchHint {
    None,
    Downswitch,
    Upswitch,
}
