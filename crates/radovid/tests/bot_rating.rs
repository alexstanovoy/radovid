//! A bot account's rating moves by its configured share of what the algorithm computes, for the
//! whole service path: the result a client is sent, its history, the stored ratings and restarts.
mod common;

use common::*;
use radovid::db::Db;
use radovid::rating::{Glicko, RatingShare};
use radovid_client::proto::*;

fn ranked() -> RulesetId {
    RulesetId("ranked".into())
}

fn share(value: f64) -> RatingShare {
    RatingShare::try_from(value).expect("a valid share")
}

fn start_sharing(bot_share: f64, behaviour: &str) -> Env {
    start_configured(vec![ruleset("ranked", true, behaviour, "0")], |config| {
        config.rating.bot_share = share(bot_share);
        // A rematch of players a game apart in rating must pair at once.
        config.limits.ranked_window.initial = Some(10_000);
        config.limits.ranked_window.maximum = Some(10_000);
        config.limits.unranked_window.initial = Some(10_000);
        config.limits.unranked_window.maximum = Some(10_000);
    })
}

/// One rated match; the result both seats were sent.
fn play(first: &mut Account, second: &mut Account) -> MatchResult {
    pair_up(first, second, &ranked());
    let result = first.wait_result();
    assert_eq!(
        second.wait_result(),
        result,
        "both seats are sent one result"
    );
    result
}

fn seat_of(result: &MatchResult, account: &Account) -> SeatResult {
    result
        .seats
        .iter()
        .find(|seat| seat.account == account.me.id)
        .expect("a seat of the match")
        .clone()
}

fn history_seat(account: &mut Account, match_id: MatchId) -> SeatResult {
    let me = account.me.id;
    match account
        .ask(ClientMsg::RecentMatches {
            account: me,
            limit: 10,
        })
        .expect("history")
    {
        Reply::Matches { list } => list
            .into_iter()
            .find(|result| result.match_id == match_id)
            .map(|result| seat_of(&result, account))
            .expect("the match in the history"),
        other => panic!("unexpected {other:?}"),
    }
}

fn change(seat: &SeatResult) -> f64 {
    seat.after.expect("rated").rating - seat.before.expect("rated").rating
}

fn stored(env: &Env, account: &Account) -> Glicko {
    env.block_on(async {
        Db::open(&env.db_path())
            .expect("open")
            .rating(account.me.id, ranked())
            .await
            .expect("rating")
            .expect("played")
            .glicko
    })
}

#[test]
fn a_human_moves_by_the_whole_change_and_a_bot_by_its_share_in_the_result_and_the_history() {
    for behaviour in ["team0", "team1"] {
        let (whole_env, scaled_env) =
            (start_sharing(1.0, behaviour), start_sharing(0.1, behaviour));
        let mut seats = Vec::new();
        for env in [&whole_env, &scaled_env] {
            let (mut human, mut bot) = (env.human("human"), env.bot("bot"));
            let result = play(&mut human, &mut bot);
            let (human_seat, bot_seat) = (seat_of(&result, &human), seat_of(&result, &bot));
            assert_eq!(history_seat(&mut human, result.match_id), human_seat);
            assert_eq!(history_seat(&mut bot, result.match_id), bot_seat);
            seats.push((human_seat, bot_seat));
        }
        let ((whole_human, whole_bot), (scaled_human, scaled_bot)) = (&seats[0], &seats[1]);
        assert_eq!(
            scaled_human, whole_human,
            "{behaviour}: the human's update is untouched"
        );
        assert!(
            change(whole_bot).abs() > 100.0,
            "{behaviour}: a first game moves far"
        );
        let expected = 0.1 * change(whole_bot);
        assert!(
            (change(scaled_bot) - expected).abs() < 1e-9,
            "{behaviour}: {} against {expected}",
            change(scaled_bot)
        );
        assert_eq!(
            scaled_bot.after.expect("rated").deviation,
            whole_bot.after.expect("rated").deviation,
            "{behaviour}: the deviation follows the algorithm"
        );
    }
}

#[test]
fn two_bots_in_one_rated_match_each_move_by_their_share() {
    let (whole_env, scaled_env) = (start_sharing(1.0, "team0"), start_sharing(0.25, "team0"));
    let mut changes = Vec::new();
    for env in [&whole_env, &scaled_env] {
        let (mut first, mut second) = (env.bot("one"), env.bot("two"));
        let result = play(&mut first, &mut second);
        changes.push([
            change(&seat_of(&result, &first)),
            change(&seat_of(&result, &second)),
        ]);
    }
    for (whole, scaled) in changes[0].iter().zip(&changes[1]) {
        assert!(whole.abs() > 100.0, "a first game moves far: {whole}");
        assert!(
            (scaled - 0.25 * whole).abs() < 1e-9,
            "{scaled} against {whole}"
        );
    }
}

#[test]
fn a_restart_with_another_share_keeps_every_finished_rating_and_rates_the_next_match_with_the_new_one()
 {
    let mut env = start_sharing(0.1, "team0");
    let (mut human, mut bot) = (env.human("human"), env.bot("bot"));
    let first = play(&mut human, &mut bot);
    let (human_seat, bot_seat) = (seat_of(&first, &human), seat_of(&first, &bot));
    let (human_before, bot_before) = (stored(&env, &human), stored(&env, &bot));
    drop((human, bot));

    env.restart_with(|config| config.rating.bot_share = share(0.5));
    let (mut human, mut bot) = (env.login("human"), env.login("bot"));
    assert_eq!(history_seat(&mut human, first.match_id), human_seat);
    assert_eq!(history_seat(&mut bot, first.match_id), bot_seat);
    assert_eq!(
        (stored(&env, &human), stored(&env, &bot)),
        (human_before, bot_before)
    );

    let second = play(&mut human, &mut bot);
    let (human_after, bot_after) = (stored(&env, &human), stored(&env, &bot));
    // The winner is the same team, so the human's seat tells which side the bot is on.
    let human_won =
        matches!(second.outcome, Outcome::Winner { team } if team == seat_of(&second, &human).team);
    let score = if human_won { 1.0 } else { 0.0 };
    let plain_bot = bot_before.updated_match(&[(human_before, 1.0 - score)]);
    // The two matches end some milliseconds apart, which grows each deviation by a hair.
    let plain_human = human_before.updated_match(&[(bot_before, score)]);
    assert!(
        (human_after.rating - plain_human.rating).abs() < 1e-4,
        "{human_after:?} against {plain_human:?}"
    );
    let expected = bot_before.rating + 0.5 * (plain_bot.rating - bot_before.rating);
    assert!(
        (bot_after.rating - expected).abs() < 1e-4,
        "{bot_after:?} from {bot_before:?}, expected {expected}"
    );
}
