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

/// What a share must be, for the messages that refuse one.
pub const SHARE_RULE: &str = "greater than 0 and at most 1";

/// The share of a computed rating change that an account receives: greater than 0 and at most 1.
/// A finished match keeps it in its pending result record until the database commit (the committed
/// rows hold only the before and after standings), so a record replayed after a restart is rated
/// with the share it was finished under, whatever the configuration says by then.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct RatingShare(f64);

impl RatingShare {
    /// The whole change: the undamped algorithm.
    pub const FULL: RatingShare = RatingShare(1.0);

    pub fn get(self) -> f64 {
        self.0
    }
}

/// A quarter, for now: the first win of a new bot version is worth about 44 points and a settled
/// one about 2.6, and a version takes about 23 games to reach its place.
impl Default for RatingShare {
    fn default() -> Self {
        RatingShare(0.25)
    }
}

impl TryFrom<f64> for RatingShare {
    type Error = String;

    fn try_from(share: f64) -> Result<Self, String> {
        // Written positively so that NaN is refused too.
        if share > 0.0 && share <= 1.0 {
            Ok(RatingShare(share))
        } else {
            Err(format!("a rating share must be {SHARE_RULE}, not {share}"))
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

    /// The standing `share` of the way from `self` to `after`, which the algorithm computed from
    /// `self`: rating, deviation and volatility all move the same part of their step, so the
    /// result is the algorithm's state a fraction of one game along. A bot whose rating moved a
    /// tenth as far is thereby also only a tenth more certain, and its volatility runs on the same
    /// clock as its deviation.
    pub fn damped(self, after: Glicko, share: RatingShare) -> Glicko {
        Glicko {
            rating: toward(self.rating, after.rating, share),
            deviation: toward(self.deviation, after.deviation, share),
            volatility: toward(self.volatility, after.volatility, share),
        }
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

/// `from` moved `share` of the way to `to`. For finite inputs the result is finite and between
/// them, and the full share returns `to` bit for bit: it is returned before any arithmetic, which
/// would turn `-0.0` into `0.0` and overflow on extreme pairs. The step is taken from `to`; when
/// the difference itself overflows, the convex combination is used, whose terms cannot. The clamp
/// keeps rounding from carrying the result past either end.
fn toward(from: f64, to: f64, share: RatingShare) -> f64 {
    let share = share.get();
    if share == 1.0 {
        return to;
    }
    let step = to - from;
    let moved = if step.is_finite() {
        to - (1.0 - share) * step
    } else {
        (1.0 - share) * from + share * to
    };
    moved.clamp(from.min(to), from.max(to))
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
    fn a_share_below_one_moves_rating_deviation_and_volatility_that_part_of_their_step() {
        let before = Glicko {
            volatility: 0.07,
            ..state(1500.0, 350.0)
        };
        let after = before.updated(&[(state(1500.0, 60.0), 1.0)]);
        let damped = before.damped(after, share(0.1));
        assert!(after.rating - before.rating > 100.0, "{after:?}");
        assert!(after.deviation < before.deviation - 50.0, "{after:?}");
        assert!(after.volatility != before.volatility, "{after:?}");
        for (name, from, to, got) in [
            ("rating", before.rating, after.rating, damped.rating),
            (
                "deviation",
                before.deviation,
                after.deviation,
                damped.deviation,
            ),
            (
                "volatility",
                before.volatility,
                after.volatility,
                damped.volatility,
            ),
        ] {
            assert!(
                (got - from - 0.1 * (to - from)).abs() < 1e-12,
                "{name}: {got} from {from} towards {to}"
            );
        }
    }

    #[test]
    fn moving_towards_any_finite_value_gives_a_finite_result_between_the_two_and_the_full_share_gives_the_target()
     {
        let values = [
            f64::MAX,
            -f64::MAX,
            1e308,
            -1e308,
            1e-308,
            -1e-308,
            f64::MIN_POSITIVE,
            5e-324,
            -5e-324,
            0.0,
            -0.0,
            1500.0,
            -42.5,
        ];
        let shares = [
            RatingShare::FULL,
            share(0.5),
            share(0.25),
            share(f64::MIN_POSITIVE),
            share(1.0 - f64::EPSILON),
        ];
        for from in values {
            for to in values {
                for value in shares {
                    let moved = toward(from, to, value);
                    let context = format!("{from:e} towards {to:e} at {value:?}: {moved:e}");
                    assert!(moved.is_finite(), "{context}");
                    assert!(moved >= from.min(to) && moved <= from.max(to), "{context}");
                    if value == RatingShare::FULL {
                        assert_eq!(moved.to_bits(), to.to_bits(), "{context}");
                    }
                }
            }
        }
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

    /// Every quantity of the damped standing lies between the old one and the computed one: the
    /// change keeps its sign and never exceeds the computed change, so a deviation never grows
    /// from a game unless the algorithm grew it. The generator is seeded with a fixed number so
    /// that a failure reproduces.
    #[test]
    fn a_damped_standing_lies_between_the_old_and_the_computed_one_in_every_quantity() {
        let mut seed = 0x5eed_u64;
        for _ in 0..20_000 {
            let before = Glicko {
                volatility: 0.03 + next_unit(&mut seed) * 0.07,
                ..state(
                    200.0 + next_unit(&mut seed) * 2_600.0,
                    25.0 + next_unit(&mut seed) * 325.0,
                )
            };
            let opponent = state(
                200.0 + next_unit(&mut seed) * 2_600.0,
                25.0 + next_unit(&mut seed) * 325.0,
            );
            let score = (next_unit(&mut seed) * 3.0).floor() / 2.0;
            let value = share(1e-6 + next_unit(&mut seed) * (1.0 - 1e-6));
            let after = before.updated(&[(opponent, score)]);
            let damped = before.damped(after, value);
            for (name, from, to, got) in [
                ("rating", before.rating, after.rating, damped.rating),
                (
                    "deviation",
                    before.deviation,
                    after.deviation,
                    damped.deviation,
                ),
                (
                    "volatility",
                    before.volatility,
                    after.volatility,
                    damped.volatility,
                ),
            ] {
                let context =
                    format!("{name} {before:?} vs {opponent:?} score {score} share {value:?}");
                assert!(
                    (got - from) * (to - from) >= 0.0,
                    "{context}: {got} from {from} to {to}"
                );
                assert!(
                    (got - from).abs() <= (to - from).abs(),
                    "{context}: {got} from {from} to {to}"
                );
            }
        }
    }

    /// `games` rated games of a bot at `share`, alternately won and lost against an established
    /// opponent of equal rating; its deviation after each.
    fn deviations(share: RatingShare, games: usize) -> Vec<f64> {
        let opponent = state(1500.0, 60.0);
        let mut standing = Glicko::NEW;
        (0..games)
            .map(|game| {
                let score = if game % 2 == 0 { 1.0 } else { 0.0 };
                let after = standing.updated(&[(opponent, score)]);
                standing = standing.damped(after, share);
                standing.deviation
            })
            .collect()
    }

    #[test]
    fn a_bot_never_grows_certain_faster_than_the_unscaled_algorithm_and_settles_where_it_does() {
        let plain = deviations(RatingShare::FULL, 3_000);
        let settled = plain[2_999];
        for value in [0.05, 0.1, 0.25, 0.5] {
            let bot = deviations(share(value), 3_000);
            // Until the unscaled deviation has settled; after that both wander within a hundredth
            // of a point of the same value as their volatilities drift.
            for (game, (damped, whole)) in bot.iter().zip(&plain).enumerate() {
                assert!(
                    *whole < settled + 1.0 || damped >= whole,
                    "share {value}, game {}: {damped} < {whole}",
                    game + 1
                );
            }
            assert!(
                bot.windows(2).all(|pair| pair[1] <= pair[0]),
                "share {value}: a deviation grew"
            );
            assert!(
                (bot[2_999] - settled).abs() < 1.0,
                "share {value}: {} against {settled}",
                bot[2_999]
            );
        }
    }

    /// The exact statement is that each game moves the standing `share` of the way along the
    /// algorithm's step (the tests above); over many games that is, to first order in the share,
    /// the unscaled algorithm run for `share` times as many games. The match is only that
    /// approximate, since the unscaled algorithm takes whole steps: here within 10%.
    #[test]
    fn after_n_games_a_bots_deviation_is_about_that_of_the_unscaled_algorithm_after_n_times_the_share()
     {
        let plain = deviations(RatingShare::FULL, 100);
        for (value, games) in [
            (0.1, 20),
            (0.1, 100),
            (0.1, 400),
            (0.25, 8),
            (0.25, 40),
            (0.5, 20),
        ] {
            let bot = deviations(share(value), games)[games - 1];
            let equivalent = plain[(games as f64 * value).round() as usize - 1];
            assert!(
                bot >= equivalent && bot <= 1.1 * equivalent,
                "share {value} after {games} games: {bot} against {equivalent}"
            );
        }
    }

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
