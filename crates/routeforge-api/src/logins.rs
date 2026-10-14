//! How often a login may be tried (D68).
//!
//! Without a limit, whoever knows an address can try passwords for it as fast as the
//! API hashes them. So the logins that are tried for an address are counted, and after
//! a few of them within a minute the next is refused until the minute is over. A login
//! that succeeds clears the count.
//!
//! It is counted by the address that was sent, whether it has an account or not, so a
//! refusal says nothing about which addresses are registered (D40).
//!
//! What it costs: somebody who knows an address can keep its owner out, for as long as
//! they keep trying. A limit by where the tries come from would not have that fault,
//! and would have another: behind Docker's published port, every caller comes from the
//! same address.
//!
//! The counts are in memory, in this process. They are gone when the API is started
//! again, and two APIs would count apart.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long the tries for an address are counted, from the first of them.
pub(crate) const WINDOW: Duration = Duration::from_secs(60);

/// How many addresses are counted at once. Whoever sends a login for an address of
/// their own making gets an entry, and nothing else would ever bound them.
const MOST_ADDRESSES: usize = 10_000;

/// The longest address that is counted under its own name. No account has a longer
/// one, and everything longer is one and the same to the count.
const LONGEST_ADDRESS: usize = 254;

/// The logins tried lately, by the address they were for.
pub(crate) struct Logins {
    /// How many may be tried for an address within one window.
    most: u32,
    window: Duration,
    tried: Mutex<HashMap<String, Tries>>,
}

/// The tries for one address.
#[derive(Clone, Copy, Debug)]
struct Tries {
    /// When the first of them was made.
    since: Instant,
    count: u32,
}

impl Logins {
    /// At most `most` logins for one address within `window`.
    pub(crate) fn new(most: u32, window: Duration) -> Self {
        Self {
            most,
            window,
            tried: Mutex::new(HashMap::new()),
        }
    }

    /// Counts one more try for the address, if one more may be made at `now`.
    ///
    /// # Errors
    ///
    /// With how long it is until the next may be made, if the address has had its
    /// share. The refused try is not counted, so asking again does not put it off. A
    /// new address waits as well when every address that is counted has had its share:
    /// see [`make_room`](Self::make_room).
    pub(crate) fn begin(&self, address: &str, now: Instant) -> Result<(), Duration> {
        let key = key(address);
        let mut tried = self.lock();
        if let Some(tries) = tried.get_mut(&key) {
            let over_at = tries.since + self.window;
            if now < over_at {
                if tries.count >= self.most {
                    return Err(over_at - now);
                }
                tries.count += 1;
                return Ok(());
            }
        }
        if tried.len() >= MOST_ADDRESSES && !tried.contains_key(&key) {
            self.make_room(&mut tried, now)?;
        }
        tried.insert(
            key,
            Tries {
                since: now,
                count: 1,
            },
        );
        Ok(())
    }

    /// Makes room for one more address. The counts whose window is over go first, and
    /// if none is, the newest of those with the fewest tries.
    ///
    /// A count that has reached its limit stays, whatever comes. Forgotten, it would
    /// give its address its tries again, and logins for addresses of somebody's own
    /// making would be a way to have that done (D68).
    ///
    /// # Errors
    ///
    /// When every address that is counted has had its tries: how long it is until the
    /// first of their windows is over. A login cannot get there, since each try takes a
    /// hash and only so many are hashed at a time, but a new address waits rather than
    /// make room at the cost of an old one.
    fn make_room(&self, tried: &mut HashMap<String, Tries>, now: Instant) -> Result<(), Duration> {
        tried.retain(|_, tries| now < tries.since + self.window);
        if tried.len() < MOST_ADDRESSES {
            return Ok(());
        }
        let least = tried
            .iter()
            .filter(|(_, tries)| tries.count < self.most)
            .min_by_key(|(_, tries)| (tries.count, Reverse(tries.since)))
            .map(|(address, _)| address.clone());
        if let Some(least) = least {
            tried.remove(&least);
            return Ok(());
        }
        let first_over = tried.values().map(|tries| tries.since + self.window).min();
        Err(first_over.map_or(self.window, |over_at| over_at - now))
    }

