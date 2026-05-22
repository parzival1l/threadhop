//! Phase D animation primitives — `Tween`, `Easing`, `Clock`.
//!
//! Pure motion math, no I/O, no global state. The Clock is test-injectable so
//! frame-buffer regression tests can pin a fixed `Instant` and walk a tween
//! forwards deterministically.
//!
//! Used by:
//!   * `App` scroll easing (`scroll_current` / `scroll_target`) — `EaseOutCubic`
//!     over 150ms when the user issues a PageDown / Home / End / etc.
//!   * Modal fade-in — `EaseOutCubic` over 80ms applied to the backdrop alpha
//!     so modals don't snap on; the dim region outside the modal rect ramps
//!     0 → ~0.7.
//!
//! Honors the `THREADHOP_NO_ANIM=1` environment override out-of-band: the App
//! reads the env var once at construction and stores the boolean on
//! `App::no_anim`. Callsites that want to bypass motion just check that flag
//! and use `Clock::frozen(..)` is purely a testing concern.

use std::time::{Duration, Instant};

/// Easing function — pure `f32 -> f32`, monotone non-decreasing on `[0, 1]`.
/// Input outside `[0, 1]` is clamped before the curve is applied.
///
/// Only `EaseOutCubic` is on the hot path today (scroll easing + modal
/// fade). `Linear` and `EaseInOutCubic` are kept for tests and future
/// phases — they cost nothing as variants but document the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Easing {
    Linear,
    EaseOutCubic,
    EaseInOutCubic,
}

impl Easing {
    /// Apply the easing curve. Clamps `t` to `[0, 1]` before sampling.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseOutCubic => 1.0 - (1.0 - t).powi(3),
            Easing::EaseInOutCubic => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
        }
    }
}

/// Test-injectable clock. Production paths construct `Clock::System`; tests
/// construct `Clock::Frozen(Instant::now())` and advance via
/// `Clock::Frozen(t)` returns of a fixed `Instant`.
///
/// The clock is `Clone` so it can be embedded in App state and passed by
/// shared reference to the tween's value-sampling methods.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub enum Clock {
    #[default]
    System,
    Frozen(Instant),
}

impl Clock {
    /// Sample current "now" according to the clock variant.
    pub fn now(&self) -> Instant {
        match self {
            Clock::System => Instant::now(),
            Clock::Frozen(t) => *t,
        }
    }

    /// Construct a frozen clock for tests. The returned clock samples `at`
    /// every time `now()` is called.
    #[allow(dead_code)]
    pub fn frozen(at: Instant) -> Self {
        Clock::Frozen(at)
    }
}

/// A finite tween between two `f32` values. Sampling outside the tween's
/// active interval clamps — `value` before `started_at` returns `from`, after
/// `started_at + duration` returns `to`.
#[derive(Debug, Clone, Copy)]
pub struct Tween {
    pub from: f32,
    pub to: f32,
    pub started_at: Instant,
    pub duration: Duration,
    pub easing: Easing,
}

impl Tween {
    /// Construct a tween whose `started_at` is the clock's current "now".
    pub fn new(from: f32, to: f32, duration: Duration, easing: Easing, clock: &Clock) -> Self {
        Self {
            from,
            to,
            started_at: clock.now(),
            duration,
            easing,
        }
    }

    /// Sample the tween's value at the clock's current instant. Before the
    /// tween's start, returns `from`. After `started_at + duration`, returns
    /// `to` (does not overshoot).
    pub fn value(&self, clock: &Clock) -> f32 {
        let now = clock.now();
        if now <= self.started_at {
            return self.from;
        }
        let elapsed = now.duration_since(self.started_at);
        if elapsed >= self.duration {
            return self.to;
        }
        // Both elapsed.as_secs_f32() and duration.as_secs_f32() are finite
        // and non-negative; duration is bounded away from 0 above so the
        // division is safe.
        let raw = elapsed.as_secs_f32() / self.duration.as_secs_f32();
        let eased = self.easing.apply(raw);
        self.from + (self.to - self.from) * eased
    }

