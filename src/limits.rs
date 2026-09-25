use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use rand::Rng;

/// sshd-style MaxStartups start:rate:full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxStartups {
    pub start: u32,
    pub rate: u32,
    pub full: u32,
}

impl MaxStartups {
    pub fn parse(spec: &str) -> Result<Self, String> {
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() != 3 {
            return Err("MaxStartups must be start:rate:full".into());
        }
        let start: u32 = parts[0]
            .parse()
            .map_err(|_| "invalid MaxStartups start".to_string())?;
        let rate: u32 = parts[1]
            .parse()
            .map_err(|_| "invalid MaxStartups rate".to_string())?;
        let full: u32 = parts[2]
            .parse()
            .map_err(|_| "invalid MaxStartups full".to_string())?;
        if rate > 100 {
            return Err("MaxStartups rate must be 0..=100".into());
        }
        if full == 0 || full < start {
            return Err("MaxStartups full must be >= start and > 0".into());
        }
        Ok(Self { start, rate, full })
    }

    /// Probability of refusing a new unauthenticated connection given `current`
    /// in-progress connections (already counting the candidate).
    pub fn drop_probability(&self, current: u32) -> f64 {
        if current >= self.full {
            return 1.0;
        }
        if current < self.start {
            return 0.0;
        }
        if self.full == self.start {
            return 1.0;
        }
        let t = f64::from(current - self.start) / f64::from(self.full - self.start);
        let base = f64::from(self.rate) / 100.0;
        base + t * (1.0 - base)
    }

    pub fn should_drop<R: Rng + ?Sized>(&self, current: u32, rng: &mut R) -> bool {
        let p = self.drop_probability(current);
        if p <= 0.0 {
            return false;
        }
        if p >= 1.0 {
            return true;
        }
        rng.gen::<f64>() < p
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateSpec {
    pub count: u32,
    pub window: Duration,
}

impl RateSpec {
    pub fn parse(spec: &str) -> Result<Self, String> {
        let (count, secs) = spec
            .split_once('/')
            .ok_or_else(|| "rate must be COUNT/SECONDS".to_string())?;
        let count: u32 = count
            .parse()
            .map_err(|_| "invalid rate count".to_string())?;
        let secs: u64 = secs
            .parse()
            .map_err(|_| "invalid rate window".to_string())?;
        if count == 0 || secs == 0 {
            return Err("rate count and window must be > 0".into());
        }
        Ok(Self {
            count,
            window: Duration::from_secs(secs),
        })
    }
}

#[derive(Debug, Clone)]
pub struct LimitConfig {
    pub max_startups: MaxStartups,
    pub max_tcp: u32,
    pub max_sessions: u32,
    pub per_source_max: u32,
    pub per_source_rate: RateSpec,
    pub penalty_seconds: u64,
    pub penalty_threshold: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitDecision {
    Allow,
    GlobalTcp,
    MaxStartups,
    MaxSessions,
    PerSourceMax,
    PerSourceRate,
    Penalty,
}

impl AdmitDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::GlobalTcp => "global_tcp",
            Self::MaxStartups => "max_startups",
            Self::MaxSessions => "max_sessions",
            Self::PerSourceMax => "per_source_max",
            Self::PerSourceRate => "per_source_rate",
            Self::Penalty => "penalty",
        }
    }
}

#[derive(Debug, Clone)]
struct IpState {
    concurrent: u32,
    recent: VecDeque<Instant>,
    cooldown_until: Option<Instant>,
}

impl IpState {
    fn new() -> Self {
        Self {
            concurrent: 0,
            recent: VecDeque::new(),
            cooldown_until: None,
        }
    }
}

#[derive(Debug)]
pub struct LimitTracker {
    cfg: LimitConfig,
    in_progress: u32,
    sessions: u32,
    tcp: u32,
    per_ip: HashMap<IpAddr, IpState>,
}

impl LimitTracker {
    pub fn new(cfg: LimitConfig) -> Self {
        Self {
            cfg,
            in_progress: 0,
            sessions: 0,
            tcp: 0,
            per_ip: HashMap::new(),
        }
    }

    fn ip_state(&mut self, ip: IpAddr) -> &mut IpState {
        self.per_ip.entry(ip).or_insert_with(IpState::new)
    }

    fn prune_recent(state: &mut IpState, now: Instant, window: Duration) {
        while state
            .recent
            .front()
            .is_some_and(|t| now.duration_since(*t) > window)
        {
            state.recent.pop_front();
        }
    }

    pub fn admit<R: Rng + ?Sized>(
        &mut self,
        ip: IpAddr,
        now: Instant,
        rng: &mut R,
    ) -> AdmitDecision {
        if self.tcp >= self.cfg.max_tcp {
            return AdmitDecision::GlobalTcp;
        }
        if self.sessions >= self.cfg.max_sessions {
            return AdmitDecision::MaxSessions;
        }

        let rate = self.cfg.per_source_rate;
        let per_source_max = self.cfg.per_source_max;
        {
            let state = self.ip_state(ip);
            if state.cooldown_until.is_some_and(|until| now < until) {
                return AdmitDecision::Penalty;
            }
            if state.concurrent >= per_source_max {
                return AdmitDecision::PerSourceMax;
            }
            Self::prune_recent(state, now, rate.window);
            if state.recent.len() as u32 >= rate.count {
                return AdmitDecision::PerSourceRate;
            }
        }

        let candidate_in_progress = self.in_progress.saturating_add(1);
        if self
            .cfg
            .max_startups
            .should_drop(candidate_in_progress, rng)
        {
            return AdmitDecision::MaxStartups;
        }

        self.tcp += 1;
        self.in_progress += 1;
        let state = self.ip_state(ip);
        state.concurrent += 1;
        state.recent.push_back(now);
        AdmitDecision::Allow
    }

