//! Deterministic selection from one queue at one instant. No game rules or database I/O.

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeSet, HashMap};
use std::net::IpAddr;

use radovid_proto::{AccountId, QueueEntryId};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Candidate {
    pub entry: QueueEntryId,
    pub account: AccountId,
    pub source: IpAddr,
    pub rating: f64,
    pub window: u32,
    pub order: usize,
}

pub(crate) fn groups(mut candidates: Vec<Candidate>, seats: usize, maximum: usize, work: &mut usize) -> Vec<Vec<QueueEntryId>> {
    assert!((2..=10).contains(&seats));
    let mut selected = Vec::new();
    while selected.len() < maximum && candidates.len() >= seats && candidates.len() <= *work {
        *work -= candidates.len();
        let Some(picked) = pick(&candidates, seats) else { break; };
        candidates.retain(|player| !picked.contains(&player.entry));
        selected.push(picked);
    }
    selected
}

/// Rebuild for each group: a removed member must not leave a narrow entry hiding compatible ones.
fn pick(candidates: &[Candidate], seats: usize) -> Option<Vec<QueueEntryId>> {
    let players = oldest_candidates(candidates);
    let mut active = BTreeSet::new();
    let mut best = None;
    for (index, player) in players.iter().enumerate() {
        let key = Rated { rating: player.rating, index };
        active.insert(key);
        if let Some(choice) = best_near(&active, key, &players, seats, index)
            && best.as_ref().is_none_or(|previous| choice > *previous) {
            best = Some(choice);
        }
    }
    let mut by_age: Vec<_> = (0..players.len()).collect();
    by_age.sort_unstable_by_key(|&index| players[index].order);
    let mut members = refine_choice(best?, &players, &by_age).members.0;
    members.sort_unstable_by_key(|&index| players[index].order);
    Some(members.into_iter().map(|index| players[index].entry).collect())
}

fn oldest_candidates(candidates: &[Candidate]) -> Vec<Candidate> {
    let mut oldest: HashMap<AccountId, Candidate> = HashMap::new();
    for &candidate in candidates {
        assert!(candidate.rating.is_finite());
        oldest.entry(candidate.account).and_modify(|previous| {
            if candidate.order < previous.order { *previous = candidate; }
        }).or_insert(candidate);
    }
    let mut players: Vec<_> = oldest.into_values().collect();
    players.sort_unstable_by(|left, right| right.window.cmp(&left.window)
        .then_with(|| left.order.cmp(&right.order)).then_with(|| left.entry.0.cmp(&right.entry.0)));
    players
}

/// A total order without an external float-ordering dependency. Only finite ratings enter it.
#[derive(Clone, Copy, Debug)]
struct Rated { rating: f64, index: usize }

