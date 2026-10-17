//! A bot in a few lines: log in (registering the first time), queue for
//! `solo_mid`, take the seat and play with a trivial policy, match after match.
//!
//! ```text
//! solo_mid_bot wss://localhost:4460 my_bot 'a long password' [dev-cert.pem]
//! ```

use bota_proto::{HeroId, Order, Target, Vec2};
use radovid_client::proto::{AccountKind, ClientInfo, Event, RulesetId};
use radovid_client::{Client, IssuedOrder, TlsConfig, play, take_seat};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (url, login, password) = match (args.next(), args.next(), args.next()) {
        (Some(url), Some(login), Some(password)) => (url, login, password),
        _ => {
            return Err(
                "usage: solo_mid_bot <wss://host:port> <login> <password> [cert.pem]".into(),
            );
        }
    };
    let tls = match args.next() {
        Some(pem) => TlsConfig::trust_pem(&std::fs::read_to_string(pem)?)?,
        None => TlsConfig::webpki(),
    };
    let info = ClientInfo {
        bot_version: Some("example-1".to_owned()),
    };

    let mut client = Client::connect(&url, &tls)?;
    if client.login(&login, &password, info.clone()).is_err() {
        client.register(&login, &password, AccountKind::Bot, info)?;
    }
    let solo_mid = RulesetId("solo_mid".to_owned());
    loop {
        let handoff = client.queue_and_wait(&solo_mid)?;
        let (mut link, seated) = take_seat(&handoff, HeroId::SHADOW_FIEND, &tls)?;
        // The policy: walk to the middle of the map and stay there.
        let middle = Order::Move {
            target: Target::Pos(Vec2::from_ints(9216, 9216)),
        };
        let outcome = play(&mut link, seated, &mut (), |_| {
            Some(IssuedOrder::hero(middle))
        })?;
        println!(
            "match {} over: {:?} after {} ticks",
            handoff.match_id.0, outcome.winner, outcome.ticks
        );
        // The rating change arrives on the control connection.
        while let Some(event) = client.next_event(std::time::Duration::from_secs(10))? {
            if let Event::MatchResult { result } = event {
                println!(
                    "{:?}",
                    result
                        .seats
                        .iter()
                        .map(|s| (&s.login, s.after))
                        .collect::<Vec<_>>()
                );
                break;
            }
        }
    }
}
