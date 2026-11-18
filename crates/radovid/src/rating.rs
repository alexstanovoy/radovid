//! Glicko-2 with the standard constants (Glickman, 2013), one rating period
//! per game, and the growth of the deviation with idle time.

use radovid_proto::{Rating, UnixMs};

/// Rating of an account that never played.
pub const INITIAL_RATING: f64 = 1500.0;
/// Deviation of an account that never played; also the most a deviation grows to.
pub const INITIAL_DEVIATION: f64 = 350.0;
/// Volatility of an account that never played.
pub const INITIAL_VOLATILITY: f64 = 0.06;
/// How strongly the volatility may move.
const MS_PER_DAY: f64 = 86_400_000.0;
const TAU: f64 = 0.5;
/// Glicko-2's internal scale factor.
const SCALE: f64 = 173.7178;
const EPSILON: f64 = 1e-6;
/// The Illinois iteration is bounded; it converges in a handful of steps.
const MAX_ITERATIONS: u32 = 100;

/// The share of a computed rating change that an account receives: greater than 0 and at most 1.
/// It is part of a match's stored result, so a record replayed after a restart is rated with the
/// share it was finished under, whatever the configuration says by then.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct RatingShare(f64);

impl RatingShare {
    /// The whole change, as for an account with no share configured.
    pub const FULL: RatingShare = RatingShare(1.0);

    pub fn get(self) -> f64 {
        self.0
    }
}

impl Default for RatingShare {
    fn default() -> Self {
        Self::FULL
    }
}

impl TryFrom<f64> for RatingShare {
    type Error = String;

    fn try_from(share: f64) -> Result<Self, String> {
        // Written positively so that NaN is refused too.
        if share > 0.0 && share <= 1.0 {
            Ok(RatingShare(share))
        } else {
            Err(format!(
                "a rating share must be greater than 0 and at most 1, not {share}"
            ))
        }
    }
}

impl From<RatingShare> for f64 {
    fn from(share: RatingShare) -> f64 {
        share.0
    }
}

/// A player's standing, with the volatility the wire never shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glicko {
    pub rating: f64,
    pub deviation: f64,
    pub volatility: f64,
}

impl Glicko {
    /// A newcomer.
    pub const NEW: Glicko = Glicko {
        rating: INITIAL_RATING,
        deviation: INITIAL_DEVIATION,
        volatility: INITIAL_VOLATILITY,
    };

    /// What the wire shows.
    pub fn public(&self) -> Rating {
        Rating {
            rating: self.rating,
            deviation: self.deviation,
        }
    }

    /// The deviation grown for `idle_days` of not playing, one rating period
    /// per day, at most back to a newcomer's.
    pub fn after_idle(self, idle_days: f64) -> Glicko {
        if idle_days <= 0.0 {
            return self;
        }
        let phi = self.deviation / SCALE;
        let grown = (phi * phi + self.volatility * self.volatility * idle_days.max(0.0)).sqrt();
        Glicko {
            deviation: (grown * SCALE).min(INITIAL_DEVIATION),
            ..self
        }
    }

    /// The standing at `now` for a player who last played at `last_played`: the one place idle growth
    /// is applied, both before a rated game and when a rating is shown.
    pub fn idle_since(self, last_played: UnixMs, now: UnixMs) -> Glicko {
        self.after_idle(now.0.saturating_sub(last_played.0) as f64 / MS_PER_DAY)
    }

    /// The standing after one rating period of games against `opponents`, each
    /// with the score of the player (1 win, 0.5 draw, 0 loss).
    pub fn updated(self, opponents: &[(Glicko, f64)]) -> Glicko {
        self.updated_weighted(opponents, 1.0)
    }

    /// One match: opposing sides divide one observation's information and gain.
    pub fn updated_match(self, opponents: &[(Glicko, f64)]) -> Glicko {
        if opponents.len() <= 1 {
            return self.updated(opponents);
        }
        self.updated_weighted(opponents, 1.0 / opponents.len() as f64)
    }

    /// `after`, the standing the algorithm computed from `self`, with the rating moved only `share`
    /// of the way there. The deviation and volatility stay the algorithm's.
    pub fn damped(self, after: Glicko, share: RatingShare) -> Glicko {
        let change = after.rating - self.rating;
        // Written from `after` so that the full share returns it bit for bit; the clamp keeps
        // rounding from carrying the result past either end.
        let rating = (after.rating - (1.0 - share.get()) * change)
            .clamp(self.rating.min(after.rating), self.rating.max(after.rating));
        Glicko { rating, ..after }
    }

