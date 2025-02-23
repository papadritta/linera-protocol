// Copyright (c) Zefchain Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(target_arch = "wasm32", no_main)]

mod state;
use crate::state::{KeyBook, OrderEntry};
use matching_engine::{
    product_price_amount, ApplicationCall, Message, Operation, Order, OrderId, OrderNature, Price,
};
use state::{LevelView, MatchingEngine, MatchingEngineError};
use std::cmp::min;

use async_trait::async_trait;
use fungible::{Account, Destination, FungibleTokenAbi};
use linera_sdk::{
    base::{AccountOwner, Amount, ApplicationId, Owner, SessionId, WithContractAbi},
    contract::system_api,
    ensure, ApplicationCallOutcome, CalleeContext, Contract, ExecutionOutcome, MessageContext,
    OperationContext, OutgoingMessage, Resources, SessionCallOutcome, ViewStateStorage,
};

linera_sdk::contract!(MatchingEngine);

impl WithContractAbi for MatchingEngine {
    type Abi = matching_engine::MatchingEngineAbi;
}

/// An order can be cancelled which removes it totally or
/// modified which is a partial cancellation. The size of the
/// order can never increase.
#[derive(Clone, Debug)]
enum ModifyAmount {
    All,
    Partial(Amount),
}

/// Transfer operation back to the owners
#[derive(Clone)]
pub struct Transfer {
    /// Beneficiary of the transfer
    pub owner: AccountOwner,
    /// Amount being transferred
    pub amount: Amount,
    /// Index of the token being transferred (0 or 1)
    pub token_idx: u32,
}

#[async_trait]
impl Contract for MatchingEngine {
    type Error = MatchingEngineError;
    type Storage = ViewStateStorage<Self>;

    async fn initialize(
        &mut self,
        _context: &OperationContext,
        _argument: (),
    ) -> Result<ExecutionOutcome<Self::Message>, Self::Error> {
        // Validate that the application parameters were configured correctly.
        assert!(Self::parameters().is_ok());

        Ok(ExecutionOutcome::default())
    }

    /// Executes the order operation.
    /// If the chain is the one of the matching engine then the order is processed
    /// locally otherwise, it gets transmitted as a message to the chain of the engine.
    async fn execute_operation(
        &mut self,
        context: &OperationContext,
        operation: Operation,
    ) -> Result<ExecutionOutcome<Self::Message>, Self::Error> {
        let mut outcome = ExecutionOutcome::default();
        match operation {
            Operation::ExecuteOrder { order } => {
                let owner = Self::get_owner(&order);
                Self::check_account_authentication(None, context.authenticated_signer, owner)?;
                if context.chain_id == system_api::current_application_id().creation.chain_id {
                    self.execute_order_local(order).await?;
                } else {
                    self.execute_order_remote(&mut outcome, order)?;
                }
            }
        }
        Ok(outcome)
    }

    /// Execution of the order on the creation chain
    async fn execute_message(
        &mut self,
        context: &MessageContext,
        message: Message,
    ) -> Result<ExecutionOutcome<Self::Message>, Self::Error> {
        ensure!(
            context.chain_id == system_api::current_application_id().creation.chain_id,
            Self::Error::MatchingEngineChainOnly
        );
        match message {
            Message::ExecuteOrder { order } => {
                let owner = Self::get_owner(&order);
                Self::check_account_authentication(None, context.authenticated_signer, owner)?;
                self.execute_order_local(order).await?;
            }
        }
        Ok(ExecutionOutcome::default())
    }

    /// Execution of the message from the application. The application call can be a local
    /// one or a remote one.
    async fn handle_application_call(
        &mut self,
        context: &CalleeContext,
        argument: ApplicationCall,
        _sessions: Vec<SessionId>,
    ) -> Result<
        ApplicationCallOutcome<Self::Message, Self::Response, Self::SessionState>,
        Self::Error,
    > {
        let mut outcome = ApplicationCallOutcome::default();
        match argument {
            ApplicationCall::ExecuteOrder { order } => {
                let owner = Self::get_owner(&order);
                Self::check_account_authentication(
                    context.authenticated_caller_id,
                    context.authenticated_signer,
                    owner,
                )?;
                if context.chain_id == system_api::current_application_id().creation.chain_id {
                    self.execute_order_local(order).await?;
                } else {
                    self.execute_order_remote(&mut outcome.execution_outcome, order)?;
                }
            }
        }
        Ok(outcome)
    }