impl PartialEq for Rated {
    fn eq(&self, other: &Self) -> bool { self.cmp(other) == Ordering::Equal }
}
impl Eq for Rated {}
impl PartialOrd for Rated {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}
impl Ord for Rated {
    fn cmp(&self, other: &Self) -> Ordering { self.rating.total_cmp(&other.rating).then_with(|| self.index.cmp(&other.index)) }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Choice {
    oldest: Reverse<usize>,
    sources: usize,
    newest: Reverse<usize>,
    members: Reverse<Vec<usize>>,
}

fn best_near(active: &BTreeSet<Rated>, key: Rated, players: &[Candidate], seats: usize, required: usize) -> Option<Choice> {
    let mut near: Vec<_> = active.range(..key).rev().take(seats - 1).copied().collect();
    near.extend(active.range(key..).take(seats).copied());
    near.sort_unstable();
    near.windows(seats).filter_map(|window| {
        if !window.iter().any(|rated| rated.index == required) { return None; }
        choice_for(window.iter().map(|rated| rated.index).collect(), players)
    }).max()
}

fn choice_for(mut members: Vec<usize>, players: &[Candidate]) -> Option<Choice> {
    let low = members.iter().map(|&index| players[index].rating).reduce(f64::min)?;
    let high = members.iter().map(|&index| players[index].rating).reduce(f64::max)?;
    let accepted = members.iter().map(|&index| players[index].window).min()?;
    if high - low > f64::from(accepted) { return None; }
    members.sort_unstable();
    let sources = members.iter().enumerate().filter(|(position, index)| {
        members[..*position].iter().all(|&previous| players[previous].source != players[**index].source)
    }).count();
    Some(Choice {
        sources,
        oldest: Reverse(members.iter().map(|&index| players[index].order).min()?),
        newest: Reverse(members.iter().map(|&index| players[index].order).max()?),
        members: Reverse(members),
    })
}

fn refine_choice(mut choice: Choice, players: &[Candidate], by_age: &[usize]) -> Choice {
    for &index in by_age {
        if choice.members.0.contains(&index) { continue; }
        let original = choice.members.0.clone();
        for position in 0..original.len() {
            let mut replacement = original.clone();
            replacement[position] = index;
            if let Some(improved) = choice_for(replacement, players)
                && improved > choice {
                choice = improved;
            }
        }
    }
    choice
}

#[cfg(test)]
mod tests {
    use super::*;
    use radovid_proto::RatingWindow;

    fn groups(candidates: Vec<Candidate>, seats: usize, maximum: usize) -> Vec<Vec<QueueEntryId>> {
        super::groups(candidates, seats, maximum, &mut 65_536)
    }

    #[test]
    fn repeated_selection_cannot_exceed_the_scan_budget() {
        let players: Vec<_> = (1..=100).map(|entry| candidate(entry, 1500.0, 100)).collect();
        let mut budget = 198;
        assert_eq!(super::groups(players, 2, 50, &mut budget).len(), 2);
        assert_eq!(budget, 0);
    }

    #[test]
    fn selection_finds_the_oldest_eligible_anchor_across_small_mixed_windows() {
        for seed in 0u64..512 {
            let mut state = seed + 1;
            let players: Vec<_> = (1..=6).map(|entry| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                candidate(entry, ((state >> 32) % 4) as f64 * 50.0, ((state >> 40) % 4) as u32 * 50)
            }).collect();
            for seats in 2..=4 {
                let expected = (1u32..64).filter(|bits| bits.count_ones() as usize == seats)
                    .filter_map(|bits| {
                        let members: Vec<_> = (0..6).filter(|index| bits & (1 << index) != 0).collect();
                        choice_for(members, &players).map(|choice| choice.oldest.0)
                    }).min();
                let selected = groups(players.clone(), seats, 1);
                let actual = selected.first().map(|ids| ids.iter().map(|id| id.0 as usize).min().expect("nonempty"));
                assert_eq!(actual, expected, "seed={seed} seats={seats} players={players:?}");
            }
        }
    }

    fn candidate(entry: u32, rating: f64, window: u32) -> Candidate {
        Candidate { entry: QueueEntryId(entry), account: AccountId(u64::from(entry)),
            source: "127.0.0.1".parse().expect("source"), rating, window, order: entry as usize }
    }

    #[test]
    fn ranked_players_outside_the_initial_window_are_not_matched() {
        assert!(groups(vec![candidate(1, 1500.0, 100), candidate(2, 1700.0, 100)], 2, 1).is_empty());
    }

    #[test]
    fn widening_with_wait_allows_a_match_and_stops_at_the_configured_maximum() {
        let window = RatingWindow { initial: 100, widen_per_minute: 100, maximum: 300 };
        assert_eq!(window.after_ms(0), 100);
        assert_eq!(window.after_ms(30_000), 150);
        assert_eq!(window.after_ms(60_000), 200);
        assert_eq!(window.after_ms(u32::MAX), 300);
        assert_eq!(groups(vec![candidate(1, 1500.0, window.after_ms(60_000)), candidate(2, 1700.0, window.after_ms(60_000))], 2, 1), vec![vec![QueueEntryId(1), QueueEntryId(2)]]);
    }

