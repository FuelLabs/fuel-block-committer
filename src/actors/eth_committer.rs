use std::time::Duration;

use actix::{Actor, AsyncContext, Context, Handler};
use fuels::tx::Bytes32;

use crate::actors::messages::{BlockUpdate, CheckTxStatus};
use crate::adapters::storage::Storage;
use crate::adapters::tx_status::TxStatusProvider;
use crate::adapters::tx_submitter::TxSubmitter;
use crate::common::EthTxStatus;

pub struct EthCommitter {
    check_interval: Duration,
    tx_status_provider: Box<dyn TxStatusProvider>,
    storage: Box<dyn Storage>,
    tx_submitter: Box<dyn TxSubmitter>,
    latest_block_id: Bytes32,
    latest_block_status: EthTxStatus,
}

impl EthCommitter {
    pub fn new(
        check_interval: Duration,
        tx_status_provider: impl TxStatusProvider + 'static,
        tx_submitter: impl TxSubmitter + 'static,
        storage: impl Storage + 'static,
    ) -> Self {
        Self {
            check_interval,
            tx_status_provider: Box::new(tx_status_provider),
            tx_submitter: Box::new(tx_submitter),
            storage: Box::new(storage),
            latest_block_id: Default::default(),
            latest_block_status: EthTxStatus::Pending,
        }
    }

    fn schedule_new_check(&self, ctx: &mut Context<Self>) {
        ctx.run_later(self.check_interval, |_, ctx| {
            ctx.address().do_send(CheckTxStatus {});
        });
    }

    fn log(&self, message: &str) {
        println!("EthCommitter: {message}");
    }
}

impl Actor for EthCommitter {
    type Context = Context<Self>;

    fn started(&mut self, _ctx: &mut Self::Context) {
        //TODO: read last block id and status from DB
    }
}

impl Handler<BlockUpdate> for EthCommitter {
    type Result = (); // <- Message response type

    fn handle(&mut self, _msg: BlockUpdate, ctx: &mut Context<Self>) {
        self.log("got block update");
        //TODO: implement block update logic
        // decide what to do with block
        // if sent add to db id and status Pending update self
        // schedule check status
        self.schedule_new_check(ctx);
    }
}

impl Handler<CheckTxStatus> for EthCommitter {
    type Result = (); // <- Message response type

    fn handle(&mut self, _msg: CheckTxStatus, ctx: &mut Context<Self>) {
        self.log("check eth tx status");
        //TODO: if not successful schedule new check or abort
        // if change (Commited/Aborted) write to DB and update self
        self.schedule_new_check(ctx);
    }
}
