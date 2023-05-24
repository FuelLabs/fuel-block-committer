use std::{println, time::Duration};

use actix::{Actor, Addr, AsyncContext, Context, Handler, Recipient};

use super::{
    eth_committer::EthCommitter,
    messages::{FuelStatus, ReportFuelStatus},
};
use crate::{
    actors::messages::{BlockUpdate, CheckNewBlock},
    adapters::block_fetcher::BlockFetcher,
};

pub struct BlockWatcher {
    check_interval: Duration,
    block_fetcher: Box<dyn BlockFetcher>,
    last_block_height: u64,
    subscriber: Recipient<BlockUpdate>,
}

impl BlockWatcher {
    pub fn new(
        check_interval: Duration,
        block_fetcher: impl BlockFetcher + 'static,
        subscriber: Recipient<BlockUpdate>,
    ) -> Self {
        Self {
            check_interval,
            block_fetcher: Box::new(block_fetcher),
            last_block_height: 0,
            subscriber,
        }
    }

    fn schedule_new_check(&self, ctx: &mut Context<Self>) {
        ctx.run_later(self.check_interval, |_, ctx| {
            ctx.address().do_send(CheckNewBlock {});
        });
    }

    fn log(&self, message: &str) {
        println!("BlockWatcher: {message}");
    }
}

impl Actor for BlockWatcher {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        ctx.address().do_send(CheckNewBlock {});
    }
}

impl Handler<CheckNewBlock> for BlockWatcher {
    type Result = (); // <- Message response type

    fn handle(&mut self, _msg: CheckNewBlock, ctx: &mut Context<Self>) {
        self.log("checking new block!");
        // TODO: HANDLE
        //self.block_fetcher.latest_block();
        self.schedule_new_check(ctx);
        //TODO: send BlockUpdate
        self.log("sending block update");
        self.subscriber.do_send(BlockUpdate {
            block: Default::default(),
        });
    }
}

impl Handler<ReportFuelStatus> for BlockWatcher {
    type Result = FuelStatus;

    fn handle(&mut self, _msg: ReportFuelStatus, ctx: &mut Context<Self>) -> FuelStatus {
        FuelStatus {
            latest_fuel_block: self.last_block_height,
        }
    }
}
