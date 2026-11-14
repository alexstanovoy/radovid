//! Deterministic selection from one queue at one instant. No game rules or database I/O.

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeSet, HashMap};
use std::net::IpAddr;

use radovid_proto::{AccountId, QueueEntryId};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Candidate {
    pub entry: QueueEntryId,
    pub account: AccountId,
    pub main_account: AccountId,
    pub source: IpAddr,
    pub rating: f64,
    pub window: u32,
    pub order: usize,
}

pub(crate) fn groups(
    mut candidates: Vec<Candidate>,
    seats: usize,
    maximum: usize,
    work: &mut usize,
) -> Vec<Vec<QueueEntryId>> {
    assert!((2..=10).contains(&seats));
    let mut selected = Vec::new();
    while selected.len() < maximum && candidates.len() >= seats && candidates.len() <= *work {
        *work -= candidates.len();
        #[cfg(test)]
        LISTED.with(|listed| listed.set(listed.get() + candidates.len()));
        let Some(picked) = pick(&candidates, seats, work) else {
            break;
        };
        candidates.retain(|player| !picked.contains(&player.entry));
        selected.push(picked);
    }
    selected
}

/// The units of work one tick of pairing may spend, whatever the queues, rulesets and owners are:
/// a candidate listed costs one, a step beyond the free ones costs one. Measured, a tick that
/// spends it all takes a few milliseconds (see `FREE_STEPS_PER_CANDIDATE`); what it does not reach
/// waits for the next tick.
pub(crate) const TICK_BUDGET: usize = 8_192;

/// Steps a pick may take for free per candidate it lists: the neighbours of a candidate in a
/// normal queue. Whatever a pick takes beyond is paid from the tick's budget, step by step.
pub(crate) const FREE_STEPS_PER_CANDIDATE: usize = 8;

/// Rebuild for each group: a removed member must not leave a narrow entry hiding compatible ones.
///
/// The steps of a pick (a neighbour looked at, a version tried, a replacement tried in the
/// refinement) are bounded whatever the seats, the owners and the versions are: a candidate
/// listed by `groups` covers [`FREE_STEPS_PER_CANDIDATE`] steps, every step beyond is paid from
/// `work`, and a pick whose steps are gone works with what it has seen and leaves the rest for a
/// later tick. All the work of a tick is thus at most `FREE_STEPS_PER_CANDIDATE + 1` times the
/// budget of the tick.
fn pick(candidates: &[Candidate], seats: usize, work: &mut usize) -> Option<Vec<QueueEntryId>> {
    let players = oldest_candidates(candidates);
    let mut versions: HashMap<AccountId, Vec<usize>> = HashMap::new();
    for (index, player) in players.iter().enumerate() {
        versions.entry(player.main_account).or_default().push(index);
    }
    for siblings in versions.values_mut() {
        siblings.sort_unstable_by_key(|&index| players[index].order);
    }
    let mut active = BTreeSet::new();
    let mut best = None;
    let allowance = players.len() * FREE_STEPS_PER_CANDIDATE;
    let mut scan = Scan {
        steps: 0,
        limit: allowance.saturating_add(*work),
    };
    for (index, player) in players.iter().enumerate() {
        let key = Rated {
            rating: player.rating,
            index,
        };
        active.insert(key);
        if let Some(choice) = best_near(&active, key, &players, &versions, seats, index, &mut scan)
            && best.as_ref().is_none_or(|previous| choice > *previous)
        {
            best = Some(choice);
        }
    }
    let mut by_age: Vec<_> = (0..players.len()).collect();
    by_age.sort_unstable_by_key(|&index| players[index].order);
    let refined = best.map(|best| refine_choice(best, &players, &by_age, &mut scan));
    *work -= scan.steps.saturating_sub(allowance).min(*work);
    let mut members = refined?.members.0;
    members.sort_unstable_by_key(|&index| players[index].order);
    Some(
        members
            .into_iter()
            .map(|index| players[index].entry)
            .collect(),
    )
}