    fn updated_weighted(self, opponents: &[(Glicko, f64)], weight: f64) -> Glicko {
        let mu = (self.rating - INITIAL_RATING) / SCALE;
        let phi = self.deviation / SCALE;
        if opponents.is_empty() {
            let grown = (phi * phi + self.volatility * self.volatility).sqrt();
            return Glicko {
                deviation: grown * SCALE,
                ..self
            };
        }
        let mut inverse_v = 0.0;
        let mut gain = 0.0;
        for (opponent, score) in opponents {
            let mu_j = (opponent.rating - INITIAL_RATING) / SCALE;
            let g = g(opponent.deviation / SCALE);
            let e = expected(mu, mu_j, g);
            inverse_v += g * g * e * (1.0 - e);
            gain += g * (score - e);
        }
        inverse_v *= weight;
        gain *= weight;
        let v = 1.0 / inverse_v;
        let delta = v * gain;
        let sigma = new_volatility(phi, v, delta, self.volatility);
        let phi_star = (phi * phi + sigma * sigma).sqrt();
        let phi_new = 1.0 / (1.0 / (phi_star * phi_star) + 1.0 / v).sqrt();
        let mu_new = mu + phi_new * phi_new * gain;
        Glicko {
            rating: SCALE * mu_new + INITIAL_RATING,
            deviation: SCALE * phi_new,
            volatility: sigma,
        }
    }
}

fn g(phi: f64) -> f64 {
    1.0 / (1.0 + 3.0 * phi * phi / (std::f64::consts::PI * std::f64::consts::PI)).sqrt()
}

fn expected(mu: f64, mu_j: f64, g_j: f64) -> f64 {
    1.0 / (1.0 + (-g_j * (mu - mu_j)).exp())
}

