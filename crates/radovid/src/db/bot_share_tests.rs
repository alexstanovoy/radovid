//! A bot account receives only its configured share of a computed rating change.
use super::results_tests::{Scratch, finish, fixture_of};
use super::*;

const DUEL: &str = "duel";

fn ruleset() -> RulesetId {
    RulesetId(DUEL.into())
}

fn share(value: f64) -> RatingShare {
    RatingShare::try_from(value).expect("a valid share")
}

/// Plays `outcomes` between the two seats of a fresh database, all finished at the same moment so
/// that no idle time grows a deviation between games.
async fn play(
    kinds: [AccountKind; 2],
    bot_share: RatingShare,
    outcomes: &[Outcome],
) -> (Scratch, Db, Vec<NewSeat>) {
    let (directory, db, first_game, seats) = fixture_of(kinds).await;
    for (index, outcome) in outcomes.iter().enumerate() {
        let game = if index == 0 {
            first_game
        } else {
            db.create_match(ruleset(), true, seats.clone())
                .await
                .expect("another match")
        };
        let mut result = finish(game, seats.clone());
        result.outcome = *outcome;
        result.bot_share = bot_share;
        db.finish_match(result).await.expect("rated finish");
    }
    (directory, db, seats)
}

async fn stored(db: &Db, seat: &NewSeat) -> Glicko {
    db.rating(seat.account, ruleset())
        .await
        .expect("rating")
        .expect("played")
        .glicko
}

async fn history(db: &Db, seat: &NewSeat) -> Vec<MatchResult> {
    db.public_recent_matches(seat.account, seat.account, 10, vec![ruleset()])
        .await
        .expect("history")
        .expect("own history is visible")
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "{what}: {actual} against {expected}"
    );
}

#[tokio::test]
async fn a_bot_seat_receives_its_share_of_the_change_while_a_human_opponent_receives_all_of_it() {
    let kinds = [AccountKind::Human, AccountKind::Bot];
    let win = [Outcome::Winner { team: 0 }];
    let (_whole, plain, seats) = play(kinds, RatingShare::FULL, &win).await;
    let (_tenth, damped, _) = play(kinds, share(0.1), &win).await;
    let (human_plain, human_damped) = (
        stored(&plain, &seats[0]).await,
        stored(&damped, &seats[0]).await,
    );
    let (bot_plain, bot_damped) = (
        stored(&plain, &seats[1]).await,
        stored(&damped, &seats[1]).await,
    );
    assert_eq!(human_damped, human_plain, "the human's update is untouched");
    assert!(
        bot_plain.rating < 1400.0,
        "the plain loss is large: {bot_plain:?}"
    );
    assert_close(
        bot_damped.rating - 1500.0,
        0.1 * (bot_plain.rating - 1500.0),
        "the bot's rating change",
    );
    assert_eq!(bot_damped.deviation, bot_plain.deviation);
    assert_eq!(bot_damped.volatility, bot_plain.volatility);
    let human_seat = &history(&damped, &seats[0]).await[0].seats[0];
    let bot_seat = &history(&damped, &seats[1]).await[0].seats[1];
    let start = Some(Rating {
        rating: 1500.0,
        deviation: 350.0,
    });
    assert_eq!(
        (human_seat.before, human_seat.after),
        (start, Some(human_damped.public()))
    );
    assert_eq!(
        (bot_seat.before, bot_seat.after),
        (start, Some(bot_damped.public()))
    );
}

#[tokio::test]
async fn a_human_who_beats_a_bot_is_rated_against_the_bots_stored_rating_and_deviation() {
    let kinds = [AccountKind::Human, AccountKind::Bot];
    let (_directory, db, seats) = play(kinds, share(0.1), &[Outcome::Winner { team: 0 }]).await;
    let (human, bot) = (stored(&db, &seats[0]).await, stored(&db, &seats[1]).await);
    let game = db
        .create_match(ruleset(), true, seats.clone())
        .await
        .expect("second match");
    let mut second = finish(game, seats.clone());
    second.bot_share = share(0.1);
    db.finish_match(second).await.expect("second finish");
    let plain_human = human.updated_match(&[(bot, 1.0)]);
    let plain_bot = bot.updated_match(&[(human, 0.0)]);
    assert_eq!(stored(&db, &seats[0]).await, plain_human);
    let after = stored(&db, &seats[1]).await;
    assert_close(
        after.rating - bot.rating,
        0.1 * (plain_bot.rating - bot.rating),
        "the bot's second change",
    );
}

