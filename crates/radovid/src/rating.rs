//! Glicko-2 with the standard constants (Glickman, 2013), one rating period
//! per game, and the growth of the deviation with idle time.

use radovid_proto::Rating;

/// Rating of an account that never played.
pub const INITIAL_RATING: f64 = 1500.0;
/// Deviation of an account that never played; also the most a deviation grows to.
pub const INITIAL_DEVIATION: f64 = 350.0;
/// Volatility of an account that never played.
pub const INITIAL_VOLATILITY: f64 = 0.06;
/// How strongly the volatility may move.
const TAU: f64 = 0.5;
/// Glicko-2's internal scale factor.
const SCALE: f64 = 173.7178;
const EPSILON: f64 = 1e-6;
/// The Illinois iteration is bounded; it converges in a handful of steps.
const MAX_ITERATIONS: u32 = 100;

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
        let phi = self.deviation / SCALE;
        let grown = (phi * phi + self.volatility * self.volatility * idle_days.max(0.0)).sqrt();
        Glicko {
            deviation: (grown * SCALE).min(INITIAL_DEVIATION),
            ..self
        }
    }

    /// The standing after one rating period of games against `opponents`, each
    /// with the score of the player (1 win, 0.5 draw, 0 loss).
    pub fn updated(self, opponents: &[(Glicko, f64)]) -> Glicko {
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
}