    async fn handle_session_call(
        &mut self,
        _context: &CalleeContext,
        _state: Self::SessionState,
        _call: (),
        _forwarded_sessions: Vec<SessionId>,
    ) -> Result<SessionCallOutcome<Self::Message, Self::Response, Self::SessionState>, Self::Error>
    {
        Err(Self::Error::SessionsNotSupported)
    }
}

impl MatchingEngine {
    /// Get the owner from the order
    fn get_owner(order: &Order) -> AccountOwner {
        match order {
            Order::Insert {
                owner,
                amount: _,
                nature: _,
                price: _,
            } => *owner,
            Order::Cancel { owner, order_id: _ } => *owner,
            Order::Modify {
                owner,
                order_id: _,
                cancel_amount: _,
            } => *owner,
        }
    }

    /// authenticate the originator of the message
    fn check_account_authentication(
        authenticated_application_id: Option<ApplicationId>,
        authenticated_signer: Option<Owner>,
        owner: AccountOwner,
    ) -> Result<(), MatchingEngineError> {
        match owner {
            AccountOwner::User(address) if authenticated_signer == Some(address) => Ok(()),
            AccountOwner::Application(id) if authenticated_application_id == Some(id) => Ok(()),
            _ => Err(MatchingEngineError::IncorrectAuthentication),
        }
    }

    /// The application engine is trading between two tokens. Those tokens are the parameters of the
    /// construction of the exchange and are accessed by index in the system.
    fn fungible_id(token_idx: u32) -> Result<ApplicationId<FungibleTokenAbi>, MatchingEngineError> {
        let parameter = Self::parameters()?;
        Ok(parameter.tokens[token_idx as usize])
    }

    /// Calls into the Fungible Token application to receive tokens from the given account.
    fn receive_from_account(
        &mut self,
        owner: &AccountOwner,
        amount: &Amount,
        nature: &OrderNature,
        price: &Price,
    ) -> Result<(), MatchingEngineError> {
        let account = Account {
            chain_id: system_api::current_chain_id(),
            owner: AccountOwner::Application(system_api::current_application_id()),
        };
        let destination = Destination::Account(account);
        let (amount, token_idx) = Self::get_amount_idx(nature, price, amount);
        self.transfer(*owner, amount, destination, token_idx)
    }

    /// Transfers `amount` tokens from the funds in custody to the `destination`.
    fn send_to(&mut self, transfer: Transfer) -> Result<(), MatchingEngineError> {
        let account = Account {
            chain_id: system_api::current_chain_id(),
            owner: transfer.owner,
        };
        let destination = Destination::Account(account);
        let owner_app = AccountOwner::Application(system_api::current_application_id());
        self.transfer(owner_app, transfer.amount, destination, transfer.token_idx)
    }

    /// Transfers tokens from the owner to the destination
    fn transfer(
        &mut self,
        owner: AccountOwner,
        amount: Amount,
        destination: Destination,
        token_idx: u32,
    ) -> Result<(), MatchingEngineError> {
        let transfer = fungible::ApplicationCall::Transfer {
            owner,
            amount,
            destination,
        };
        let token = Self::fungible_id(token_idx).expect("failed to get the token");
        self.call_application(true, token, &transfer, vec![])?;
        Ok(())
    }