#[tokio::test]
async fn two_bots_each_receive_their_share_and_a_draw_between_unequal_bots_is_scaled_too() {
    let kinds = [AccountKind::Bot; 2];
    let script = [Outcome::Winner { team: 0 }, Outcome::Draw];
    let (_plain_dir, plain, seats) = play(kinds, RatingShare::FULL, &script[..1]).await;
    let (_damped_dir, damped, _) = play(kinds, share(0.25), &script[..1]).await;
    for seat in &seats {
        let (whole, part) = (stored(&plain, seat).await, stored(&damped, seat).await);
        assert_close(
            part.rating - 1500.0,
            0.25 * (whole.rating - 1500.0),
            "one side of a bot duel",
        );
    }
    let (winner, loser) = (
        stored(&damped, &seats[0]).await,
        stored(&damped, &seats[1]).await,
    );
    let game = damped
        .create_match(ruleset(), true, seats.clone())
        .await
        .expect("draw match");
    let mut drawn = finish(game, seats.clone());
    drawn.outcome = script[1];
    drawn.bot_share = share(0.25);
    damped.finish_match(drawn).await.expect("draw finish");
    for (before, opponent, seat) in [(winner, loser, &seats[0]), (loser, winner, &seats[1])] {
        let computed = before.updated_match(&[(opponent, 0.5)]);
        assert!(
            (computed.rating - before.rating).abs() > 1.0,
            "the draw moves unequal bots"
        );
        assert_close(
            stored(&damped, seat).await.rating - before.rating,
            0.25 * (computed.rating - before.rating),
            "a drawn bot's change",
        );
    }
}

#[tokio::test]
async fn an_unrated_match_changes_no_rating_whatever_the_share() {
    let (_directory, db, _game, seats) = fixture_of([AccountKind::Human, AccountKind::Bot]).await;
    let game = db
        .create_match(ruleset(), false, seats.clone())
        .await
        .expect("unrated match");
    let mut unrated = finish(game, seats.clone());
    unrated.rate = false;
    unrated.bot_share = share(0.1);
    db.finish_match(unrated).await.expect("unrated finish");
    for seat in &seats {
        assert!(
            db.rating(seat.account, ruleset())
                .await
                .expect("rating")
                .is_none(),
            "no rating row for an unrated game"
        );
    }
}

#[tokio::test]
async fn the_full_share_rates_a_bot_bit_for_bit_as_it_rates_a_human() {
    let script = [
        Outcome::Winner { team: 0 },
        Outcome::Draw,
        Outcome::Winner { team: 1 },
        Outcome::Winner { team: 0 },
    ];
    let (_human_dir, humans, human_seats) =
        play([AccountKind::Human; 2], RatingShare::FULL, &script).await;
    let (_bot_dir, bots, bot_seats) = play(
        [AccountKind::Human, AccountKind::Bot],
        RatingShare::FULL,
        &script,
    )
    .await;
    for (human_seat, bot_seat) in human_seats.iter().zip(&bot_seats) {
        let (expected, actual) = (
            stored(&humans, human_seat).await,
            stored(&bots, bot_seat).await,
        );
        for (left, right) in [
            (expected.rating, actual.rating),
            (expected.deviation, actual.deviation),
            (expected.volatility, actual.volatility),
        ] {
            assert_eq!(
                left.to_bits(),
                right.to_bits(),
                "{expected:?} against {actual:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_pending_result_is_rated_with_the_share_it_was_finished_under_after_a_restart() {
    let kinds = [AccountKind::Human, AccountKind::Bot];
    let win = [Outcome::Winner { team: 0 }];
    let (_plain_dir, plain, seats) = play(kinds, RatingShare::FULL, &win).await;
    let (_control_dir, control, _) = play(kinds, share(0.1), &win).await;

    let (directory, db, game, pending_seats) = fixture_of(kinds).await;
    let mut result = finish(game, pending_seats.clone());
    result.bot_share = share(0.1);
    db.queue_finish(result).await.expect("durable record");
    drop(db);
    let reopened = Db::open(&directory.0.join("main.db")).expect("reopen");
    assert_eq!(reopened.recover_results().await.expect("replay"), 1);

    let replayed = stored(&reopened, &pending_seats[1]).await;
    assert_eq!(replayed, stored(&control, &seats[1]).await);
    assert_ne!(
        replayed.rating,
        stored(&plain, &seats[1]).await.rating,
        "the replay must not fall back to the whole change"
    );
}

#[test]
fn a_pending_record_of_the_format_without_a_share_is_refused_by_name() {
    let mut record = b"RDVRES1\0".to_vec();
    record.extend_from_slice(&[0; 64]);
    let error = decode_pending_result(&record).expect_err("an older format");
    assert_eq!(
        error.to_string(),
        "database file: pending-result record was written by an older version; start that version to commit it first"
    );
}
