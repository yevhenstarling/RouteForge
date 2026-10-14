//! A place in words: the search for an address, and the places it found until one
//! is picked (D72).

use super::*;

impl<A: Api, G: Geocoder> Chats<A, G> {
    /// Asks where an address is, and shows what was found, each place with a button.
    /// Nothing is begun here: the press of one of those buttons begins the order.
    pub(super) async fn look_up(&self, sender: &Sender, words: &str) -> Reply {
        let words = words.trim();
        if words.is_empty() {
            return Reply::new(NOT_UNDERSTOOD_BY_ADDRESS);
        }
        if words.chars().count() > ADDRESS_ASKED_LONGEST {
            return Reply::new(format!(
                "That is too long for an address: I read up to {ADDRESS_ASKED_LONGEST} \
                 letters of one. {OTHER_WAYS}"
            ));
        }
        let near = self.depot(sender).await;
        let found = match self.geocoder.find(words, near).await {
            // The service is somebody else's, and what it says goes into a message. So:
            // as many places as there are buttons for, and of each name as much as
            // the address of an order may be.
            Ok(found) => found
                .into_iter()
                .take(MOST_FOUND)
                .map(|place| Found {
                    name: place.name.chars().take(ADDRESS_LONGEST).collect(),
                    ..place
                })
                .collect::<Vec<_>>(),
            Err(Unfound::Busy) => {
                return Reply::new(
                    "Too many addresses are being looked up at this moment. Try again in a \
                     few seconds.",
                );
            }
            Err(unfound @ Unfound::Away(_)) => {
                warn!(%unfound, "could not look an address up");
                return Reply::new(format!(
                    "The address service does not answer right now. {OTHER_WAYS}"
                ));
            }
        };
        if found.is_empty() {
            return Reply::new(format!(
                "I found no place by that. Write the street and the number, with the town. \
                 Or: {}",
                lower_first(OTHER_WAYS)
            ));
        }

        let none = Press::NotFound(sender.message);
        let place = |at: u8| Press::Found(sender.message, at);
        let (text, rows) = match found.as_slice() {
            [only] => (
                format!("Is it this place?\n{}\n\n{CREDIT}", only.name),
                vec![key("Yes, an order there", place(0)), key("No", none)],
            ),
            many => {
                // A button carries where its place stands, counted from nought, and
                // shows the number the list gives it, counted from one.
                let listed: Vec<String> = (1..)
                    .zip(many)
                    .map(|(number, found)| format!("{number}. {}", found.name))
                    .collect();
                let numbers = (0..)
                    .zip(many)
                    .map(|(at, _)| Key {
                        label: (u32::from(at) + 1).to_string(),
                        press: place(at),
                    })
                    .collect();
                (
                    format!("Which of these is it?\n{}\n\n{CREDIT}", listed.join("\n")),
                    vec![numbers, key("None of these", none)],
                )
            }
        };
        self.remember(
            sender.chat,
            Search {
                id: sender.message,
                age: self.begun.fetch_add(1, Ordering::Relaxed),
                found,
            },
        );
        Reply::new(text).with(rows)
    }

    /// Where the depot is, so that an address is looked for round it: a street has
    /// its name in many towns. `None` when the API does not say, and then nothing is
    /// preferred.
    async fn depot(&self, sender: &Sender) -> Option<LatLon> {
        let session = self
            .api
            .telegram_login(sender.telegram_id, &sender.request_id)
            .await
            .ok()?;
        let depot = self.api.get_depot(&caller(&session, sender)).await.ok()??;
        Some(depot.location)
    }

    /// Keeps what a search found, in the place of what the chat searched for before.
    fn remember(&self, chat: i64, search: Search) {
        let mut searches = self.searches();
        searches.insert(chat, search);
        if searches.len() > MOST_DRAFTS {
            let oldest = searches
                .iter()
                .min_by_key(|(_, search)| search.age)
                .map(|(chat, _)| *chat);
            if let Some(oldest) = oldest {
                searches.remove(&oldest);
            }
        }
    }

    /// Begins an order at the place of a search that was picked. The search is over
    /// with that: its other places go with the message they were listed in.
    ///
    /// A press that names no place of the chat's search picks nothing, and ends
    /// nothing: the places are still there for a button that names one.
    pub(super) fn pick(&self, sender: &Sender, id: i32, place: u8) -> Reply {
        let picked = {
            let mut searches = self.searches();
            let picked = searches
                .get(&sender.chat)
                .filter(|search| search.id == id)
                .and_then(|search| search.found.get(usize::from(place)).cloned());
            if picked.is_some() {
                searches.remove(&sender.chat);
            }
            picked
        };
        match picked {
            Some(found) => {
                let place = Place {
                    lat: found.location.lat(),
                    lon: found.location.lon(),
                    address: Some(found.name),
                };
                self.begin(sender, place, true)
            }
            None => Reply::in_place(STALE_SEARCH),
        }
    }

    pub(super) fn pick_none(&self, sender: &Sender, id: i32) -> Reply {
        let mut searches = self.searches();
        match searches.get(&sender.chat) {
            Some(search) if search.id == id => {
                searches.remove(&sender.chat);
                Reply::in_place(format!(
                    "Then I do not know the place. Write more of its address, with the \
                     town. Or: {}",
                    lower_first(OTHER_WAYS)
                ))
            }
            _ => Reply::in_place(STALE_SEARCH),
        }
    }

    pub(super) fn searches(&self) -> MutexGuard<'_, HashMap<i64, Search>> {
        // Nothing is ever done under the lock that could leave the map half changed.
        self.searches.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