/// Step 5 of the algorithm: the new volatility, by the Illinois method.
fn new_volatility(phi: f64, v: f64, delta: f64, sigma: f64) -> f64 {
    let a = (sigma * sigma).ln();
    let f = |x: f64| {
        let ex = x.exp();
        let top = ex * (delta * delta - phi * phi - v - ex);
        let bottom = 2.0 * (phi * phi + v + ex).powi(2);
        top / bottom - (x - a) / (TAU * TAU)
    };
    let mut big_a = a;
    let mut big_b = if delta * delta > phi * phi + v {
        (delta * delta - phi * phi - v).ln()
    } else {
        let mut k = 1.0;
        while f(a - k * TAU) < 0.0 && k < 1000.0 {
            k += 1.0;
        }
        a - k * TAU
    };
    let mut f_a = f(big_a);
    let mut f_b = f(big_b);
    for _ in 0..MAX_ITERATIONS {
        if (big_b - big_a).abs() <= EPSILON {
            break;
        }
        let c = big_a + (big_a - big_b) * f_a / (f_b - f_a);
        let f_c = f(c);
        if f_c * f_b <= 0.0 {
            big_a = big_b;
            f_a = f_b;
        } else {
            f_a /= 2.0;
        }
        big_b = c;
        f_b = f_c;
    }
    (big_a / 2.0).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_published_worked_example_comes_out() {
        let player = Glicko {
            rating: 1500.0,
            deviation: 200.0,
            volatility: 0.06,
        };
        let opponent = |rating, deviation| Glicko {
            rating,
            deviation,
            volatility: 0.06,
        };
        let after = player.updated(&[
            (opponent(1400.0, 30.0), 1.0),
            (opponent(1550.0, 100.0), 0.0),
            (opponent(1700.0, 300.0), 0.0),
        ]);
        assert!((after.rating - 1464.06).abs() < 0.01, "{after:?}");
        assert!((after.deviation - 151.52).abs() < 0.01, "{after:?}");
        assert!((after.volatility - 0.05999).abs() < 0.00001, "{after:?}");
    }

    #[test]
    fn idle_time_grows_the_deviation_up_to_a_newcomers() {
        let settled = Glicko {
            rating: 1600.0,
            deviation: 60.0,
            volatility: 0.06,
        };
        let week = settled.after_idle(7.0).deviation;
        assert!(week > 60.0 && week < 70.0, "{week}");
        assert_eq!(settled.after_idle(1e9).deviation, INITIAL_DEVIATION);
        assert!((settled.after_idle(0.0).deviation - 60.0).abs() < 1e-9);
    }

    fn state(rating: f64, deviation: f64) -> Glicko {
        Glicko {
            rating,
            deviation,
            ..Glicko::NEW
        }
    }

    fn share(value: f64) -> RatingShare {
        RatingShare::try_from(value).expect("a valid share")
    }

    #[test]
    fn a_share_below_one_scales_the_rating_change_and_leaves_deviation_and_volatility() {
        let before = state(1500.0, 350.0);
        let after = before.updated(&[(state(1500.0, 60.0), 1.0)]);
        let damped = before.damped(after, share(0.1));
        assert!(after.rating - before.rating > 100.0, "{after:?}");
        assert!(
            ((damped.rating - before.rating) - 0.1 * (after.rating - before.rating)).abs() < 1e-9,
            "{damped:?}"
        );
        assert_eq!(damped.deviation, after.deviation);
        assert_eq!(damped.volatility, after.volatility);
    }

    #[test]
    fn the_full_share_returns_the_computed_standing_bit_for_bit() {
        let mut seed = 7_u64;
        for _ in 0..2_000 {
            let before = state(
                500.0 + next_unit(&mut seed) * 2_000.0,
                30.0 + next_unit(&mut seed) * 320.0,
            );
            let opponent = state(500.0 + next_unit(&mut seed) * 2_000.0, 60.0);
            let after = before.updated(&[(opponent, (next_unit(&mut seed) * 2.0).floor() / 2.0)]);
            let damped = before.damped(after, RatingShare::FULL);
            assert_eq!(
                damped.rating.to_bits(),
                after.rating.to_bits(),
                "{before:?}"
            );
            assert_eq!(damped.deviation.to_bits(), after.deviation.to_bits());
            assert_eq!(damped.volatility.to_bits(), after.volatility.to_bits());
        }
    }

    /// The scaled change keeps the sign of the computed one and never exceeds it, for any standing,
    /// opponent, score and share. The generator is seeded with a fixed number so that a failure
    /// reproduces.
    #[test]
    fn a_scaled_change_never_has_the_opposite_sign_of_the_computed_one_and_never_exceeds_it() {
        let mut seed = 0x5eed_u64;
        for _ in 0..20_000 {
            let before = state(
                200.0 + next_unit(&mut seed) * 2_600.0,
                25.0 + next_unit(&mut seed) * 325.0,
            );
            let opponent = state(
                200.0 + next_unit(&mut seed) * 2_600.0,
                25.0 + next_unit(&mut seed) * 325.0,
            );
            let score = (next_unit(&mut seed) * 3.0).floor() / 2.0;
            let value = share(1e-6 + next_unit(&mut seed) * (1.0 - 1e-6));
            let after = before.updated(&[(opponent, score)]);
            let damped = before.damped(after, value);
            let computed = after.rating - before.rating;
            let scaled = damped.rating - before.rating;
            assert!(
                scaled * computed >= 0.0,
                "{before:?} vs {opponent:?} score {score} share {value:?}: {scaled} against {computed}"
            );
            assert!(
                scaled.abs() <= computed.abs(),
                "{before:?} vs {opponent:?} score {score} share {value:?}: {scaled} against {computed}"
            );
        }
    }

    /// The example configuration's tenth: what a bot gains from a first win and, once its deviation
    /// has settled, from every win, against an established opponent of equal rating.
    #[test]
    fn a_tenth_gives_a_bot_about_seventeen_points_for_a_first_win_and_one_for_a_settled_one() {
        let established = state(1500.0, 60.0);
        for (deviation, expected) in [(350.0, 17.5), (60.0, 1.02)] {
            let before = state(1500.0, deviation);
            let after = before.updated(&[(established, 1.0)]);
            let damped = before.damped(after, share(0.1));
            assert!(
                (damped.rating - before.rating - expected).abs() < 0.01,
                "deviation {deviation}: {}",
                damped.rating - before.rating
            );
        }
    }

    #[test]
    fn a_share_outside_zero_exclusive_to_one_inclusive_is_refused_with_its_value() {
        for (value, shown) in [
            (0.0, "0"),
            (-0.1, "-0.1"),
            (1.0000001, "1.0000001"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "inf"),
        ] {
            assert_eq!(
                RatingShare::try_from(value),
                Err(format!(
                    "a rating share must be greater than 0 and at most 1, not {shown}"
                ))
            );
        }
        assert_eq!(RatingShare::try_from(1.0), Ok(RatingShare::FULL));
        assert!(RatingShare::try_from(f64::MIN_POSITIVE).is_ok());
    }

    /// A uniform number in [0, 1) from a xorshift state.
    fn next_unit(seed: &mut u64) -> f64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 11) as f64 / (1_u64 << 53) as f64
    }
}
