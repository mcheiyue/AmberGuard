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

    /// 根据健康度推进；返回是否应尝试下切 / 上切。
    ///
    /// **优先级（与阈值页一致）**
    /// 1. 下切线 `switch_th`（健康分）：仅在首选频段，score 持续 < switch → Downswitch  
    /// 2. 上切不看当前健康分硬门：非首选频段防抖满 → Upswitch（对端 RSSI 由主循环把关）  
    /// 3. 观察线 `detect_th`（健康分）：首选上 switch≤score<detect → 仅 GradientDetect（更勤扫描由主循环）
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

        // —— 首选频段（通常 5G）：三区间 ——
        //   score >= detect     → Idle（稳）
        //   switch <= score < detect → 观察（不切）
        //   score < switch      → 下切防抖
        if on_preferred_band {
            self.up_deb.reset();
            if score < switch_th {
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
            self.down_deb.reset();
            if score < detect_th {
                self.state = State::GradientDetect;
                return SwitchHint::None;
            }
            self.state = State::Idle;
            return SwitchHint::None;
        }

        // —— 非偏好频段：上切回偏好 ——
        // 后备上仍健康（分≥观察线）→ 上切防抖 25s，减轻「手切后备立刻被拉回」。
        // 后备已差 → 7s 尽快回偏好。
        self.down_deb.reset();
        self.state = State::GradientDetect;
        let up_need = if score >= detect_th { 25 } else { 7 };
        self.up_deb.set_need_secs(up_need);
        if self.up_deb.push_and_ready(score, |_| true) {
            self.up_deb.reset();
            self.state = State::Switching;
            return SwitchHint::Upswitch;
        }
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
    /// 偏好频段内追优（仅同频更好 AP，由主循环判定）
    SameBandRoam,
}
