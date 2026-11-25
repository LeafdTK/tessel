use std::time::Duration;
use tessel_proto::Millis;
use tessel_proto::Watchdog;
use tessel_proto::task::TaskState::{self, *};

pub const STATES: [TaskState; 11] = [Draft, Ready, Running, Proving, Review, Landing, Checking, Planned, Landed, Dropped, Unknown];

pub const MAX_RETRIES: u32 = 3;

pub const MAX_RESTARTS: u32 = 3;

pub fn legal(from: TaskState, to: TaskState) -> bool {
    if from == to {
        return !from.finished();
    }
    match from {
        Draft => matches!(to, Ready | Dropped),
        Ready => matches!(to, Draft | Running | Dropped),
        Running => matches!(to, Proving | Review | Planned | Draft | Ready | Dropped),
        Proving => matches!(to, Running | Ready | Review | Landing | Dropped),
        Review => matches!(to, Running | Ready | Proving | Landing | Checking | Planned | Dropped),
        Landing => matches!(to, Landed | Checking | Running | Review | Dropped),
        Checking => matches!(to, Landed | Review | Running | Dropped),
        Planned => matches!(to, Landed | Review | Dropped),
        Landed => false,
        Dropped => to == Ready,
        Unknown => matches!(to, Draft | Ready | Review | Dropped),
    }
}

pub fn backoff(tries: u32, jitter: f64) -> Duration {
    let full = 200u64.saturating_mul(1u64 << tries.min(20)).min(60_000);
    let half = full / 2;
    Duration::from_millis(half + (half as f64 * jitter.clamp(0.0, 1.0)) as u64)
}