    #[test]
    fn an_old_wide_window_cannot_force_a_new_player_outside_its_own_window() {
        assert!(groups(vec![candidate(1, 1500.0, 1000), candidate(2, 1800.0, 100)], 2, 1).is_empty());
    }

    #[test]
    fn the_looser_window_supports_team_games_and_three_way_games() {
        let players: Vec<_> = [1500.0, 1700.0, 1900.0, 2100.0].into_iter().enumerate()
            .map(|(index, rating)| candidate(index as u32 + 1, rating, 600)).collect();
        assert_eq!(groups(players.clone(), 4, 1).len(), 1);
        assert_eq!(groups(players, 3, 1).len(), 1);
    }

    #[test]
    fn a_narrow_newcomer_does_not_block_compatible_older_players() {
        let players = vec![candidate(1, 0.0, 100), candidate(2, 0.0, 0), candidate(3, 50.0, 100), candidate(4, 100.0, 100)];
        assert_eq!(groups(players, 3, 1), vec![vec![QueueEntryId(1), QueueEntryId(3), QueueEntryId(4)]]);
    }

    #[test]
    fn one_account_cannot_fill_multiple_seats_and_its_oldest_place_is_used() {
        let mut players = vec![candidate(1, 1500.0, 100), candidate(2, 1500.0, 100), candidate(3, 1500.0, 100)];
        players[1].account = players[0].account;
        assert_eq!(groups(players, 2, 1), vec![vec![QueueEntryId(1), QueueEntryId(3)]]);
    }

    #[test]
    fn a_compatible_different_source_is_preferred_to_self_matching() {
        let mut players = vec![candidate(1, 1500.0, 100), candidate(2, 1500.0, 100), candidate(3, 1500.0, 100)];
        players[2].source = "127.0.0.2".parse().expect("other source");
        assert_eq!(groups(players, 2, 1), vec![vec![QueueEntryId(1), QueueEntryId(3)]]);
    }

    #[test]
    fn selected_groups_do_not_overlap_and_respect_available_capacity() {
        let players: Vec<_> = (1..=8).map(|entry| candidate(entry, 1500.0, 100)).collect();
        assert_eq!(groups(players, 2, 2), vec![vec![QueueEntryId(1), QueueEntryId(2)], vec![QueueEntryId(3), QueueEntryId(4)]]);
    }

    #[test]
    fn a_narrow_entry_cannot_hide_a_second_compatible_group_after_removal() {
        let mut players = vec![candidate(1, 0.0, 100), candidate(2, 10.0, 100), candidate(3, 20.0, 100), candidate(4, 30.0, 100), candidate(5, 20.0, 0)];
        for player in &mut players[2..] { player.source = "127.0.0.2".parse().expect("other source"); }
        assert_eq!(groups(players, 2, 2), vec![vec![QueueEntryId(1), QueueEntryId(3)], vec![QueueEntryId(2), QueueEntryId(4)]]);
    }

    #[test]
    fn independent_searches_of_one_account_can_fill_separate_matches() {
        let mut players: Vec<_> = (1..=4).map(|entry| candidate(entry, 1500.0, 100)).collect();
        players[1].account = players[0].account;
        assert_eq!(groups(players, 2, 2), vec![vec![QueueEntryId(1), QueueEntryId(3)], vec![QueueEntryId(2), QueueEntryId(4)]]);
    }

    #[test]
    fn old_compatible_players_are_not_starved_by_newer_distinct_sources() {
        let mut players = vec![candidate(1, 0.0, 100), candidate(2, 0.0, 100), candidate(3, 1000.0, 100), candidate(4, 1000.0, 100)];
        players[2].source = "127.0.0.2".parse().expect("other source");
        players[3].source = "127.0.0.3".parse().expect("third source");
        assert_eq!(groups(players, 2, 1), vec![vec![QueueEntryId(1), QueueEntryId(2)]]);
    }
}
