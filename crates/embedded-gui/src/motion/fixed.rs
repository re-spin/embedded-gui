//! Integer-only easing and tweens for FPU-less targets.
//!
//! Progress and eased values are Q16 with `65535` as 1.0. Nothing here uses
//! floats or has a reachable panic, so it can run every frame on the BF531.

use core::num::NonZeroU32;

/// 1.0 in the Q16 scale used by [`ease_q16`].
pub const Q16_ONE: u16 = u16::MAX;

/// Integer easing curves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QEase {
    #[default]
    Linear,
    OutQuad,
    OutCubic,
    InOutCubic,
    Smoothstep,
    /// Overshoots past 1.0 (peaks near 1.1) before settling.
    OutBack,
}

/// `a * b / 65535`, exact for `a, b <= 65535`, without a divide.
#[inline]
fn mul(a: u32, b: u32) -> u32 {
    let x = a * b;
    (x + (x >> 16) + 1) >> 16
}

/// Eases Q16 progress `t` (`65535` = 1.0). The result is Q16 and stays in
/// `0..=65535` except for `OutBack`, which overshoots.
pub fn ease_q16(e: QEase, t: u16) -> i32 {
    let one = u32::from(Q16_ONE);
    let t = u32::from(t);
    let u = one - t;
    let r = match e {
        QEase::Linear => t,
        QEase::OutQuad => one - mul(u, u),
        QEase::OutCubic => one - mul(mul(u, u), u),
        QEase::InOutCubic => {
            // 4t³ below the midpoint, 1 - 4u³ above it, as (2t)³ / 2.
            if t < 32768 {
                let s = 2 * t;
                mul(mul(s, s), s) / 2
            } else {
                let s = 2 * u;
                one - mul(mul(s, s), s) / 2
            }
        }
        QEase::Smoothstep => {
            let t2 = mul(t, t);
            3 * t2 - 2 * mul(t2, t)
        }
        QEase::OutBack => {
            // 1 - c3·u³ + c1·u², with c1 = 1.70158 and c3 = c1 + 1.
            const K: u32 = 45978; // 0.70158 in Q16
            let u2 = mul(u, u);
            let u3 = mul(u2, u);
            let up = u2 + mul(u2, K);
            let down = 2 * u3 + mul(u3, K);
            return one as i32 + up as i32 - down as i32;
        }
    };
    r as i32
}

/// A time-based integer tween. Its value depends only on `now_ms`, so a
/// dropped frame never drifts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QTween {
    pub from: i32,
    pub to: i32,
    pub start_ms: u32,
    pub dur_ms: u16,
    pub ease: QEase,
}

impl QTween {
    pub const fn new(from: i32, to: i32, start_ms: u32, dur_ms: u16, ease: QEase) -> Self {
        Self {
            from,
            to,
            start_ms,
            dur_ms,
            ease,
        }
    }

    /// A finished tween resting at `value`.
    pub const fn at(value: i32) -> Self {
        Self::new(value, value, 0, 0, QEase::Linear)
    }

    /// Q16 progress at `now_ms`; `now_ms` before `start_ms` reads as 0. The
    /// clock may wrap.
    pub fn progress(&self, now_ms: u32) -> u16 {
        let elapsed = now_ms.wrapping_sub(self.start_ms) as i32;
        let Some(dur) = NonZeroU32::new(u32::from(self.dur_ms)) else {
            return Q16_ONE;
        };
        if elapsed <= 0 {
            return 0;
        }
        let elapsed = elapsed as u32;
        if elapsed >= dur.get() {
            return Q16_ONE;
        }
        // elapsed < dur <= 65535, so the product fits and the quotient < 65535.
        (elapsed * u32::from(Q16_ONE) / dur) as u16
    }

    pub fn done(&self, now_ms: u32) -> bool {
        self.progress(now_ms) == Q16_ONE
    }

    pub fn value(&self, now_ms: u32) -> i32 {
        let p = self.progress(now_ms);
        if p == Q16_ONE {
            return self.to;
        }
        let e = i64::from(ease_q16(self.ease, p));
        let delta = i64::from(self.to) - i64::from(self.from);
        (i64::from(self.from) + ((delta * e) >> 16)) as i32
    }

    /// Restarts toward `to` from wherever the tween is at `now_ms`, so a
    /// target that changes mid-flight glides instead of jumping.
    pub fn retarget(&mut self, now_ms: u32, to: i32) {
        self.from = self.value(now_ms);
        self.to = to;
        self.start_ms = now_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [QEase; 6] = [
        QEase::Linear,
        QEase::OutQuad,
        QEase::OutCubic,
        QEase::InOutCubic,
        QEase::Smoothstep,
        QEase::OutBack,
    ];

    #[test]
    fn curves_hit_endpoints_and_track_float_reference() {
        for e in ALL {
            assert_eq!(ease_q16(e, 0), 0, "{e:?}");
            assert_eq!(ease_q16(e, Q16_ONE), 65535, "{e:?}");
            for t in (0..=65535u32).step_by(257) {
                let x = t as f64 / 65535.0;
                let u = 1.0 - x;
                let want = match e {
                    QEase::Linear => x,
                    QEase::OutQuad => 1.0 - u * u,
                    QEase::OutCubic => 1.0 - u * u * u,
                    QEase::InOutCubic if x < 0.5 => 4.0 * x * x * x,
                    QEase::InOutCubic => 1.0 - 4.0 * u * u * u,
                    QEase::Smoothstep => x * x * (3.0 - 2.0 * x),
                    QEase::OutBack => 1.0 - 2.70158 * u * u * u + 1.70158 * u * u,
                } * 65535.0;
                let got = ease_q16(e, t as u16) as f64;
                assert!(
                    (got - want).abs() <= 8.0,
                    "{e:?} t={t} got={got} want={want}"
                );
            }
        }
        let peak = (0..=65535u16).map(|t| ease_q16(QEase::OutBack, t)).max();
        assert!(peak > Some(70000));
    }

    #[test]
    fn tween_is_time_based_and_retargets() {
        let mut tw = QTween::new(0, 19, 1000, 120, QEase::OutCubic);
        assert_eq!(tw.value(500), 0);
        assert_eq!(tw.value(1000), 0);
        assert_eq!(tw.value(1120), 19);
        assert!(tw.done(1120) && !tw.done(1119));
        let mid = tw.value(1060);
        assert!(mid > 9 && mid < 19, "{mid}");
        // Monotonic, and 480 px over 250 ms reaches the end.
        let slide = QTween::new(480, 0, 0, 250, QEase::OutCubic);
        assert!((1..=250).all(|t| slide.value(t) <= slide.value(t - 1)));
        assert_eq!(slide.value(250), 0);
        tw.retarget(1060, 38);
        assert_eq!(tw.value(1060), mid);
        assert_eq!(tw.value(1180), 38);
        // Clock wrap and zero duration.
        let wrap = QTween::new(0, 100, u32::MAX - 10, 20, QEase::Linear);
        assert_eq!(wrap.value(u32::MAX - 10), 0);
        assert_eq!(wrap.value(9), 100);
        assert_eq!(QTween::at(7).value(0), 7);
    }
}