    pub fn mark_session_started(&mut self) {
        if self.in_progress > 0 {
            self.in_progress -= 1;
        }
        self.sessions += 1;
    }

    pub fn on_disconnect(
        &mut self,
        ip: IpAddr,
        now: Instant,
        session_started: bool,
        duration: Duration,
    ) {
        if self.tcp > 0 {
            self.tcp -= 1;
        }
        if session_started {
            if self.sessions > 0 {
                self.sessions -= 1;
            }
        } else if self.in_progress > 0 {
            self.in_progress -= 1;
        }

        let threshold = self.cfg.penalty_threshold;
        let penalty = Duration::from_secs(self.cfg.penalty_seconds);
        if let Some(state) = self.per_ip.get_mut(&ip) {
            if state.concurrent > 0 {
                state.concurrent -= 1;
            }
            if session_started && duration < threshold && penalty > Duration::ZERO {
                let until = now + penalty;
                state.cooldown_until = Some(
                    state
                        .cooldown_until
                        .map(|prev| prev.max(until))
                        .unwrap_or(until),
                );
            }
            if state.concurrent == 0
                && state.recent.is_empty()
                && state.cooldown_until.is_none_or(|u| now >= u)
            {
                self.per_ip.remove(&ip);
            }
        }
    }
}

pub fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;
    use std::net::Ipv4Addr;

    fn cfg() -> LimitConfig {
        LimitConfig {
            max_startups: MaxStartups {
                start: 2,
                rate: 50,
                full: 4,
            },
            max_tcp: 10,
            max_sessions: 8,
            per_source_max: 2,
            per_source_rate: RateSpec {
                count: 3,
                window: Duration::from_secs(60),
            },
            penalty_seconds: 30,
            penalty_threshold: Duration::from_secs(10),
        }
    }

    #[test]
    fn parse_max_startups() {
        let m = MaxStartups::parse("10:30:100").unwrap();
        assert_eq!(
            m,
            MaxStartups {
                start: 10,
                rate: 30,
                full: 100
            }
        );
        assert!(MaxStartups::parse("10:30").is_err());
        assert!(MaxStartups::parse("10:130:100").is_err());
        assert!(MaxStartups::parse("20:30:10").is_err());
    }

    #[test]
    fn startups_probability() {
        let m = MaxStartups {
            start: 10,
            rate: 30,
            full: 60,
        };
        assert_eq!(m.drop_probability(0), 0.0);
        assert_eq!(m.drop_probability(9), 0.0);
        assert!((m.drop_probability(10) - 0.30).abs() < 1e-9);
        assert_eq!(m.drop_probability(60), 1.0);
        assert_eq!(m.drop_probability(80), 1.0);
        let mid = m.drop_probability(35);
        assert!(mid > 0.30 && mid < 1.0);
    }

    #[test]
    fn startups_full_always_drops() {
        let m = MaxStartups {
            start: 1,
            rate: 0,
            full: 2,
        };
        let mut rng = SmallRng::seed_from_u64(1);
        assert!(m.should_drop(2, &mut rng));
        assert!(!m.should_drop(0, &mut rng));
    }

    #[test]
    fn per_source_cap_and_rate() {
        let mut t = LimitTracker::new(cfg());
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let now = Instant::now();
        let mut rng = SmallRng::seed_from_u64(2);
        assert_eq!(t.admit(ip, now, &mut rng), AdmitDecision::Allow);
        assert_eq!(t.admit(ip, now, &mut rng), AdmitDecision::Allow);
        assert_eq!(t.admit(ip, now, &mut rng), AdmitDecision::PerSourceMax);
    }

    #[test]
    fn per_source_rate_window() {
        let mut t = LimitTracker::new(LimitConfig {
            per_source_max: 10,
            per_source_rate: RateSpec {
                count: 2,
                window: Duration::from_secs(60),
            },
            ..cfg()
        });
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        let now = Instant::now();
        let mut rng = SmallRng::seed_from_u64(3);
        assert_eq!(t.admit(ip, now, &mut rng), AdmitDecision::Allow);
        t.on_disconnect(ip, now, false, Duration::from_millis(5));
        assert_eq!(
            t.admit(ip, now + Duration::from_secs(1), &mut rng),
            AdmitDecision::Allow
        );
        t.on_disconnect(ip, now, false, Duration::from_millis(5));
        assert_eq!(
            t.admit(ip, now + Duration::from_secs(2), &mut rng),
            AdmitDecision::PerSourceRate
        );
    }

    #[test]
    fn short_session_applies_penalty_http_does_not() {
        let mut t = LimitTracker::new(cfg());
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3));
        let now = Instant::now();
        let mut rng = SmallRng::seed_from_u64(4);
        assert_eq!(t.admit(ip, now, &mut rng), AdmitDecision::Allow);
        t.mark_session_started();
        t.on_disconnect(
            ip,
            now + Duration::from_secs(2),
            true,
            Duration::from_secs(2),
        );
        assert_eq!(
            t.admit(ip, now + Duration::from_secs(3), &mut rng),
            AdmitDecision::Penalty
        );

        let ip2 = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 4));
        assert_eq!(t.admit(ip2, now, &mut rng), AdmitDecision::Allow);
        t.on_disconnect(
            ip2,
            now + Duration::from_millis(3),
            false,
            Duration::from_millis(3),
        );
        assert_eq!(
            t.admit(ip2, now + Duration::from_secs(1), &mut rng),
            AdmitDecision::Allow
        );
    }

    #[test]
    fn normalize_mapped_v6() {
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        assert_eq!(
            normalize_ip(mapped),
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))
        );
    }
}
