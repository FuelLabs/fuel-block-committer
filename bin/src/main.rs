use actix::prelude::*;
use async_trait::async_trait;
use core::time::Duration;

#[async_trait]
trait BlockFetcher {}

struct FuelBlockFetcher {}

impl BlockFetcher for FuelBlockFetcher {}

struct BlockWatcher {
    last_block_height: u64,
    block_fetcher: Box<dyn BlockFetcher>,
    me: Option<Addr<BlockWatcher>>,
}

impl BlockWatcher {
    fn new(block_fetcher: impl BlockFetcher + 'static) -> Self {
        Self {
            last_block_height: 0,
            block_fetcher: Box::new(block_fetcher),
            me: None,
        }
    }
}

impl Actor for BlockWatcher {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        println!("I am alive!");

        // ctx.address().do_send(CheckNewBlock {});
    }

    fn stopped(&mut self, ctx: &mut Self::Context) {
        println!("i dead");
    }
}

impl Handler<CheckNewBlock> for BlockWatcher {
    type Result = (); // <- Message response type

    fn handle(&mut self, msg: CheckNewBlock, ctx: &mut Context<Self>) {
        println!("nani");
        self.me = Some(ctx.address());

        // wait 100 nanoseconds
        ctx.run_later(Duration::from_millis(500), move |act, _| {
            act.me.as_ref().unwrap().do_send(CheckNewBlock {});
        });
    }
}

#[derive(Message)]
#[rtype(result = "()")]
struct CheckNewBlock {}

fn main() {
    let system = System::new();

    let bw = BlockWatcher::new(FuelBlockFetcher {});
    system.block_on(async {
        let addr = bw.start();
        addr.do_send(CheckNewBlock {});
    });

    system.run().unwrap();
}
