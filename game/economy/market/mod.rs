use kay::{ActorSystem, Fate, World};
use kay::swarm::Swarm;
use compact::{CVec, CDict};
use super::resources::{ResourceMap, ResourceId, ResourceAmount};
use super::households::{HouseholdID, MemberIdx};
use core::simulation::{TimeOfDay, Seconds, Timestamp};

#[derive(Compact, Clone)]
pub struct Deal {
    pub duration: Seconds,
    pub take: ResourceMap<ResourceAmount>,
    pub give: (ResourceId, ResourceAmount),
}

impl Deal {
    pub fn new<T: IntoIterator<Item = (ResourceId, ResourceAmount)>>(
        give: (ResourceId, ResourceAmount),
        take: T,
        duration: Seconds,
    ) -> Self {
        Deal {
            duration,
            give,
            take: take.into_iter().collect(),
        }
    }
}

#[derive(Compact, Clone)]
pub struct Offer {
    id: OfferID,
    by: HouseholdID,
    location: RoughLocationID,
    from: TimeOfDay,
    to: TimeOfDay,
    deal: Deal,
    users: CVec<(HouseholdID, Option<MemberIdx>)>,
}

impl Offer {
    pub fn register(
        id: OfferID,
        by: HouseholdID,
        location: RoughLocationID,
        from: TimeOfDay,
        to: TimeOfDay,
        deal: &Deal,
        world: &mut World,
    ) -> Offer {
        // TODO: ugly singleton send
        MarketID::broadcast(world).register(deal.give.0, id, world);

        Offer {
            id,
            by,
            location,
            from,
            to,
            deal: deal.clone(),
            users: CVec::new(),
        }
    }

    // The offer stays alive until the withdrawal is confirmed
    // to prevent offers being used while they're being withdrawn
    pub fn withdraw(&mut self, world: &mut World) {
        // TODO: notify users and wait for their confirmation as well

        // TODO: ugly singleton send
        MarketID::broadcast(world).withdraw(self.deal.give.0, self.id, world);
    }

    pub fn withdrawal_confirmed(&mut self, _: &mut World) -> Fate {
        Fate::Die
    }

    pub fn evaluate(
        &mut self,
        tick: Timestamp,
        location: RoughLocationID,
        requester: EvaluationRequesterID,
        world: &mut World,
    ) {
        if TimeOfDay::from_tick(tick) < self.to {
            let search_result = EvaluatedSearchResult {
                resource: self.deal.give.0,
                evaluated_deals: vec![
                    EvaluatedDeal {
                        offer: self.id,
                        deal: self.deal.clone(),
                        from: self.from,
                        to: self.to,
                    },
                ].into(),
            };
            TripCostEstimatorID::spawn(
                requester,
                location,
                self.location,
                search_result,
                tick,
                world,
            );
        } else {
            requester.on_result(
                EvaluatedSearchResult {
                    resource: self.deal.give.0,
                    evaluated_deals: CVec::new(),
                },
                world,
            );
        }
    }

    pub fn get_receivable_deal(
        &mut self,
        household: HouseholdID,
        member: MemberIdx,
        world: &mut World,
    ) {
        self.by.provide_deal(self.deal.clone(), world);
        household.receive_deal(self.deal.clone(), member, world);
    }

    pub fn started_using(
        &mut self,
        household: HouseholdID,
        member: Option<MemberIdx>,
        _: &mut World,
    ) {
        self.users.push((household, member));
    }

    pub fn stopped_using(
        &mut self,
        household: HouseholdID,
        member: Option<MemberIdx>,
        _: &mut World,
    ) {
        self.users.retain(|&(o_household, o_member)| {
            o_household != household || o_member != member
        });
    }
}

use transport::pathfinding::{RoughLocation, RoughLocationID,
                             MSG_RoughLocation_resolve_as_location, LocationRequesterID,
                             MSG_LocationRequester_location_resolved};

impl RoughLocation for Offer {
    fn resolve_as_location(
        &mut self,
        requester: LocationRequesterID,
        rough_location: RoughLocationID,
        tick: Timestamp,
        world: &mut World,
    ) {
        self.location.resolve_as_location(
            requester,
            rough_location,
            tick,
            world,
        );
    }
}

pub trait EvaluationRequester {
    fn expect_n_results(&mut self, resource: ResourceId, n: usize, world: &mut World);
    fn on_result(&mut self, result: &EvaluatedSearchResult, world: &mut World);
}

#[derive(Compact, Clone)]
pub struct Market {
    id: MarketID,
    offers_by_resource: CDict<ResourceId, CVec<OfferID>>,
}

use economy::resources::r_info;

impl Market {
    pub fn spawn(id: MarketID, _: &mut World) -> Market {
        Market { id, offers_by_resource: CDict::new() }
    }

    pub fn search(
        &mut self,
        tick: Timestamp,
        location: RoughLocationID,
        resource: ResourceId,
        requester: EvaluationRequesterID,
        world: &mut World,
    ) {
        let n_to_expect = if let Some(offers) = self.offers_by_resource.get(resource) {
            for offer in offers.iter() {
                offer.evaluate(tick, location, requester, world);
            }

            offers.len()
        } else {
            0
        };

        println!("{} offers for {}", n_to_expect, r_info(resource).0);

        requester.expect_n_results(resource, n_to_expect, world);
    }