pub fn jitter() -> f64 {
    (uuid::Uuid::new_v4().as_u128() as u64) as f64 / u64::MAX as f64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bark {
    Nudge,
    Retry,
    RestartProof,
    Hold,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Watch {
    pub nudged: Option<Millis>,
    pub restarted: Option<Millis>,
}

pub fn watchdog(state: TaskState, now: Millis, since: Millis, active: Millis, watch: Watch, w: &Watchdog) -> Option<Bark> {
    let ms = |secs: u64| secs.saturating_mul(1000);
    let after = |from: Millis, secs: u64| secs > 0 && now.saturating_sub(from) >= ms(secs);
    match state {
        Running => match watch.nudged {
            None => after(since.max(active), w.quiet_secs).then_some(Bark::Nudge),
            Some(n) => after(n.max(active), w.nudge_secs).then_some(Bark::Retry),
        },
        Proving => match watch.restarted {
            None => after(since, w.proving_secs).then_some(Bark::RestartProof),
            Some(r) => after(r, w.proving_secs).then_some(Bark::Hold),
        },
        Landing => after(since, w.landing_secs).then_some(Bark::Hold),
        Checking => after(since, w.checking_secs).then_some(Bark::Hold),
        _ => None,
    }
}

pub fn minutes(secs: u64) -> String {
    match secs {
        1 => "a second".to_owned(),
        s if s < 60 => format!("{s} seconds"),
        s if s < 120 => "a minute".to_owned(),
        s => format!("{} minutes", s / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashSet, VecDeque};

    fn reachable(from: TaskState) -> HashSet<TaskState> {
        let mut seen = HashSet::from([from]);
        let mut queue = VecDeque::from([from]);
        while let Some(s) = queue.pop_front() {
            for to in STATES {
                if legal(s, to) && seen.insert(to) {
                    queue.push_back(to);
                }
            }
        }
        seen
    }

    #[test]
    fn every_unfinished_state_can_reach_an_end_or_a_person() {
        for s in STATES.into_iter().filter(|s| !s.finished()) {
            let r = reachable(s);
            assert!(r.contains(&Landed) || r.contains(&Dropped) || r.contains(&Review), "{s:?} is a dead end: {r:?}");
            assert!(legal(s, Dropped), "{s:?} can always be dropped");
        }
        for s in [Running, Proving, Landing, Checking, Planned] {
            assert!(legal(s, Review), "tessel can always hold {s:?} for a person");
        }
    }

    #[test]
    fn nothing_leaves_landed_and_dropped_only_comes_back_when_a_person_starts_it_again() {
        for to in STATES {
            assert!(!legal(Landed, to), "landed went to {to:?}");
            assert_eq!(legal(Dropped, to), to == Ready, "dropped went to {to:?}");
        }
        assert!(!legal(Landed, Landed) && !legal(Dropped, Dropped), "a finished task doesn't finish twice");
    }

    #[test]
    fn work_goes_forward_through_proof_and_landing_not_around_them() {
        assert!(!legal(Running, Landing) && !legal(Running, Landed), "nothing lands without being proved");
        assert!(!legal(Ready, Proving) && !legal(Draft, Running), "nothing is proved or run before it's dispatched");
        assert!(!legal(Proving, Landed) && !legal(Review, Landed), "landing is its own step");
        for s in STATES {
            assert!(legal(s, s) != s.finished(), "{s:?} stays put only while it's unfinished");
        }
    }

    #[test]
    fn retries_back_off_from_200ms_doubling_to_a_minute_with_jitter() {
        assert_eq!(backoff(0, 1.0), Duration::from_millis(200));
        assert_eq!(backoff(0, 0.0), Duration::from_millis(100));
        assert_eq!(backoff(1, 1.0), Duration::from_millis(400));
        assert_eq!(backoff(3, 1.0), Duration::from_millis(1600));
        assert_eq!(backoff(30, 1.0), Duration::from_secs(60));
        assert_eq!(backoff(30, 0.0), Duration::from_secs(30));
        for _ in 0..100 {
            let j = jitter();
            assert!((0.0..=1.0).contains(&j));
            let b = backoff(2, j);
            assert!(b >= Duration::from_millis(400) && b <= Duration::from_millis(800), "{b:?}");
        }
    }

    #[test]
    fn the_watchdog_nudges_a_quiet_agent_then_starts_it_over_and_holds_what_overruns() {
        let w = Watchdog::default();
        let min = 60_000;
        let quiet = Watch::default();
        assert_eq!(watchdog(Running, 19 * min, 0, 0, quiet, &w), None);
        assert_eq!(watchdog(Running, 20 * min, 0, 0, quiet, &w), Some(Bark::Nudge));
        assert_eq!(watchdog(Running, 30 * min, 0, 15 * min, quiet, &w), None, "it did something a while ago");
        let nudged = Watch { nudged: Some(20 * min), ..quiet };
        assert_eq!(watchdog(Running, 29 * min, 0, 0, nudged, &w), None);
        assert_eq!(watchdog(Running, 30 * min, 0, 0, nudged, &w), Some(Bark::Retry));
        assert_eq!(watchdog(Proving, 30 * min, 0, 0, quiet, &w), Some(Bark::RestartProof));
        let restarted = Watch { restarted: Some(30 * min), ..quiet };
        assert_eq!(watchdog(Proving, 59 * min, 0, 0, restarted, &w), None);
        assert_eq!(watchdog(Proving, 60 * min, 0, 0, restarted, &w), Some(Bark::Hold));
        assert_eq!(watchdog(Landing, 9 * min, 0, 0, quiet, &w), None);
        assert_eq!(watchdog(Landing, 10 * min, 0, 0, quiet, &w), Some(Bark::Hold));
        assert_eq!(watchdog(Checking, 90 * min, 0, 0, quiet, &w), Some(Bark::Hold));
        for s in [Draft, Ready, Review, Planned, Landed, Dropped] {
            assert_eq!(watchdog(s, u64::MAX / 2, 0, 0, quiet, &w), None, "{s:?} waits as long as it likes");
        }
        assert_eq!((minutes(1), minutes(30), minutes(90), minutes(1200)), ("a second".to_owned(), "30 seconds".to_owned(), "a minute".to_owned(), "20 minutes".to_owned()));
        let off = Watchdog { quiet_secs: 0, proving_secs: 0, landing_secs: 0, checking_secs: 0, nudge_secs: 0 };
        for s in STATES {
            assert_eq!(watchdog(s, u64::MAX / 2, 0, 0, quiet, &off), None, "0 turns a check off");
        }
    }
}