    /// Takes back a try that was counted and came to nothing: the API could not check
    /// the password, which is not the fault of whoever sent it.
    pub(crate) fn take_back(&self, address: &str) {
        let key = key(address);
        let mut tried = self.lock();
        if let Some(tries) = tried.get_mut(&key) {
            tries.count = tries.count.saturating_sub(1);
            if tries.count == 0 {
                tried.remove(&key);
            }
        }
    }

    /// Forgets the tries for an address: its owner has logged in.
    pub(crate) fn succeeded(&self, address: &str) {
        self.lock().remove(&key(address));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Tries>> {
        // Nothing is ever done under the lock that could leave the map half changed.
        self.tried.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What an address is counted under: as an account has it, in lower case, however it
/// was typed.
fn key(address: &str) -> String {
    address
        .trim()
        .chars()
        .take(LONGEST_ADDRESS + 1)
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    fn seconds(count: u64) -> Duration {
        Duration::from_secs(count)
    }

    #[test]
    fn an_address_has_so_many_tries_within_a_window() {
        let logins = Logins::new(3, MINUTE);
        let start = Instant::now();
        for second in 0..3 {
            assert_eq!(
                logins.begin("ada@example.com", start + seconds(second)),
                Ok(())
            );
        }
        // The fourth is refused, and told how long the window still runs: from the
        // first try, not from the last.
        assert_eq!(
            logins.begin("ada@example.com", start + seconds(10)),
            Err(seconds(50))
        );
        // Asking again changes nothing, and does not put the end off.
        assert_eq!(
            logins.begin("ada@example.com", start + seconds(59)),
            Err(seconds(1))
        );
        // Another address has tries of its own.
        assert_eq!(logins.begin("bob@example.com", start + seconds(10)), Ok(()));

        // When the window is over, the count begins again.
        assert_eq!(logins.begin("ada@example.com", start + MINUTE), Ok(()));
        assert_eq!(logins.begin("ada@example.com", start + MINUTE), Ok(()));
        assert_eq!(logins.begin("ada@example.com", start + MINUTE), Ok(()));
        assert_eq!(
            logins.begin("ada@example.com", start + MINUTE + seconds(1)),
            Err(seconds(59))
        );
    }

    #[test]
    fn an_address_is_one_however_it_is_typed() {
        let logins = Logins::new(2, MINUTE);
        let now = Instant::now();
        assert_eq!(logins.begin("Ada@Example.com", now), Ok(()));
        assert_eq!(logins.begin("  ada@example.com ", now), Ok(()));
        assert!(logins.begin("ADA@EXAMPLE.COM", now).is_err());

        // Longer than any account's, they are all one: nobody gets a count for each
        // of a million letters.
        let long = "a".repeat(300);
        let longer = "a".repeat(5_000);
        assert_eq!(logins.begin(&long, now), Ok(()));
        assert_eq!(logins.begin(&longer, now), Ok(()));
        assert!(logins.begin(&long, now).is_err());
        assert!(key(&longer).len() <= LONGEST_ADDRESS + 1);
    }

    #[test]
    fn a_login_that_succeeds_clears_the_count() {
        let logins = Logins::new(2, MINUTE);
        let now = Instant::now();
        assert_eq!(logins.begin("ada@example.com", now), Ok(()));
        assert_eq!(logins.begin("ada@example.com", now), Ok(()));
        logins.succeeded("ADA@example.com");
        assert_eq!(logins.begin("ada@example.com", now), Ok(()));
        assert_eq!(logins.begin("ada@example.com", now), Ok(()));
        assert!(logins.begin("ada@example.com", now).is_err());
    }

    #[test]
    fn a_try_that_came_to_nothing_is_taken_back() {
        let logins = Logins::new(2, MINUTE);
        let now = Instant::now();
        assert_eq!(logins.begin("ada@example.com", now), Ok(()));
        assert_eq!(logins.begin("ada@example.com", now), Ok(()));
        logins.take_back("ada@example.com");
        assert_eq!(logins.begin("ada@example.com", now), Ok(()));
        assert!(logins.begin("ada@example.com", now).is_err());

        // Taking back what was never counted is nothing.
        logins.take_back("nobody@example.com");
        logins.take_back("ada@example.com");
        logins.take_back("ada@example.com");
        logins.take_back("ada@example.com");
        assert_eq!(logins.begin("ada@example.com", now), Ok(()));
        assert_eq!(logins.begin("ada@example.com", now), Ok(()));
        assert!(logins.begin("ada@example.com", now).is_err());
    }

    /// Addresses nobody has cost a try each and an entry each. The entries are bounded:
    /// the ones whose window is over go first, and then one of those with the fewest
    /// tries.
    #[test]
    fn the_addresses_that_are_counted_are_bounded() {
        let logins = Logins::new(2, MINUTE);
        let start = Instant::now();
        // One with both its tries, begun first, and then as many with one as there is
        // room for.
        assert_eq!(logins.begin("ada@example.com", start), Ok(()));
        assert_eq!(logins.begin("ada@example.com", start), Ok(()));
        for number in 1..MOST_ADDRESSES {
            let address = format!("{number}@example.com");
            assert_eq!(logins.begin(&address, start + seconds(1)), Ok(()));
        }
        assert_eq!(logins.lock().len(), MOST_ADDRESSES);

        // One more, with every window still running: a count of one try goes, and not
        // the oldest, which has had its tries.
        assert_eq!(logins.begin("new@example.com", start + seconds(2)), Ok(()));
        assert_eq!(logins.lock().len(), MOST_ADDRESSES);
        assert!(logins.lock().contains_key("new@example.com"));
        assert!(logins.begin("ada@example.com", start + seconds(2)).is_err());

        // One more when every window but the last one is over: those all go.
        let later = start + MINUTE + seconds(1);
        assert_eq!(logins.begin("late@example.com", later), Ok(()));
        let left = logins.lock().len();
        assert_eq!(left, 2, "the one of a moment ago, and the new one");
    }

    /// Whoever has had their tries at an address does not get them again by sending
    /// logins for addresses of their own making until its count is pushed out (D68).
    #[test]
    fn a_flood_of_new_addresses_does_not_give_an_address_its_tries_again() {
        let logins = Logins::new(5, MINUTE);
        let start = Instant::now();
        for _ in 0..5 {
            assert_eq!(logins.begin("ada@example.com", start), Ok(()));
        }
        assert!(logins.begin("ada@example.com", start).is_err());

        // More made-up addresses than are counted, all within the window. Each is
        // counted, and each makes room for the next.
        for number in 0..MOST_ADDRESSES + 100 {
            let address = format!("{number}@example.com");
            assert_eq!(logins.begin(&address, start + seconds(1)), Ok(()));
        }
        assert_eq!(logins.lock().len(), MOST_ADDRESSES);
        assert_eq!(
            logins.begin("ada@example.com", start + seconds(2)),
            Err(seconds(58))
        );
    }

    /// When every address that is counted has had its tries, none of them is forgotten
    /// to make room: a new one waits until the first window is over.
    #[test]
    fn when_every_count_is_at_its_limit_a_new_address_waits() {
        let logins = Logins::new(1, MINUTE);
        let start = Instant::now();
        for number in 0..MOST_ADDRESSES {
            let address = format!("{number}@example.com");
            let at = start + seconds(number as u64 % 10);
            assert_eq!(logins.begin(&address, at), Ok(()));
        }
        assert_eq!(
            logins.begin("new@example.com", start + seconds(20)),
            Err(seconds(40))
        );
        assert_eq!(logins.lock().len(), MOST_ADDRESSES);

        // The windows that began first are over, and their counts make room.
        assert_eq!(logins.begin("new@example.com", start + MINUTE), Ok(()));
        assert!(logins.begin("0@example.com", start + MINUTE).is_ok());
    }
}