    /// True when the clock has advanced past `started_at + duration`.
    /// Sampling `value` after that still returns `to`, but consumers can use
    /// this to clear the tween from their state.
    pub fn is_done(&self, clock: &Clock) -> bool {
        clock.now().duration_since(self.started_at) >= self.duration
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Boundary values for the linear curve. Anchors the contract: `apply(0)`
    /// is zero, `apply(0.5)` is one half, `apply(1)` is one. Anything else
    /// would break the affine sampling that `Tween::value` relies on.
    #[test]
    fn easing_linear_at_zero_one_half_one() {
        assert!((Easing::Linear.apply(0.0) - 0.0).abs() < 1e-6);
        assert!((Easing::Linear.apply(0.5) - 0.5).abs() < 1e-6);
        assert!((Easing::Linear.apply(1.0) - 1.0).abs() < 1e-6);
    }

    /// `EaseOutCubic` should be faster than linear at the start of the
    /// interval — `apply(0.1)` is strictly greater than `0.1` (linear) and in
    /// practice ~0.27. Asserting `> 0.2` gives generous headroom for fpu
    /// variance.
    #[test]
    fn easing_out_cubic_starts_fast() {
        let v = Easing::EaseOutCubic.apply(0.1);
        assert!(v > 0.2, "EaseOutCubic(0.1) should be > 0.2, got {v}");
        // And it should still be on `[0, 1]`.
        assert!(v <= 1.0);
    }

    /// `EaseInOutCubic` is symmetric around `t = 0.5` — `apply(t) + apply(1-t)`
    /// should sum to 1.0. Checking a few points is enough to detect a copy-
    /// paste error in the piecewise definition.
    #[test]
    fn easing_in_out_cubic_symmetric() {
        for t in [0.1_f32, 0.25, 0.4, 0.49] {
            let lhs = Easing::EaseInOutCubic.apply(t);
            let rhs = Easing::EaseInOutCubic.apply(1.0 - t);
            assert!(
                (lhs + rhs - 1.0).abs() < 1e-5,
                "EaseInOutCubic asymmetric at t={t}: {lhs} + {rhs} != 1.0"
            );
        }
        // And `apply(0.5)` is exactly 0.5.
        assert!((Easing::EaseInOutCubic.apply(0.5) - 0.5).abs() < 1e-6);
    }

    /// A tween sampled at its own `started_at` returns `from`. The
    /// production code constructs the tween with `clock.now()` and then
    /// samples immediately; we want that to read as the source value, not
    /// some half-step.
    #[test]
    fn tween_at_start_returns_from() {
        let now = Instant::now();
        let clock = Clock::Frozen(now);
        let t = Tween::new(0.0, 100.0, Duration::from_millis(150), Easing::Linear, &clock);
        assert!((t.value(&clock) - 0.0).abs() < 1e-5);
    }

    /// A tween sampled exactly at `started_at + duration` returns `to`.
    #[test]
    fn tween_at_end_returns_to() {
        let now = Instant::now();
        let start_clock = Clock::Frozen(now);
        let t = Tween::new(0.0, 100.0, Duration::from_millis(150), Easing::Linear, &start_clock);
        let end_clock = Clock::Frozen(now + Duration::from_millis(150));
        let v = t.value(&end_clock);
        assert!((v - 100.0).abs() < 1e-4, "expected ~100.0, got {v}");
    }

    /// Sampling past `started_at + 2 * duration` still returns `to` — the
    /// tween clamps, it doesn't overshoot. This matters for the App's render
    /// loop, which may sample a tween several frames after it nominally
    /// finished if the loop is busy.
    #[test]
    fn tween_clamps_after_end() {
        let now = Instant::now();
        let start_clock = Clock::Frozen(now);
        let t = Tween::new(0.0, 100.0, Duration::from_millis(100), Easing::Linear, &start_clock);
        let late_clock = Clock::Frozen(now + Duration::from_millis(500));
        assert!((t.value(&late_clock) - 100.0).abs() < 1e-4);
        assert!(t.is_done(&late_clock));
    }

    /// Pinning a clock and advancing it explicitly walks the tween — the
    /// midpoint sample should land strictly between `from` and `to` for any
    /// monotone non-decreasing easing.
    #[test]
    fn tween_with_frozen_clock_advances_explicitly() {
        let now = Instant::now();
        let start_clock = Clock::Frozen(now);
        let t = Tween::new(
            0.0,
            100.0,
            Duration::from_millis(100),
            Easing::EaseOutCubic,
            &start_clock,
        );
        let mid_clock = Clock::Frozen(now + Duration::from_millis(50));
        let v = t.value(&mid_clock);
        assert!(v > 0.0, "midpoint should be > from");
        assert!(v < 100.0, "midpoint should be < to");
        // EaseOutCubic is fast at the start — midpoint should be > 0.5 of
        // the linear interpolation (which would be 50.0).
        assert!(v > 50.0, "EaseOutCubic at 50% time should be > 50% value (got {v})");
    }
}
