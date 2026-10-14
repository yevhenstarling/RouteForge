//! Placing an order: the draft a chat puts together, from the place it begins with
//! to the press that places it (D61, D76).

use super::*;

impl<A: Api, G: Geocoder> Chats<A, G> {
    pub(super) async fn order(&self, sender: &Sender, arguments: &str) -> Reply {
        if arguments.is_empty() {
            return Reply {
                text: format!("To place an order, {}", self.how_to_begin()),
                keys: Keys::ShareLocation,
                in_place: false,
            };
        }
        match coordinates(arguments) {
            Some((lat, lon)) => {
                let place = Place {
                    lat,
                    lon,
                    address: None,
                };
                self.begin(sender, place, false)
            }
            // What is no pair of numbers is an address, to a bot that looks them up.
            None if self.geocoder.is_on() => self.look_up(sender, arguments).await,
            None => Reply::new(
                "After /order I read a latitude and a longitude in degrees, as in \
                 /order 50.4501 30.5234",
            ),
        }
    }

    /// Begins an order at a place. An order the chat was putting together gives way.
    ///
    /// The first question is a message of its own under a location that was shared,
    /// and takes the place of the list a place was picked from.
    pub(super) fn begin(&self, sender: &Sender, place: Place, in_place: bool) -> Reply {
        let Ok(location) = LatLon::new(place.lat, place.lon) else {
            return Reply::new(
                "That is no position on Earth: a latitude runs from -90 to 90 and a \
                 longitude from -180 to 180.",
            );
        };
        let address = place
            .address
            .map(|address| {
                address
                    .trim()
                    .chars()
                    .take(ADDRESS_LONGEST)
                    .collect::<String>()
            })
            .filter(|address| !address.is_empty());
        let draft = Draft {
            id: sender.message,
            age: self.begun.fetch_add(1, Ordering::Relaxed),
            location,
            address,
            kind: None,
            parcels: None,
            day: None,
            key: IdempotencyKey::generate(),
        };
        let question = draft.question(sender.today, in_place);

        let mut drafts = self.drafts();
        drafts.insert(sender.chat, draft);
        if drafts.len() > MOST_DRAFTS {
            let oldest = drafts
                .iter()
                .min_by_key(|(_, draft)| draft.age)
                .map(|(chat, _)| *chat);
            if let Some(oldest) = oldest {
                drafts.remove(&oldest);
            }
        }
        question
    }

    /// Notes what a button said of the chat's draft, and asks for what comes next.
    pub(super) fn fill(&self, sender: &Sender, id: i32, note: impl FnOnce(&mut Draft)) -> Reply {
        let mut drafts = self.drafts();
        match drafts.get_mut(&sender.chat).filter(|draft| draft.id == id) {
            Some(draft) => {
                note(draft);
                // Another order now than the one its key may have gone out with. A
                // key answers for one order (D76): asked for with the old one, the
                // API would refuse the order for not being the one it has.
                draft.key = IdempotencyKey::generate();
                draft.question(sender.today, true)
            }
            None => Reply::in_place(STALE),
        }
    }

    pub(super) fn fill_day(&self, sender: &Sender, id: i32, day: NaiveDate) -> Reply {
        if on_offer(day, sender.today) {
            return self.fill(sender, id, |draft| draft.day = Some(day));
        }
        // A button that was left unpressed overnight names a day that has passed.
        let mut asked_again = self.fill(sender, id, |draft| draft.day = None);
        if asked_again.keys != Keys::None {
            asked_again.text = format!("That day is no longer on offer.\n\n{}", asked_again.text);
        }
        asked_again
    }

    /// Places the order the chat has put together: the one thing here that changes
    /// anything, done by the press of the button that says so and by nothing else.
    pub(super) async fn place(&self, sender: &Sender, id: i32) -> Reply {
        // Taken out before the API is asked. A second press that arrives while the
        // first is on its way finds nothing to place.
        let taken = {
            let mut drafts = self.drafts();
            match drafts.get(&sender.chat) {
                Some(draft) if draft.id == id => drafts.remove(&sender.chat),
                _ => None,
            }
        };
        let Some(mut draft) = taken else {
            return Reply::in_place(STALE);
        };
        let (Some(kind), Some(parcels), Some(day)) = (draft.kind, draft.parcels, draft.day) else {
            // Nothing offers this button before the order is whole.
            let question = draft.question(sender.today, true);
            self.put_back(sender.chat, draft);
            return question;
        };
        if !on_offer(day, sender.today) {
            // It cannot go out again before it has a day, and it gets another key
            // with that day, in `fill`: it is another order then.
            draft.day = None;
            let mut question = draft.question(sender.today, true);
            question.text = format!("That day is no longer on offer.\n\n{}", question.text);
            self.put_back(sender.chat, draft);
            return question;
        }

        let asked = OrderRequest {
            kind,
            service_date: day,
            location: draft.location,
            address: draft.address.clone(),
            demand: parcels,
        };
        let not_placed = match self.login(sender).await {
            Err(reply) => format!("{}\nThe order was not placed.", reply.text),
            Ok(session) => match self
                .api
                .create_order(&caller(&session, sender), &asked, &draft.key)
                .await
            {
                Ok(placed) => {
                    info!(order_id = %placed.id, service_date = %day, "placed an order");
                    return Reply::in_place(format!(
                        "Placed: {}.\nIt is pending until that day is planned. /orders shows it.",
                        described(&placed)
                    ));
                }
                // The request went out and no answer came back. The API may have
                // stored the order all the same. The draft is kept with its key, so
                // the next press asks for the same order and cannot place a second.
                Err(failure @ Failure::Away(_)) => {
                    warn!(%failure, "do not know yet whether an order was placed");
                    NO_ANSWER.to_owned()
                }
                Err(failure) => {
                    format!("{}\nThe order was not placed.", refused(&failure, &session))
                }
            },
        };
        // Still the chat's to place, or to drop.
        let mut question = draft.question(sender.today, true);
        question.text = format!("{not_placed}\n\n{}", question.text);
        self.put_back(sender.chat, draft);
        question
    }

    pub(super) fn drop_draft(&self, sender: &Sender, id: i32) -> Reply {
        let mut drafts = self.drafts();
        match drafts.get(&sender.chat) {
            Some(draft) if draft.id == id => {
                drafts.remove(&sender.chat);
                Reply::in_place("Dropped: nothing was placed.")
            }
            _ => Reply::in_place(STALE),
        }
    }

    pub(super) fn cancel(&self, sender: &Sender) -> Reply {
        // A search whose places still wait to be picked is the beginning of an order.
        let searched = self.searches().remove(&sender.chat).is_some();
        let drafted = self.drafts().remove(&sender.chat).is_some();
        if searched || drafted {
            Reply::new("Dropped: nothing was placed.")
        } else {
            Reply::new("You are not putting an order together, so there is nothing to drop.")
        }
    }

    pub(super) fn drafts(&self) -> MutexGuard<'_, HashMap<i64, Draft>> {
        // Nothing is ever done under the lock that could leave the map half changed.
        self.drafts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Gives a draft back to its chat, unless the chat has begun another meanwhile.
    pub(super) fn put_back(&self, chat: i64, draft: Draft) {
        self.drafts().entry(chat).or_insert(draft);
    }
}
