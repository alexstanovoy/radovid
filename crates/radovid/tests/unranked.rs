//! An unrated queue uses its mode's rated ladder for matching, without writing it.
mod common;

use common::*;
use radovid_client::proto::*;

#[test]
fn an_unranked_game_uses_the_rated_ladder_and_leaves_every_rating_unchanged() {
    let ranked = ruleset("ranked", true, "team0", "0.3");
    let mut casual = ruleset("casual", false, "team0", "0.3");
    casual.rating_ruleset = Some("ranked".to_owned());
    let env = start(vec![ranked, casual]);
    let mut alpha = env.bot("alpha");
    let mut beta = env.bot("beta");
    let rated = RulesetId("ranked".to_owned());
    pair_up(&mut alpha, &mut beta, &rated);
    let before = alpha.wait_result();
    beta.wait_result();
    assert!(before.rated);
    let board_before = alpha.ask(ClientMsg::Ladder { ruleset: rated.clone(), offset: 0, limit: 10 }).expect("ladder");

    let (first, second) = pair_up(&mut alpha, &mut beta, &RulesetId("casual".to_owned()));
    assert!(!first.rated);
    assert!(!second.rated);
    assert_eq!(first.seats[0].rating, before.seats[0].after);
    assert_eq!(first.seats[1].rating, before.seats[1].after);
    let result = alpha.wait_result();
    beta.wait_result();
    assert!(!result.rated);
    assert_eq!(result.outcome, Outcome::Winner { team: 0 });
    assert!(result.seats.iter().all(|seat| seat.before.is_none() && seat.after.is_none()));
    assert_eq!(alpha.ask(ClientMsg::Ladder { ruleset: rated, offset: 0, limit: 10 }).expect("ladder"), board_before);
    assert_eq!(alpha.ask(ClientMsg::Ladder { ruleset: RulesetId("casual".to_owned()), offset: 0, limit: 10 }), Err(Error::NoQueue));
}