    /// Execution of orders. There are three kinds:
    /// * Cancel for total cancellation
    /// * Modify where the order is partially cancelled
    /// * Insertion order where an order is inserted into the system. It goes into following steps:
    ///   - Transfer of tokens corresponding to the order in question so that it can be paid
    ///     to the counterparty.
    ///   - Insertion of the order into the market and immediately uncrossing the market that
    ///     is making sure that at the end we have best bid < best ask.
    ///   - Creation of the corresponding orders and operation of the corresponding transfers
    async fn execute_order_local(&mut self, order: Order) -> Result<(), MatchingEngineError> {
        match order {
            Order::Insert {
                owner,
                amount,
                nature,
                price,
            } => {
                self.receive_from_account(&owner, &amount, &nature, &price)?;
                let transfers = self
                    .insert_and_uncross_market(&owner, amount, nature, &price)
                    .await?;
                for transfer in transfers {
                    self.send_to(transfer)?;
                }
            }
            Order::Cancel { owner, order_id } => {
                self.modify_order_check(order_id, ModifyAmount::All, &owner)
                    .await?;
            }
            Order::Modify {
                owner,
                order_id,
                cancel_amount,
            } => {
                self.modify_order_check(order_id, ModifyAmount::Partial(cancel_amount), &owner)
                    .await?;
            }
        }
        Ok(())
    }

    /// Returns amount and type of tokens that need to be transferred to the matching engine when
    /// an order is added:
    /// * For an ask, just the token1 have to be put forward
    /// * For a bid, the product of the price with the amount has to be put
    fn get_amount_idx(nature: &OrderNature, price: &Price, amount: &Amount) -> (Amount, u32) {
        match nature {
            OrderNature::Bid => {
                let size0 = product_price_amount(*price, *amount);
                (size0, 0)
            }
            OrderNature::Ask => (*amount, 1),
        }
    }

    /// Execution of the remote order. This is done in two steps:
    /// * Transfer of the token (under the same owner to the chain of the matching engine)
    ///   This is similar to the code for the crowd-funding.
    /// * Creation of the message that will represent the order on the chain of the matching
    ///   engine
    fn execute_order_remote(
        &mut self,
        outcome: &mut ExecutionOutcome<Message>,
        order: Order,
    ) -> Result<(), MatchingEngineError> {
        let chain_id = system_api::current_application_id().creation.chain_id;
        let message = Message::ExecuteOrder {
            order: order.clone(),
        };
        if let Order::Insert {
            owner,
            amount,
            nature,
            price,
        } = order
        {
            // First, move the funds to the matching engine chain (under the same owner).
            let destination = fungible::Destination::Account(Account { chain_id, owner });
            let (amount, token_idx) = Self::get_amount_idx(&nature, &price, &amount);
            self.transfer(owner, amount, destination, token_idx)?;
        }
        outcome.messages.push(OutgoingMessage {
            destination: chain_id.into(),
            authenticated: true,
            is_tracked: false,
            resources: Resources::default(),
            message,
        });
        Ok(())
    }

    /// Checks that the order exists and has been issued by the claimed owner.
    async fn check_order_id(
        &self,
        order_id: &OrderId,
        owner: &AccountOwner,
    ) -> Result<(), MatchingEngineError> {
        let value = self.orders.get(order_id).await?;
        match value {
            None => Err(MatchingEngineError::OrderNotPresent),
            Some(value) => {
                if &value.owner != owner {
                    return Err(MatchingEngineError::WrongOwnerOfOrder);
                }
                Ok(())
            }
        }
    }

    /// This function does three things:
    /// * check the order ownership
    /// * modify the order
    /// * Send the corresponding transfers
    async fn modify_order_check(
        &mut self,
        order_id: OrderId,
        cancel_amount: ModifyAmount,
        owner: &AccountOwner,
    ) -> Result<(), MatchingEngineError> {
        self.check_order_id(&order_id, owner).await?;
        let transfer = self.modify_order(order_id, cancel_amount).await?;
        self.send_to(transfer)
    }

    /// Orders which have length 0 should be removed from the system.
    /// It is possible that we have some zero orders in the QueueView
    /// under the condition that they are not the oldest.
    /// An order can be of size zero for two reasons:
    /// * It has been totally cancelled
    /// * It has been filled that is the owner got what they wanted.
    async fn remove_zero_orders_from_level(
        view: &mut LevelView,
    ) -> Result<(), MatchingEngineError> {
        // If some order has amount zero but is after an order of non-zero amount, then it is left.
        let iter = view.queue.iter_mut().await?;
        let n_remove = iter
            .take_while(|order| order.amount == Amount::ZERO)
            .count();
        for _ in 0..n_remove {
            view.queue.delete_front();
        }
        Ok(())
    }