fn oldest_candidates(candidates: &[Candidate]) -> Vec<Candidate> {
    let mut oldest: HashMap<AccountId, Candidate> = HashMap::new();
    for &candidate in candidates {
        assert!(candidate.rating.is_finite());
        oldest
            .entry(candidate.account)
            .and_modify(|previous| {
                if candidate.order < previous.order {
                    *previous = candidate;
                }
            })
            .or_insert(candidate);
    }
    let mut players: Vec<_> = oldest.into_values().collect();
    players.sort_unstable_by(|left, right| {
        right
            .window
            .cmp(&left.window)
            .then_with(|| left.order.cmp(&right.order))
            .then_with(|| left.entry.0.cmp(&right.entry.0))
    });
    players
}

/// A total order without an external float-ordering dependency. Only finite ratings enter it.
#[derive(Clone, Copy, Debug)]
struct Rated {
    rating: f64,
    index: usize,
}

impl PartialEq for Rated {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Rated {}
impl PartialOrd for Rated {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Rated {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rating
            .total_cmp(&other.rating)
            .then_with(|| self.index.cmp(&other.index))
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Choice {
    oldest: Reverse<usize>,
    sources: usize,
    newest: Reverse<usize>,
    members: Reverse<Vec<usize>>,
}

#[cfg(test)]
thread_local! {
    /// Every step the thread's picks have taken.
    static EXECUTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Every candidate the thread's picks have listed.
    static LISTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The steps one pick may take through the queue's neighbourhoods.
struct Scan {
    steps: usize,
    limit: usize,
}

impl Scan {
    /// Takes one step; false, with nothing taken, when the pick has none left.
    fn step(&mut self) -> bool {
        if self.steps >= self.limit {
            return false;
        }
        self.steps += 1;
        #[cfg(test)]
        EXECUTED.with(|executed| executed.set(executed.get() + 1));
        true
    }
}

fn best_near(
    active: &BTreeSet<Rated>,
    key: Rated,
    players: &[Candidate],
    versions: &HashMap<AccountId, Vec<usize>>,
    seats: usize,
    required: usize,
    scan: &mut Scan,
) -> Option<Choice> {
    // Each owner has at most 128 distinct versions after identity deduplication, so this many
    // entries cover every possible nearest distinct owner.
    let reach = radovid_proto::MAX_VERSIONS_PER_ACCOUNT * seats;
    let mut left: Vec<Rated> = Vec::with_capacity(seats);
    let mut owners: Vec<AccountId> = Vec::with_capacity(seats);
    owners.push(players[required].main_account);
    for rated in active.range(..key).rev().take(reach) {
        if left.len() == seats - 1 || !scan.step() {
            break;
        }
        let owner = players[rated.index].main_account;
        if !owners.contains(&owner) {
            owners.push(owner);
            left.push(*rated);
        }
    }
    (0..=left.len())
        .filter_map(|count| {
            let mut members: Vec<_> = left[..count].iter().map(|rated| rated.index).collect();
            members.push(required);
            let mut owners: Vec<AccountId> = members
                .iter()
                .map(|&index| players[index].main_account)
                .collect();
            for rated in active.range(key..).skip(1).take(reach) {
                if members.len() == seats || !scan.step() {
                    break;
                }
                let owner = players[rated.index].main_account;
                if !owners.contains(&owner) {
                    owners.push(owner);
                    members.push(rated.index);
                }
            }
            (members.len() == seats)
                .then(|| choice_for(members, players))
                .flatten()
                .map(|choice| prefer_older_versions(choice, players, versions, scan))
        })
        .max()
}

/// Nearest versions establish feasibility; an older eligible sibling keeps its place
/// before groups are compared, even when improving it needs a different group.
fn prefer_older_versions(
    mut choice: Choice,
    players: &[Candidate],
    versions: &HashMap<AccountId, Vec<usize>>,
    scan: &mut Scan,
) -> Choice {
    let owners: Vec<_> = choice
        .members
        .0
        .iter()
        .map(|&index| players[index].main_account)
        .collect();
    for owner in owners {
        for &index in versions[&owner]
            .iter()
            .take(radovid_proto::MAX_VERSIONS_PER_ACCOUNT)
        {
            if !scan.step() || players[index].order >= choice.oldest.0 {
                break;
            }
            let mut members = choice.members.0.clone();
            let position = members
                .iter()
                .position(|&member| players[member].main_account == owner)
                .expect("selected owner remains present");
            members[position] = index;
            if let Some(improved) = choice_for(members, players)
                && improved > choice
            {
                choice = improved;
            }
        }
    }
    choice
}

fn choice_for(mut members: Vec<usize>, players: &[Candidate]) -> Option<Choice> {
    // Two seats of one owner never play together; at most ten seats, so no allocation.
    for (position, &index) in members.iter().enumerate() {
        if members[..position]
            .iter()
            .any(|&earlier| players[earlier].main_account == players[index].main_account)
        {
            return None;
        }
    }
    let low = members
        .iter()
        .map(|&index| players[index].rating)
        .reduce(f64::min)?;
    let high = members
        .iter()
        .map(|&index| players[index].rating)
        .reduce(f64::max)?;
    let accepted = members.iter().map(|&index| players[index].window).min()?;
    if high - low > f64::from(accepted) {
        return None;
    }
    members.sort_unstable();
    let sources = members
        .iter()
        .enumerate()
        .filter(|(position, index)| {
            members[..*position]
                .iter()
                .all(|&previous| players[previous].source != players[**index].source)
        })
        .count();
    Some(Choice {
        sources,
        oldest: Reverse(members.iter().map(|&index| players[index].order).min()?),
        newest: Reverse(members.iter().map(|&index| players[index].order).max()?),
        members: Reverse(members),
    })
}

fn refine_choice(
    mut choice: Choice,
    players: &[Candidate],
    by_age: &[usize],
    scan: &mut Scan,
) -> Choice {
    for &index in by_age {
        if choice.members.0.contains(&index) {
            continue;
        }
        let original = choice.members.0.clone();
        for position in 0..original.len() {
            if !scan.step() {
                return choice;
            }
            let mut replacement = original.clone();
            replacement[position] = index;
            if let Some(improved) = choice_for(replacement, players)
                && improved > choice
            {
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
        let players: Vec<_> = (1..=100)
            .map(|entry| candidate(entry, 1500.0, 100))
            .collect();
        let mut budget = 198;
        assert_eq!(super::groups(players, 2, 50, &mut budget).len(), 2);
        assert_eq!(budget, 0);
    }

    #[test]
    fn selection_finds_the_oldest_eligible_anchor_across_small_mixed_windows() {
        for seed in 0u64..512 {
            let mut state = seed + 1;
            let players: Vec<_> = (1..=6)
                .map(|entry| {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    candidate(
                        entry,
                        ((state >> 32) % 4) as f64 * 50.0,
                        ((state >> 40) % 4) as u32 * 50,
                    )
                })
                .collect();
            for seats in 2..=4 {
                let expected = (1u32..64)
                    .filter(|bits| bits.count_ones() as usize == seats)
                    .filter_map(|bits| {
                        let members: Vec<_> =
                            (0..6).filter(|index| bits & (1 << index) != 0).collect();
                        choice_for(members, &players).map(|choice| choice.oldest.0)
                    })
                    .min();
                let selected = groups(players.clone(), seats, 1);
                let actual = selected
                    .first()
                    .map(|ids| ids.iter().map(|id| id.0 as usize).min().expect("nonempty"));
                assert_eq!(
                    actual, expected,
                    "seed={seed} seats={seats} players={players:?}"
                );
            }
        }
    }

    fn tick_budget() -> usize {
        TICK_BUDGET
    }

    /// The worst queue the budget has to bear: every owner with every version queued at one rating,
    /// beside a mixed crowd of single entries, for the widest ruleset.
    fn worst_queue() -> Vec<Candidate> {
        let versions = radovid_proto::MAX_VERSIONS_PER_ACCOUNT as u32;
        let owners = 8;
        let mut players = Vec::new();
        for owner in 0..owners {
            for version in 0..versions {
                let entry = owner * versions + version + 1;
                let mut player = candidate(entry, 1500.0, 100);
                player.main_account = AccountId(10_000 + u64::from(owner));
                players.push(player);
            }
        }
        let first = owners * versions + 1;
        for index in 0..512 {
            players.push(candidate(
                first + index,
                1000.0 + f64::from(index % 37) * 40.0,
                100 + (index % 5) * 100,
            ));
        }
        players
    }

    /// One tick of pairing as the hub runs it: group after group while the budget lasts. Returns
    /// the steps taken, the candidates listed and the groups paired.
    fn one_tick(mut players: Vec<Candidate>, seats: usize) -> (usize, usize, usize) {
        EXECUTED.with(|executed| executed.set(0));
        LISTED.with(|listed| listed.set(0));
        let mut work = tick_budget();
        let mut paired = 0;
        while let Some(group) = super::groups(players.clone(), seats, 1, &mut work).pop() {
            paired += 1;
            players.retain(|player| !group.contains(&player.entry));
        }
        (
            EXECUTED.with(std::cell::Cell::get),
            LISTED.with(std::cell::Cell::get),
            paired,
        )
    }

    #[test]
    fn the_worst_queue_of_ten_seats_takes_no_more_steps_in_a_tick_than_the_tick_pays_for() {
        let (steps, listed, _) = one_tick(worst_queue(), 10);
        let granted = FREE_STEPS_PER_CANDIDATE * listed + tick_budget();
        assert!(
            steps <= granted,
            "{steps} steps for {listed} listed candidates and a budget of {}",
            tick_budget()
        );
        assert!(
            steps > tick_budget() / 2,
            "the worst queue does use the tick: {steps}"
        );
    }

    #[test]
    fn a_normal_queue_still_pairs_within_one_tick() {
        let players: Vec<_> = (1..=400)
            .map(|entry| candidate(entry, 1000.0 + f64::from(entry % 50) * 5.0, 200))
            .collect();
        let (_, _, paired) = one_tick(players, 2);
        assert!(paired >= 20, "only {paired} pairs in one tick");
    }

    #[test]
    fn a_tight_budget_still_pairs_distinct_owners_that_sit_side_by_side() {
        let players = vec![candidate(1, 1500.0, 100), candidate(2, 1500.0, 100)];
        let mut budget = 2;
        assert_eq!(
            super::groups(players, 2, 1, &mut budget),
            vec![vec![QueueEntryId(1), QueueEntryId(2)]]
        );
    }

    fn candidate(entry: u32, rating: f64, window: u32) -> Candidate {
        Candidate {
            entry: QueueEntryId(entry),
            account: AccountId(u64::from(entry)),
            main_account: AccountId(u64::from(entry)),
            source: "127.0.0.1".parse().expect("source"),
            rating,
            window,
            order: entry as usize,
        }
    }

    #[test]
    fn ranked_players_outside_the_initial_window_are_not_matched() {
        assert!(
            groups(
                vec![candidate(1, 1500.0, 100), candidate(2, 1700.0, 100)],
                2,
                1
            )
            .is_empty()
        );
    }

    #[test]
    fn widening_with_wait_allows_a_match_and_stops_at_the_configured_maximum() {
        let window = RatingWindow {
            initial: 100,
            widen_per_minute: 100,
            maximum: 300,
        };
        assert_eq!(window.after_ms(0), 100);
        assert_eq!(window.after_ms(30_000), 150);
        assert_eq!(window.after_ms(60_000), 200);
        assert_eq!(window.after_ms(u32::MAX), 300);
        assert_eq!(
            groups(
                vec![
                    candidate(1, 1500.0, window.after_ms(60_000)),
                    candidate(2, 1700.0, window.after_ms(60_000))
                ],
                2,
                1
            ),
            vec![vec![QueueEntryId(1), QueueEntryId(2)]]
        );
    }

    #[test]
    fn an_old_wide_window_cannot_force_a_new_player_outside_its_own_window() {
        assert!(
            groups(
                vec![candidate(1, 1500.0, 1000), candidate(2, 1800.0, 100)],
                2,
                1
            )
            .is_empty()
        );
    }

    #[test]
    fn the_looser_window_supports_team_games_and_three_way_games() {
        let players: Vec<_> = [1500.0, 1700.0, 1900.0, 2100.0]
            .into_iter()
            .enumerate()
            .map(|(index, rating)| candidate(index as u32 + 1, rating, 600))
            .collect();
        assert_eq!(groups(players.clone(), 4, 1).len(), 1);
        assert_eq!(groups(players, 3, 1).len(), 1);
    }

    #[test]
    fn a_narrow_newcomer_does_not_block_compatible_older_players() {
        let players = vec![
            candidate(1, 0.0, 100),
            candidate(2, 0.0, 0),
            candidate(3, 50.0, 100),
            candidate(4, 100.0, 100),
        ];
        assert_eq!(
            groups(players, 3, 1),
            vec![vec![QueueEntryId(1), QueueEntryId(3), QueueEntryId(4)]]
        );
    }

    #[test]
    fn one_account_cannot_fill_multiple_seats_and_its_oldest_place_is_used() {
        let mut players = vec![
            candidate(1, 1500.0, 100),
            candidate(2, 1500.0, 100),
            candidate(3, 1500.0, 100),
        ];
        players[1].account = players[0].account;
        players[1].main_account = players[0].main_account;
        assert_eq!(
            groups(players, 2, 1),
            vec![vec![QueueEntryId(1), QueueEntryId(3)]]
        );
    }

    #[test]
    fn a_compatible_different_source_is_preferred_to_self_matching() {
        let mut players = vec![
            candidate(1, 1500.0, 100),
            candidate(2, 1500.0, 100),
            candidate(3, 1500.0, 100),
        ];
        players[2].source = "127.0.0.2".parse().expect("other source");
        assert_eq!(
            groups(players, 2, 1),
            vec![vec![QueueEntryId(1), QueueEntryId(3)]]
        );
    }

    #[test]
    fn selected_groups_do_not_overlap_and_respect_available_capacity() {
        let players: Vec<_> = (1..=8).map(|entry| candidate(entry, 1500.0, 100)).collect();
        assert_eq!(
            groups(players, 2, 2),
            vec![
                vec![QueueEntryId(1), QueueEntryId(2)],
                vec![QueueEntryId(3), QueueEntryId(4)]
            ]
        );
    }

    #[test]
    fn a_narrow_entry_cannot_hide_a_second_compatible_group_after_removal() {
        let mut players = vec![
            candidate(1, 0.0, 100),
            candidate(2, 10.0, 100),
            candidate(3, 20.0, 100),
            candidate(4, 30.0, 100),
            candidate(5, 20.0, 0),
        ];
        for player in &mut players[2..] {
            player.source = "127.0.0.2".parse().expect("other source");
        }
        assert_eq!(
            groups(players, 2, 2),
            vec![
                vec![QueueEntryId(1), QueueEntryId(3)],
                vec![QueueEntryId(2), QueueEntryId(4)]
            ]
        );
    }

    #[test]
    fn independent_searches_of_one_account_can_fill_separate_matches() {
        let mut players: Vec<_> = (1..=4).map(|entry| candidate(entry, 1500.0, 100)).collect();
        players[1].account = players[0].account;
        players[1].main_account = players[0].main_account;
        assert_eq!(
            groups(players, 2, 2),
            vec![
                vec![QueueEntryId(1), QueueEntryId(3)],
                vec![QueueEntryId(2), QueueEntryId(4)]
            ]
        );
    }

    #[test]
    fn old_compatible_players_are_not_starved_by_newer_distinct_sources() {
        let mut players = vec![
            candidate(1, 0.0, 100),
            candidate(2, 0.0, 100),
            candidate(3, 1000.0, 100),
            candidate(4, 1000.0, 100),
        ];
        players[2].source = "127.0.0.2".parse().expect("other source");
        players[3].source = "127.0.0.3".parse().expect("third source");
        assert_eq!(
            groups(players, 2, 1),
            vec![vec![QueueEntryId(1), QueueEntryId(2)]]
        );
    }
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn versions_never_hide_a_feasible_group_or_fill_two_seats_of_it() {
        for seed in 0u64..512 {
            let mut state = seed + 1;
            let players: Vec<_> = (0..6)
                .map(|index| {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    Candidate {
                        entry: QueueEntryId(index + 1),
                        account: AccountId(u64::from(index + 1)),
                        main_account: AccountId((state >> 48) % 4),
                        source: "127.0.0.1".parse().expect("source"),
                        rating: ((state >> 32) % 4) as f64 * 50.0,
                        window: ((state >> 40) % 4) as u32 * 50,
                        order: index as usize,
                    }
                })
                .collect();
            for seats in 2..=4 {
                let expected = (1u32..64)
                    .filter(|bits| bits.count_ones() as usize == seats)
                    .filter_map(|bits| {
                        choice_for(
                            (0..6).filter(|index| bits & (1 << index) != 0).collect(),
                            &players,
                        )
                        .map(|choice| choice.oldest.0)
                    })
                    .min();
                let actual = groups(players.clone(), seats, 1, &mut 65_536)
                    .first()
                    .map(|group| {
                        group
                            .iter()
                            .map(|entry| entry.0 as usize - 1)
                            .min()
                            .expect("group")
                    });
                assert_eq!(
                    actual, expected,
                    "seed={seed} seats={seats} players={players:?}"
                );
            }
        }
    }
}
