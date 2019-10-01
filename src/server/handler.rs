use eth::contracts::TRANSFER_EVENT_SIGNATURE;
use ethabi::Token;
use relay::{Network, NetworkType};
use server::endpoint::{NetworkStatus, RequestType, StatusResponse};
use std::cmp;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;
use tokio_core::reactor;
use transfers::past::{FindTransferInTransaction, ValidateAndApproveTransfer};
use web3::contract::tokens::{Detokenize, Tokenize};
use web3::contract::Options;
use web3::futures::future;
use web3::futures::prelude::*;
use web3::futures::sync::mpsc;
use web3::futures::try_ready;
use web3::types::{Address, BlockNumber, FilterBuilder, Log, U256};
use web3::{contract, DuplexTransport};

pub struct BalanceQuery {
    address: Address,
}

impl BalanceQuery {
    pub fn new(address: Address) -> Self {
        BalanceQuery { address }
    }
}

impl Tokenize for BalanceQuery {
    fn into_tokens(self) -> Vec<Token> {
        vec![Token::Address(self.address)]
    }
}

/// Withdrawal event added to contract after a transfer
#[derive(Debug, Clone)]
pub struct BalanceOf(U256);

impl Detokenize for BalanceOf {
    /// Creates a new instance from parsed ABI tokens.
    fn from_tokens(tokens: Vec<Token>) -> Result<Self, contract::Error>
    where
        Self: Sized,
    {
        let balance = tokens[0].clone().to_uint().ok_or_else(|| {
            contract::Error::from_kind(contract::ErrorKind::Msg(
                "cannot parse balance from contract response".to_string(),
            ))
        })?;
        debug!("balance of: {:?}", balance);
        Ok(BalanceOf(balance))
    }
}

pub struct HandleRequests<T: DuplexTransport + 'static> {
    listen: mpsc::UnboundedReceiver<RequestType>,
    homechain: Network<T>,
    sidechain: Network<T>,
    handle: reactor::Handle,
}

impl<T: DuplexTransport + 'static> HandleRequests<T> {
    /// Returns a newly created HandleMissedTransfers Future
    ///
    /// # Arguments
    ///
    /// * `source` - Network where the missed transfers are captured
    /// * `target` - Network where the transfer will be approved for a withdrawal
    /// * `rx` - Receiver where requested RequestTypes will come across
    /// * `handle` - Handle to spawn new futures
    pub fn new(
        homechain: &Network<T>,
        sidechain: &Network<T>,
        rx: mpsc::UnboundedReceiver<RequestType>,
        handle: &reactor::Handle,
    ) -> Self {
        HandleRequests {
            listen: rx,
            homechain: homechain.clone(),
            sidechain: sidechain.clone(),
            handle: handle.clone(),
        }
    }
}

impl<T: DuplexTransport + 'static> Future for HandleRequests<T> {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let homechain = self.homechain.clone();
            let sidechain = self.sidechain.clone();
            let handle = self.handle.clone();
            let request = try_ready!(self.listen.poll());
            match request {
                Some(RequestType::Hash(chain, tx_hash)) => {
                    let handle = self.handle.clone();
                    let (source, target) = match chain {
                        NetworkType::Home => (homechain, sidechain),
                        NetworkType::Side => (sidechain, homechain),
                    };
                    let future = FindTransferInTransaction::new(&source, &tx_hash)
                        .and_then(move |transfers| {
                            let handle = handle.clone();
                            let source = source.clone();
                            let target = target.clone();
                            let futures: Vec<ValidateAndApproveTransfer<T>> = transfers
                                .iter()
                                .map(move |transfer| {
                                    let handle = handle.clone();
                                    let target = target.clone();
                                    ValidateAndApproveTransfer::new(&source, &target, &handle, &transfer)
                                })
                                .collect();
                            future::join_all(futures)
                        })
                        .and_then(|_| Ok(()))
                        .or_else(move |_| {
                            // No log here, errors are caught in Futures
                            Ok(())
                        });
                    self.handle.spawn(future);
                }
                Some(RequestType::Status(ref tx)) => handle.spawn(StatusCheck::new(&homechain, &sidechain, tx)),
                None => {}
            };
        }
    }
}

pub struct StatusCheck {
    future: Box<Future<Item = Vec<Option<U256>>, Error = ()>>,
    tx: mpsc::UnboundedSender<Result<StatusResponse, ()>>,
}

impl StatusCheck {
    fn new<T: DuplexTransport + 'static>(
        homechain: &Network<T>,
        sidechain: &Network<T>,
        tx: &mpsc::UnboundedSender<Result<StatusResponse, ()>>,
    ) -> Self {
        let home_eth_future = homechain
            .web3
            .eth()
            .balance(homechain.account, None)
            .and_then(move |balance| Ok(Some(balance)))
            .or_else(|_| Ok(None));

        let home_balance_query = BalanceQuery::new(homechain.relay.address());
        let home_nct_future = homechain
            .token
            .query::<BalanceOf, Address, BlockNumber, BalanceQuery>(
                "balanceOf",
                home_balance_query,
                homechain.account,
                Options::default(),
                BlockNumber::Latest,
            )
            .and_then(move |balance| Ok(Some(balance.0)))
            .or_else(move |_| Ok(None));

        let home_last_block_future = homechain
            .web3
            .eth()
            .block_number()
            .and_then(move |block| Ok(Some(block)))
            .or_else(|_| Ok(None));

        let side_eth_future = sidechain
            .web3
            .eth()
            .balance(sidechain.account, None)
            .and_then(move |balance| Ok(Some(balance)))
            .or_else(|_| Ok(None));
        let side_balance_query = BalanceQuery::new(sidechain.relay.address());
        let side_nct_future = sidechain
            .token
            .query::<BalanceOf, Address, BlockNumber, BalanceQuery>(
                "balanceOf",
                side_balance_query,
                sidechain.account,
                Options::default(),
                BlockNumber::Latest,
            )
            .and_then(move |balance| Ok(Some(balance.0)))
            .or_else(move |_| Ok(None));

        let side_last_block_future = sidechain
            .web3
            .eth()
            .block_number()
            .and_then(move |block| Ok(Some(block)))
            .or_else(|_| Ok(None));

        let futures: Vec<Box<Future<Item = Option<U256>, Error = ()>>> = vec![
            Box::new(home_eth_future),
            Box::new(home_last_block_future),
            Box::new(home_nct_future),
            Box::new(side_eth_future),
            Box::new(side_last_block_future),
            Box::new(side_nct_future),
        ];
        let future = Box::new(future::join_all(futures));
        StatusCheck { future, tx: tx.clone() }
    }
}

impl Future for StatusCheck {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let results = self.future.poll();
        match results {
            Ok(Async::Ready(results)) => {
                let home = NetworkStatus::new(results[0], results[1], results[2]);
                let side = NetworkStatus::new(results[3], results[4], results[5]);
                let send_result = self.tx.unbounded_send(Ok(StatusResponse::new(home, side)));
                if send_result.is_err() {
                    error!("error sending status response");
                }
            }
            Ok(Async::NotReady) => return Ok(Async::NotReady),
            Err(e) => {
                error!("error getting status: {:?}", e);
                let send_result = self.tx.unbounded_send(Err(()));
                if send_result.is_err() {
                    error!("error sending status response");
                }
            }
        };
        Ok(Async::Ready(()))
    }
}