    /// For a specific level of price, look at all the orders and find the one that
    /// has this specific order_id.
    /// When that order is found, then the cancellation is applied to it.
    /// Then the information is emitted for the handling of this operation.
    async fn modify_order_level(
        view: &mut LevelView,
        order_id: OrderId,
        cancel_amount: ModifyAmount,
    ) -> Result<(Amount, bool), MatchingEngineError> {
        let mut iter = view.queue.iter_mut().await?;
        let state_order = iter
            .find(|order| order.order_id == order_id)
            .ok_or(MatchingEngineError::OrderNotPresent)?;
        let new_amount = match cancel_amount {
            ModifyAmount::All => Amount::ZERO,
            ModifyAmount::Partial(cancel_amount) => {
                if cancel_amount > state_order.amount {
                    return Err(MatchingEngineError::TooLargeModifyOrder);
                }
                state_order.amount.try_sub(cancel_amount).unwrap()
            }
        };
        let corr_cancel_amount = state_order.amount.try_sub(new_amount).unwrap();
        state_order.amount = new_amount;
        Self::remove_zero_orders_from_level(view).await?;
        Ok((corr_cancel_amount, new_amount == Amount::ZERO))
    }

    /// Modification of the order from the order_id.
    /// This means that some transfers have to be done and the size depends
    /// whether ask or bid.
    async fn modify_order(
        &mut self,
        order_id: OrderId,
        cancel_amount: ModifyAmount,
    ) -> Result<Transfer, MatchingEngineError> {
        let key_book = self.orders.get(&order_id).await?;
        if let Some(key_book) = key_book {
            match key_book.nature {
                OrderNature::Bid => {
                    let view = self.bids.load_entry_mut(&key_book.price.to_bid()).await?;
                    let (cancel_amount, remove_order_id) =
                        Self::modify_order_level(view, order_id, cancel_amount).await?;
                    if remove_order_id {
                        self.remove_order_id((key_book.owner, order_id)).await?;
                    }
                    let cancel_amount0 = product_price_amount(key_book.price, cancel_amount);
                    let transfer = Transfer {
                        owner: key_book.owner,
                        amount: cancel_amount0,
                        token_idx: 0,
                    };
                    Ok(transfer)
                }
                OrderNature::Ask => {
                    let view = self.asks.load_entry_mut(&key_book.price.to_ask()).await?;
                    let (cancel_count, remove_order_id) =
                        Self::modify_order_level(view, order_id, cancel_amount).await?;
                    if remove_order_id {
                        self.remove_order_id((key_book.owner, order_id)).await?;
                    }
                    let transfer = Transfer {
                        owner: key_book.owner,
                        amount: cancel_count,
                        token_idx: 1,
                    };
                    Ok(transfer)
                }
            }
        } else {
            Err(MatchingEngineError::OrderNotPresent)
        }
    }

    /// Get the order_id that increases starting from 0.
    fn get_new_order_id(&mut self) -> Result<OrderId, MatchingEngineError> {
        let value = self.next_order_number.get_mut();
        let value_ret = *value;
        *value += 1;
        Ok(value_ret)
    }