    pub fn register(&mut self, resource: ResourceId, offer: OfferID, _: &mut World) {
        self.offers_by_resource.push_at(resource, offer);
    }

    pub fn withdraw(&mut self, resource: ResourceId, offer: OfferID, world: &mut World) {
        if let Some(offers) = self.offers_by_resource.get_mut(resource) {
            offers.retain(|o| *o != offer);
        }
        offer.withdrawal_confirmed(world);
    }
}

#[derive(Compact, Clone)]
pub struct EvaluatedDeal {
    pub offer: OfferID,
    pub deal: Deal,
    pub from: TimeOfDay,
    pub to: TimeOfDay,
}

#[derive(Compact, Clone)]
pub struct EvaluatedSearchResult {
    pub resource: ResourceId,
    pub evaluated_deals: CVec<EvaluatedDeal>,
}

use transport::pathfinding::{Location, LocationRequester, GetDistanceTo, DistanceRequester,
                             DistanceRequesterID, MSG_DistanceRequester_on_distance};

#[derive(Compact, Clone)]
pub struct TripCostEstimator {
    id: TripCostEstimatorID,
    requester: EvaluationRequesterID,
    rough_source: RoughLocationID,
    source: Option<Location>,
    rough_destination: RoughLocationID,
    destination: Option<Location>,
    n_resolved: u8,
    base_result: EvaluatedSearchResult,
}

impl TripCostEstimator {
    pub fn spawn(
        id: TripCostEstimatorID,
        requester: EvaluationRequesterID,
        rough_source: RoughLocationID,
        rough_destination: RoughLocationID,
        base_result: &EvaluatedSearchResult,
        tick: Timestamp,
        world: &mut World,
    ) -> TripCostEstimator {
        rough_source.resolve_as_location(id.into(), rough_source, tick, world);
        rough_destination.resolve_as_location(id.into(), rough_destination, tick, world);

        TripCostEstimator {
            id,
            requester,
            rough_source,
            rough_destination,
            base_result: base_result.clone(),
            source: None,
            n_resolved: 0,
            destination: None,
        }
    }

    pub fn done(&mut self, _: &mut World) -> Fate {
        Fate::Die
    }
}

impl LocationRequester for TripCostEstimator {
    fn location_resolved(
        &mut self,
        rough_location: RoughLocationID,
        location: Option<Location>,
        _tick: Timestamp,
        world: &mut World,
    ) {
        if self.rough_source == rough_location {
            self.source = location;
        } else if self.rough_destination == rough_location {
            self.destination = location;
        } else {
            panic!("Should have this rough source/destination")
        }

        self.n_resolved += 1;

        if let (Some(source), Some(destination)) = (self.source, self.destination) {
            world.send(
                source.node,
                GetDistanceTo { destination, requester: self.id.into() },
            );
        } else if self.n_resolved == 2 {
            println!(
                "Either source or dest not resolvable for {}",
                r_info(self.base_result.resource).0
            );
            self.requester.on_result(
                EvaluatedSearchResult {
                    resource: self.base_result.resource,
                    evaluated_deals: CVec::new(),
                },
                world,
            );
            self.id.done(world);
        }
    }
}

impl DistanceRequester for TripCostEstimator {
    fn on_distance(&mut self, maybe_distance: Option<f32>, world: &mut World) {
        const ASSUMED_AVG_SPEED: f32 = 10.0; // m/s

        let result = if let Some(distance) = maybe_distance {
            EvaluatedSearchResult {
                evaluated_deals: self.base_result
                    .evaluated_deals
                    .iter()
                    .map(|evaluated_deal| {
                        let estimated_travel_time =
                            Seconds((distance / ASSUMED_AVG_SPEED) as usize);
                        let mut new_deal = evaluated_deal.clone();
                        new_deal.deal.duration += estimated_travel_time;
                        new_deal.from -= estimated_travel_time;
                        new_deal.to -= estimated_travel_time;
                        // TODO: adjust possible-until and resources
                        new_deal
                    })
                    .collect(),
                ..self.base_result
            }
        } else {
            println!(
                "No distance for {}, from {:?} to {:?}",
                r_info(self.base_result.resource).0,
                self.source,
                self.destination
            );
            EvaluatedSearchResult {
                resource: self.base_result.resource,
                evaluated_deals: CVec::new(),
            }
        };
        self.requester.on_result(result, world);
        self.id.done(world);
    }
}

pub fn setup(system: &mut ActorSystem) {
    system.add(Swarm::<Offer>::new(), |_| {});
    system.add(Swarm::<Market>::new(), |_| {});
    system.add(Swarm::<TripCostEstimator>::new(), |_| {});

    kay_auto::auto_setup(system);
    MarketID::spawn(&mut system.world());
}

mod kay_auto;
pub use self::kay_auto::*;
