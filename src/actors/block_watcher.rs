use std::time::Duration;

use actix::{Actor, AsyncContext, Context, Handler, Message};

use crate::adapters::block_fetcher::BlockFetcher;

pub struct BlockWatcher {
    check_interval: Duration,
    block_fetcher: Box<dyn BlockFetcher>,
    last_block_height: u64,
}

impl BlockWatcher {
    pub fn new(check_interval: Duration, block_fetcher: impl BlockFetcher + 'static) -> Self {
        Self {
            check_interval,
            block_fetcher: Box::new(block_fetcher),
            last_block_height: 0,
        }
    }
}

impl BlockWatcher {
    fn schedule_new_check(&self, ctx: &mut Context<Self>) {
        ctx.run_later(self.check_interval, |_, ctx| {
            ctx.address().do_send(CheckNewBlock {});
        });
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
        println!("checking!");
        // TODO: HANDLE
        //self.block_fetcher.latest_block();
        self.schedule_new_check(ctx);
    }
}

#[derive(Message)]
#[rtype(result = "()")]
pub struct CheckNewBlock {}