    /// Creates the transfers corresponding to the order:
    ///
    /// * `nature` is the nature of the order in question.
    /// * `fill` is the amount that is being processed.
    /// * `owner` is the owner of the new order being inserted.
    /// * `order_level` is the liquidity providing order.
    /// * `price_level` is the price of the existing order that provides liquidity.
    /// * `price_insert` is the price that of the newly added order.
    ///
    /// If the new order satisfies bid > best_ask or ask < best_bid
    /// then there is money on the table. There are three possible
    /// ways to handle this:
    ///
    /// * The delta gets to the owner of the matching engine.
    /// * The liquidity providing order gets the delta.
    /// * The liquidity eating order gets the delta.
    ///
    /// We choose the second scenario since the liquidity providing
    /// order is waiting and so deserves to be rewarded for the wait.
    fn get_transfers(
        nature: &OrderNature,
        fill: Amount,
        owner: &AccountOwner,
        order_level: &OrderEntry,
        price_level: Price,  // the price that was present in the level
        price_insert: Price, // the price of the inserted order
    ) -> Vec<Transfer> {
        let mut transfers = Vec::new();
        match nature {
            OrderNature::Bid => {
                // The order offers to buy token1 at price price_insert
                // * When the old order was created fill of token1 were commited
                //   by the seller.
                // * When the new order is created price_insert * fill of token0
                //   were committed by the buyer.
                // The result is that
                // * price_insert * fill of token0 go to the seller (more than he expected)
                // * fill of token1 go to the buyer.
                assert!(price_insert >= price_level);
                let transfer_to_buyer = Transfer {
                    owner: *owner,
                    amount: fill,
                    token_idx: 1,
                };
                let fill0 = product_price_amount(price_insert, fill);
                let transfer_to_seller = Transfer {
                    owner: order_level.owner,
                    amount: fill0,
                    token_idx: 0,
                };
                transfers.push(transfer_to_buyer);
                transfers.push(transfer_to_seller);
            }
            OrderNature::Ask => {
                // The order offers to sell token1 at price price_insert
                // * When the old order was created, price_level * fill of token0
                //   had to be commited by the buyer.
                // * When the new order is created, fill of token1 have to
                //   be commited by the seller.
                // The result is that
                // * price_insert * fill have to be sent to the seller
                // * the buyer receives
                //   - fill of token1
                //   - (price_level - price_insert) fill of token0 (nice bonus)
                assert!(price_insert <= price_level);
                let fill0 = product_price_amount(price_insert, fill);
                let transfer_to_seller = Transfer {
                    owner: *owner,
                    amount: fill0,
                    token_idx: 0,
                };
                let transfer_to_buyer1 = Transfer {
                    owner: order_level.owner,
                    amount: fill,
                    token_idx: 1,
                };
                transfers.push(transfer_to_buyer1);
                transfers.push(transfer_to_seller);
                if price_level != price_insert {
                    let price_diff = Price {
                        price: price_level.price - price_insert.price,
                    };
                    let fill0 = product_price_amount(price_diff, fill);
                    let transfer_to_buyer0 = Transfer {
                        owner: order_level.owner,
                        amount: fill0,
                        token_idx: 0,
                    };
                    transfers.push(transfer_to_buyer0);
                }
            }
        }
        transfers
    }

    /// A price level is cleared starting from the oldest one till the
    /// new order is completely filled or there is no more liquidity
    /// providing order remaining to fill it.
    async fn level_clearing(
        view: &mut LevelView,
        owner: &AccountOwner,
        amount: &mut Amount,
        transfers: &mut Vec<Transfer>,
        nature: &OrderNature,
        price_level: Price,
        price_insert: Price,
    ) -> Result<Vec<(AccountOwner, OrderId)>, MatchingEngineError> {
        let mut remove_order = Vec::new();
        for order in view.queue.iter_mut().await? {
            let fill = min(order.amount, *amount);
            amount.try_sub_assign(fill).unwrap();
            order.amount.try_sub_assign(fill).unwrap();
            if fill > Amount::ZERO {
                transfers.extend_from_slice(&Self::get_transfers(
                    nature,
                    fill,
                    owner,
                    order,
                    price_level,
                    price_insert,
                ));
            }
            if order.amount == Amount::ZERO {
                remove_order.push((order.owner, order.order_id));
            }
            if *amount == Amount::ZERO {
                break;
            }
        }
        Self::remove_zero_orders_from_level(view).await?;
        Ok(remove_order)
    }

    /// Insert the order_id and insert it into:
    /// * account_info which give the orders by owner
    /// * The orders which contain the symbolic information and the key_book.
    async fn insert_order(
        &mut self,
        owner: AccountOwner,
        nature: OrderNature,
        order_id: OrderId,
        price: Price,
    ) -> Result<(), MatchingEngineError> {
        let account_info = self.account_info.get_mut_or_default(&owner).await?;
        account_info.orders.insert(order_id);
        let key_book = KeyBook {
            price,
            nature,
            owner,
        };
        self.orders.insert(&order_id, key_book)?;
        Ok(())
    }

