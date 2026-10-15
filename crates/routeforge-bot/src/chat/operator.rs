//! An operator's day: its pending orders, a plan for it, and how far that has got.

use super::*;

impl<A: Api, G: Geocoder> Chats<A, G> {
    /// Asks which day an operator means, with a button for each day on offer.
    pub(super) async fn ask_day(
        &self,
        sender: &Sender,
        question: &str,
        press: fn(NaiveDate) -> Press,
    ) -> Reply {
        match self.staff(sender).await {
            Ok(_) => Reply::new(question).with(days(sender.today, press)),
            Err(reply) => reply,
        }
    }

    pub(super) async fn pending(&self, sender: &Sender, day: NaiveDate) -> Reply {
        let session = match self.staff(sender).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        let waiting = OrderFilter {
            service_date: Some(day),
            status: Some(OrderStatus::Pending),
            ..OrderFilter::default()
        };
        let orders = match self.every_order(&caller(&session, sender), waiting).await {
            Ok(orders) => orders,
            Err(failure) => return Reply::in_place(refused(&failure, &session)),
        };
        if orders.is_empty() {
            return Reply::in_place(format!("{}: no order is pending.", name(day)));
        }
        let parcels: u64 = orders.iter().map(|order| u64::from(order.demand)).sum();
        let deliveries = orders
            .iter()
            .filter(|order| order.kind == OrderKind::Delivery)
            .count();
        let text = format!(
            "{}: {} pending, {parcels} parcels in all: {} and {}.",
            name(day),
            if orders.len() == 1 {
                "1 order is".to_owned()
            } else {
                format!("{} orders are", orders.len())
            },
            some(deliveries, "delivery", "deliveries"),
            some(orders.len() - deliveries, "pickup", "pickups"),
        );
        Reply::in_place(text).with(vec![key(format!("Plan {}", name(day)), Press::Plan(day))])
    }

    /// Asks whether to plan a day. Nothing is planned here.
    pub(super) async fn ask_to_plan(&self, sender: &Sender, day: NaiveDate) -> Reply {
        if let Err(reply) = self.staff(sender).await {
            return reply;
        }
        if day < sender.today {
            return Reply::in_place(format!(
                "{} has passed. /plan asks for a day again.",
                name(day)
            ));
        }
        Reply::in_place(format!(
            "Plan {}?\nIts pending orders are given to the couriers, and a route is searched \
             for each of them.",
            name(day)
        ))
        .with(vec![
            key("Yes, plan it", Press::PlanYes(day)),
            key("No", Press::PlanNo),
        ])
    }

    pub(super) async fn plan(&self, sender: &Sender, day: NaiveDate) -> Reply {
        let session = match self.staff(sender).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        // The seed and the budget are left to the API, which gives a plan its own (D45).
        let asked = PlanRequest {
            service_date: day,
            seed: None,
            budget: None,
        };
        match self
            .api
            .create_plan(&caller(&session, sender), &asked)
            .await
        {
            Ok(plan) => {
                info!(plan_id = %plan.id, service_date = %day, "asked for a plan");
                Reply::in_place(format!(
                    "Asked for: the plan of {} is {}. /status says how far it is.",
                    name(day),
                    plan.status
                ))
            }
            Err(failure) => Reply::in_place(format!(
                "{}\nNothing was planned.",
                refused(&failure, &session)
            )),
        }
    }

    pub(super) async fn status(&self, sender: &Sender, day: NaiveDate) -> Reply {
        let session = match self.staff(sender).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        let text = match self.api.list_plans(&caller(&session, sender), day).await {
            // The newest is first, and is the one that counts: the others are over.
            Ok(plans) => match plans.first() {
                Some(plan) => format!("{}: {}", name(day), how_far(plan)),
                None => format!("{}: no plan has been asked for.", name(day)),
            },
            Err(failure) => refused(&failure, &session),
        };
        // The days stay under it. Pressing one again looks again.
        Reply::in_place(text).with(days(sender.today, Press::Status))
    }
}
