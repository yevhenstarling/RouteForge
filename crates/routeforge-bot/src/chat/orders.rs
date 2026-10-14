//! A person's own orders: the list of them, one of them, and withdrawing it.

use super::*;

impl<A: Api, G: Geocoder> Chats<A, G> {
    pub(super) async fn orders(&self, sender: &Sender) -> Reply {
        let session = match self.login(sender).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        // Their own, also for an operator, who would otherwise be shown everybody's.
        let mine = OrderFilter {
            client_id: Some(session.user.id),
            ..OrderFilter::default()
        };
        let orders = match self.every_order(&caller(&session, sender), mine).await {
            Ok(orders) => orders,
            Err(failure) => return Reply::new(refused(&failure, &session)),
        };
        let mut coming: Vec<&Order> = orders
            .iter()
            .filter(|order| order.service_date >= sender.today)
            .filter(|order| order.status != OrderStatus::Cancelled)
            .collect();
        coming.sort_by_key(|order| (order.service_date, order.id));
        if coming.is_empty() {
            return Reply::new(
                "You have no orders for today or later. Share a location to place one.",
            );
        }

        let text = if coming.len() > ORDERS_SHOWN {
            format!(
                "You have {} orders for today or later. These are the first {ORDERS_SHOWN}. \
                 Press one to look at it, or to withdraw it.",
                coming.len()
            )
        } else {
            format!(
                "You have {} for today or later. Press one to look at it, or to withdraw it.",
                some(coming.len(), "order", "orders")
            )
        };
        let rows = coming
            .iter()
            .take(ORDERS_SHOWN)
            .map(|order| {
                key(
                    format!(
                        "{}: {} of {}, {}",
                        name(order.service_date),
                        order.kind,
                        order.demand,
                        order.status
                    ),
                    Press::Show(order.id),
                )
            })
            .collect();
        Reply::new(text).with(rows)
    }

    /// Every order that passes the filter, read a page at a time: each page is asked
    /// for by the id of the last order of the page before (D44).
    pub(super) async fn every_order(
        &self,
        who: &Caller<'_>,
        mut filter: OrderFilter,
    ) -> Result<Vec<Order>, Failure> {
        filter.limit = Some(PAGE);
        let mut orders = Vec::new();
        for _ in 0..MOST_PAGES {
            let page = self.api.list_orders(who, &filter).await?;
            // A page that is not full is the last.
            let next = page
                .last()
                .map(|last| last.id)
                .filter(|_| page.len() >= PAGE as usize);
            orders.extend(page);
            match next {
                Some(last) => filter.after = Some(last),
                None => return Ok(orders),
            }
        }
        warn!(
            read = orders.len(),
            "there are more orders than the bot reads at once"
        );
        Ok(orders)
    }

    pub(super) async fn show(&self, sender: &Sender, id: OrderId, in_place: bool) -> Reply {
        let session = match self.login(sender).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        let reply = match self.api.get_order(&caller(&session, sender), id).await {
            Ok(order) => {
                let text = format!(
                    "{}\n{}\nOrder {}.",
                    sentence(&described(&order)),
                    standing(order.status),
                    order.id
                );
                if order.status == OrderStatus::Pending {
                    Reply::new(text).with(vec![key("Withdraw it", Press::Withdraw(order.id))])
                } else {
                    Reply::new(text)
                }
            }
            Err(failure) => Reply::new(refused(&failure, &session)),
        };
        Reply { in_place, ..reply }
    }

    /// Asks whether to withdraw an order. Nothing is withdrawn here.
    pub(super) async fn ask_to_withdraw(&self, sender: &Sender, id: OrderId) -> Reply {
        let session = match self.login(sender).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        match self.api.get_order(&caller(&session, sender), id).await {
            Ok(order) if order.status == OrderStatus::Pending => Reply::in_place(format!(
                "Withdraw this order?\n{}",
                sentence(&described(&order))
            ))
            .with(vec![
                key("Yes, withdraw it", Press::WithdrawYes(id)),
                key("No, keep it", Press::Keep(id)),
            ]),
            // It is no longer one that can be withdrawn. How it stands says why.
            Ok(_) => self.show(sender, id, true).await,
            Err(failure) => Reply::in_place(refused(&failure, &session)),
        }
    }

    pub(super) async fn withdraw(&self, sender: &Sender, id: OrderId) -> Reply {
        let session = match self.login(sender).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        match self.api.cancel_order(&caller(&session, sender), id).await {
            Ok(order) => {
                info!(order_id = %order.id, "withdrew an order");
                Reply::in_place(format!("Withdrawn: {}.", described(&order)))
            }
            Err(failure) => Reply::in_place(format!(
                "{}\nThe order was not withdrawn.",
                refused(&failure, &session)
            )),
        }
    }
}