    /// Remove one single (owner, order_id) from the database
    /// * This is done for the info by owners
    /// * And the symbolic information of orders
    async fn remove_order_id(
        &mut self,
        entry: (AccountOwner, OrderId),
    ) -> Result<(), MatchingEngineError> {
        let (owner, order_id) = entry;
        let account_info = self
            .account_info
            .get_mut(&owner)
            .await
            .expect("account_info")
            .unwrap();
        account_info.orders.remove(&order_id);
        Ok(())
    }

    /// Removing a bunch of order_id
    async fn remove_order_ids(
        &mut self,
        entries: Vec<(AccountOwner, OrderId)>,
    ) -> Result<(), MatchingEngineError> {
        for entry in entries {
            self.remove_order_id(entry).await?;
        }
        Ok(())
    }

    /// We insert an order into the matching engine and this creates several things:
    /// * The price levels that matches are selected
    /// * Getting from the best matching price to the least good the price levels
    ///   are cleared.
    /// * That clearing creates a number of transfer orders.
    /// * If after the level clearing the order is completely filled then it it not
    ///   inserted. Otherwise, it became a liquidity order in the matching engine
    async fn insert_and_uncross_market(
        &mut self,
        owner: &AccountOwner,
        amount: Amount,
        nature: OrderNature,
        price: &Price,
    ) -> Result<Vec<Transfer>, MatchingEngineError> {
        // Bids are ordered from the highest bid (most preferable) to the smallest bid.
        // Asks are ordered from the smallest (most preferable) to the highest.
        // The prices have custom serialization so that they are in increasing order.
        // To reverse the order of the bids, we take the bitwise complement of the price.
        let order_id = self.get_new_order_id()?;
        let mut final_amount = amount;
        let mut transfers = Vec::new();
        match nature {
            OrderNature::Bid => {
                let mut matching_price_asks = Vec::new();
                self.asks
                    .for_each_index_while(|price_ask| {
                        let matches = price_ask.to_price() <= *price;
                        if matches {
                            matching_price_asks.push(price_ask);
                        }
                        Ok(matches)
                    })
                    .await?;
                for price_ask in matching_price_asks {
                    let view = self.asks.load_entry_mut(&price_ask).await?;
                    let remove_entry = Self::level_clearing(
                        view,
                        owner,
                        &mut final_amount,
                        &mut transfers,
                        &nature,
                        price_ask.to_price(),
                        *price,
                    )
                    .await?;
                    if view.queue.count() == 0 {
                        self.asks.remove_entry(&price_ask)?;
                    }
                    self.remove_order_ids(remove_entry).await?;
                    if final_amount == Amount::ZERO {
                        break;
                    }
                }
                if final_amount != Amount::ZERO {
                    let view = self.bids.load_entry_mut(&price.to_bid()).await?;
                    let order = OrderEntry {
                        amount: final_amount,
                        owner: *owner,
                        order_id,
                    };
                    view.queue.push_back(order);
                    self.insert_order(*owner, OrderNature::Bid, order_id, *price)
                        .await?;
                }
            }
            OrderNature::Ask => {
                let mut matching_price_bids = Vec::new();
                self.bids
                    .for_each_index_while(|price_bid| {
                        let matches = price_bid.to_price() >= *price;
                        if matches {
                            matching_price_bids.push(price_bid);
                        }
                        Ok(matches)
                    })
                    .await?;
                for price_bid in matching_price_bids {
                    let view = self.bids.load_entry_mut(&price_bid).await?;
                    let remove_entry = Self::level_clearing(
                        view,
                        owner,
                        &mut final_amount,
                        &mut transfers,
                        &nature,
                        price_bid.to_price(),
                        *price,
                    )
                    .await?;
                    if view.queue.count() == 0 {
                        self.bids.remove_entry(&price_bid)?;
                    }
                    self.remove_order_ids(remove_entry).await?;
                    if final_amount == Amount::ZERO {
                        break;
                    }
                }
                if final_amount != Amount::ZERO {
                    let view = self.asks.load_entry_mut(&price.to_ask()).await?;
                    let order = OrderEntry {
                        amount: final_amount,
                        owner: *owner,
                        order_id,
                    };
                    view.queue.push_back(order);
                    self.insert_order(*owner, OrderNature::Ask, order_id, *price)
                        .await?;
                }
            }
        }
        Ok(transfers)
    }
}
