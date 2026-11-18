//! Asking a service, before anyone signs in to it, whether it is a radovid this client can use:
//! one connection and the protocol's own first exchange (the `Hello`, answered by a `Welcome` or
//! a refusal), nothing else. [`ServiceCheck`] runs it without blocking its caller on either
//! backend; the outcome is a [`ServiceFound`] or the one [`ServiceFault`] that says what to do.

use radovid_proto::{Error, PROTO_VERSION, Reply};

use crate::{Control, Incoming};

/// Bytes of the service's answer read at most: a `Welcome` holds a catalog of rulesets, a few
/// kilobytes each at most. Past it the connection is cut off.
pub const MAX_ANSWER_BYTES: usize = 256 * 1024;

/// Characters of the service's own name kept: it is shown to a person.
pub const MAX_NAME_CHARS: usize = 48;

/// Checks running at once at most, in total: one that cannot be ended (a resolver that does not
/// answer) holds a thread until the system gives up.
pub const MAX_CHECKS_AT_ONCE: usize = 8;

/// What a service that passed said of itself, before any sign-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceFound {
    /// What the service calls itself; may be empty.
    pub name: String,
}

/// Why a service did not pass: what the caller can tell the person to do about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceFault {
    /// The address is no `wss://host[:port][/path]`.
    BadAddress,
    /// No such host.
    HostNotFound,
    /// The host answered that nothing listens at the port.
    Refused,
    /// Nothing answered within the time given.
    TimedOut,
    /// The service's certificate or its TLS is not acceptable.
    Insecure,
    /// Something answers, but it is no radovid: a web server, a game server, anything else.
    NotService,
    /// The connection ended without a word.
    Hangup,
    /// The service speaks a newer protocol than this client.
    Newer { server: u16 },
    /// The service speaks an older protocol than this client.
    Older { server: u16 },
    /// The service refused the greeting as unreadable: another version of the protocol.
    Incompatible,
    /// The service is busy, restarting or limiting this client.
    Busy,
    /// Earlier checks are still running: see [`MAX_CHECKS_AT_ONCE`].
    TooManyChecks,
    /// A browser says only that it could not connect: not found, refused and not secure look the
    /// same to a page.
    CannotConnect,
}

/// What the first message a service sent, as it arrived, says of it: `seq` is the number of the
/// `Hello` it answers. Bytes that are no message of the protocol are no service.
pub fn judge_message(seq: u32, message: &[u8]) -> Result<ServiceFound, ServiceFault> {
    let answer = Control::new()
        .receive(message)
        .map_err(|_| ServiceFault::NotService)?;
    judge(seq, answer)
}

/// What the answer to the `Hello` numbered `seq` says of the service.
pub(crate) fn judge(seq: u32, answer: Incoming) -> Result<ServiceFound, ServiceFault> {
    let Incoming::Reply {
        seq: answered,
        result,
    } = answer
    else {
        return Err(ServiceFault::NotService);
    };
    if answered != seq {
        return Err(ServiceFault::NotService);
    }
    match result {
        Ok(Reply::Welcome { server, .. }) => Ok(ServiceFound {
            name: tidy(&server),
        }),
        Ok(_) => Err(ServiceFault::NotService),
        Err(Error::ProtoMismatch { server }) => Err(match server.cmp(&PROTO_VERSION) {
            std::cmp::Ordering::Greater => ServiceFault::Newer { server },
            std::cmp::Ordering::Less => ServiceFault::Older { server },
            std::cmp::Ordering::Equal => ServiceFault::Incompatible,
        }),
        Err(Error::Malformed) => Err(ServiceFault::Incompatible),
        Err(
            Error::RateLimited { .. }
            | Error::Throttled { .. }
            | Error::ServerStopping
            | Error::Internal,
        ) => Err(ServiceFault::Busy),
        Err(_) => Err(ServiceFault::NotService),
    }
}

/// A name a service gave, fit to show: no control characters, bounded.
fn tidy(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(error: Error) -> Incoming {
        Incoming::Reply {
            seq: 1,
            result: Err(error),
        }
    }

    #[test]
    fn a_welcome_to_the_hello_passes_with_the_services_tidied_name() {
        let welcome = |server: &str| Incoming::Reply {
            seq: 1,
            result: Ok(Reply::Welcome {
                proto: PROTO_VERSION,
                server: server.to_owned(),
                rulesets: Vec::new(),
            }),
        };
        assert_eq!(
            judge(1, welcome("  Match\u{7}maker\n")),
            Ok(ServiceFound {
                name: "Matchmaker".to_owned()
            })
        );
        let long = judge(1, welcome(&"é".repeat(500))).expect("passes");
        assert_eq!(long.name.chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn a_refused_version_says_which_side_is_older() {
        assert_eq!(
            judge(
                1,
                refused(Error::ProtoMismatch {
                    server: PROTO_VERSION + 1
                })
            ),
            Err(ServiceFault::Newer {
                server: PROTO_VERSION + 1
            })
        );
        assert_eq!(
            judge(
                1,
                refused(Error::ProtoMismatch {
                    server: PROTO_VERSION - 1
                })
            ),
            Err(ServiceFault::Older {
                server: PROTO_VERSION - 1
            })
        );
        assert_eq!(
            judge(1, refused(Error::Malformed)),
            Err(ServiceFault::Incompatible)
        );
    }

    #[test]
    fn anything_but_the_answer_to_the_hello_is_no_service() {
        assert_eq!(
            judge(
                1,
                Incoming::Reply {
                    seq: 2,
                    result: Ok(Reply::Done)
                }
            ),
            Err(ServiceFault::NotService)
        );
        assert_eq!(
            judge(
                1,
                Incoming::Reply {
                    seq: 1,
                    result: Ok(Reply::Done)
                }
            ),
            Err(ServiceFault::NotService)
        );
        assert_eq!(
            judge(
                1,
                Incoming::Event(radovid_proto::Event::Bye {
                    error: Error::ServerStopping
                })
            ),
            Err(ServiceFault::NotService)
        );
        assert_eq!(
            judge(1, refused(Error::BadCredentials)),
            Err(ServiceFault::NotService)
        );
        assert_eq!(
            judge(1, refused(Error::ServerStopping)),
            Err(ServiceFault::Busy)
        );
    }
}
