// Copyright (c) Zefchain Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Handle requests from the synchronous execution thread of user applications.

use crate::{
    system::{ApplicationPermissions, OpenChainConfig, Recipient, UserData},
    util::RespondExt,
    ExecutionError, ExecutionRuntimeContext, ExecutionStateView, RawExecutionOutcome,
    RawOutgoingMessage, SystemExecutionError, SystemMessage, UserApplicationDescription,
    UserApplicationId, UserContractCode, UserServiceCode,
};
use futures::channel::mpsc;
use linera_base::{
    data_types::{Amount, Timestamp},
    identifiers::{Account, MessageId, Owner},
    ownership::ChainOwnership,
};

#[cfg(with_metrics)]
use linera_base::{
    prometheus_util::{self, MeasureLatency as _},
    sync::Lazy,
};

use linera_views::{
    batch::Batch,
    common::Context,
    views::{View, ViewError},
};
use oneshot::Sender;
#[cfg(with_metrics)]
use prometheus::HistogramVec;
use std::fmt::{self, Debug, Formatter};

#[cfg(with_metrics)]
/// Histogram of the latency to load a contract bytecode.
static LOAD_CONTRACT_LATENCY: Lazy<HistogramVec> = Lazy::new(|| {
    prometheus_util::register_histogram_vec(
        "load_contract_latency",
        "Load contract latency",
        &[],
        Some(vec![
            0.001, 0.002_5, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 25.0,
            100.0, 250.0,
        ]),
    )
    .expect("Histogram creation should not fail")
});

#[cfg(with_metrics)]
/// Histogram of the latency to load a service bytecode.
static LOAD_SERVICE_LATENCY: Lazy<HistogramVec> = Lazy::new(|| {
    prometheus_util::register_histogram_vec(
        "load_service_latency",
        "Load service latency",
        &[],
        Some(vec![
            0.001, 0.002_5, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 25.0,
            100.0, 250.0,
        ]),
    )
    .expect("Histogram creation should not fail")
});

pub(crate) type ExecutionStateSender = mpsc::UnboundedSender<Request>;

impl<C> ExecutionStateView<C>
where
    C: Context + Clone + Send + Sync + 'static,
    ViewError: From<C::Error>,
    C::Extra: ExecutionRuntimeContext,
{
    // TODO(#1416): Support concurrent I/O.
    pub(crate) async fn handle_request(&mut self, request: Request) -> Result<(), ExecutionError> {
        use Request::*;
        match request {
            LoadContract { id, callback } => {
                #[cfg(with_metrics)]
                let _latency = LOAD_CONTRACT_LATENCY.measure_latency();
                let description = self.system.registry.describe_application(id).await?;
                let code = self
                    .context()
                    .extra()
                    .get_user_contract(&description)
                    .await?;
                callback.respond((code, description));
            }

            LoadService { id, callback } => {
                #[cfg(with_metrics)]
                let _latency = LOAD_SERVICE_LATENCY.measure_latency();
                let description = self.system.registry.describe_application(id).await?;
                let code = self
                    .context()
                    .extra()
                    .get_user_service(&description)
                    .await?;
                callback.respond((code, description));
            }

            ChainBalance { callback } => {
                let balance = *self.system.balance.get();
                callback.respond(balance);
            }

            OwnerBalance { owner, callback } => {
                let balance = self.system.balances.get(&owner).await?.unwrap_or_default();
                callback.respond(balance);
            }

            Transfer {
                source,
                destination,
                amount,
                signer,
                callback,
            } => {
                let mut execution_outcome = RawExecutionOutcome::default();
                let message = self
                    .system
                    .transfer(signer, source, Recipient::Account(destination), amount)
                    .await?;

                if let Some(message) = message {
                    execution_outcome.messages.push(message);
                }
                callback.respond(execution_outcome);
            }

            Claim {
                source,
                destination,
                amount,
                signer,
                callback,
            } => {
                let owner = source.owner.ok_or(ExecutionError::OwnerIsNone)?;
                let mut execution_outcome = RawExecutionOutcome::default();
                let message = self
                    .system
                    .claim(
                        signer,
                        owner,
                        source.chain_id,
                        Recipient::Account(destination),
                        amount,
                        UserData::default(),
                    )
                    .await?;

                execution_outcome.messages.push(message);
                callback.respond(execution_outcome);
            }

            SystemTimestamp { callback } => {
                let timestamp = *self.system.timestamp.get();
                callback.respond(timestamp);
            }

            ChainOwnership { callback } => {
                let ownership = self.system.ownership.get().clone();
                callback.respond(ownership);
            }

            ContainsKey { id, key, callback } => {
                let view = self.users.try_load_entry_or_insert(&id).await?;
                let result = view.contains_key(&key).await?;
                callback.respond(result);
            }

            ReadMultiValuesBytes { id, keys, callback } => {
                let view = self.users.try_load_entry_or_insert(&id).await?;
                let values = view.multi_get(keys).await?;
                callback.respond(values);
            }

            ReadValueBytes { id, key, callback } => {
                let view = self.users.try_load_entry_or_insert(&id).await?;
                let result = view.get(&key).await?;
                callback.respond(result);
            }

            FindKeysByPrefix {
                id,
                key_prefix,
                callback,
            } => {
                let view = self.users.try_load_entry_or_insert(&id).await?;
                let result = view.find_keys_by_prefix(&key_prefix).await?;
                callback.respond(result);
            }

            FindKeyValuesByPrefix {
                id,
                key_prefix,
                callback,
            } => {
                let view = self.users.try_load_entry_or_insert(&id).await?;
                let result = view.find_key_values_by_prefix(&key_prefix).await?;
                callback.respond(result);
            }

            WriteBatch {
                id,
                batch,
                callback,
            } => {
                let mut view = self.users.try_load_entry_mut(&id).await?;
                view.write_batch(batch).await?;
                callback.respond(());
            }

            OpenChain {
                ownership,
                balance,
                next_message_id,
                application_permissions,
                callback,
            } => {
                let inactive_err = || SystemExecutionError::InactiveChain;
                let config = OpenChainConfig {
                    ownership,
                    admin_id: self.system.admin_id.get().ok_or_else(inactive_err)?,
                    epoch: self.system.epoch.get().ok_or_else(inactive_err)?,
                    committees: self.system.committees.get().clone(),
                    balance,
                    application_permissions,
                };
                let messages = self.system.open_chain(config, next_message_id)?;
                callback.respond(messages)
            }

            CloseChain {
                application_id,
                callback,
            } => {
                let app_permissions = self.system.application_permissions.get();
                if !app_permissions.can_close_chain(&application_id) {
                    callback.respond(Err(ExecutionError::UnauthorizedApplication(application_id)));
                } else {
                    let chain_id = self.context().extra().chain_id();
                    self.system.close_chain(chain_id).await?;
                    callback.respond(Ok(()));
                }
            }
        }

        Ok(())
    }
}

/// Requests to the execution state.
pub enum Request {
    LoadContract {
        id: UserApplicationId,
        callback: Sender<(UserContractCode, UserApplicationDescription)>,
    },

    LoadService {
        id: UserApplicationId,
        callback: Sender<(UserServiceCode, UserApplicationDescription)>,
    },

    ChainBalance {
        callback: Sender<Amount>,
    },

    OwnerBalance {
        owner: Owner,
        callback: Sender<Amount>,
    },

    Transfer {
        source: Option<Owner>,
        destination: Account,
        amount: Amount,
        signer: Option<Owner>,
        callback: Sender<RawExecutionOutcome<SystemMessage, Amount>>,
    },

    Claim {
        source: Account,
        destination: Account,
        amount: Amount,
        signer: Option<Owner>,
        callback: Sender<RawExecutionOutcome<SystemMessage, Amount>>,
    },

    SystemTimestamp {
        callback: Sender<Timestamp>,
    },

    ChainOwnership {
        callback: Sender<ChainOwnership>,
    },

    ReadValueBytes {
        id: UserApplicationId,
        key: Vec<u8>,
        callback: Sender<Option<Vec<u8>>>,
    },

    ContainsKey {
        id: UserApplicationId,
        key: Vec<u8>,
        callback: Sender<bool>,
    },

    ReadMultiValuesBytes {
        id: UserApplicationId,
        keys: Vec<Vec<u8>>,
        callback: Sender<Vec<Option<Vec<u8>>>>,
    },

    FindKeysByPrefix {
        id: UserApplicationId,
        key_prefix: Vec<u8>,
        callback: Sender<Vec<Vec<u8>>>,
    },

    FindKeyValuesByPrefix {
        id: UserApplicationId,
        key_prefix: Vec<u8>,
        callback: Sender<Vec<(Vec<u8>, Vec<u8>)>>,
    },

    WriteBatch {
        id: UserApplicationId,
        batch: Batch,
        callback: Sender<()>,
    },

    OpenChain {
        ownership: ChainOwnership,
        balance: Amount,
        next_message_id: MessageId,
        application_permissions: ApplicationPermissions,
        callback: Sender<[RawOutgoingMessage<SystemMessage, Amount>; 2]>,
    },

    CloseChain {
        application_id: UserApplicationId,
        callback: oneshot::Sender<Result<(), ExecutionError>>,
    },
}

impl Debug for Request {
    fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
        match self {
            Request::LoadContract { id, .. } => formatter
                .debug_struct("Request::LoadContract")
                .field("id", id)
                .finish_non_exhaustive(),

            Request::LoadService { id, .. } => formatter
                .debug_struct("Request::LoadService")
                .field("id", id)
                .finish_non_exhaustive(),

            Request::ChainBalance { .. } => formatter
                .debug_struct("Request::ChainBalance")
                .finish_non_exhaustive(),

            Request::OwnerBalance { owner, .. } => formatter
                .debug_struct("Request::OwnerBalance")
                .field("owner", owner)
                .finish_non_exhaustive(),

            Request::Transfer {
                source,
                destination,
                amount,
                signer,
                ..
            } => formatter
                .debug_struct("Request::Transfer")
                .field("source", source)
                .field("destination", destination)
                .field("amount", amount)
                .field("signer", signer)
                .finish_non_exhaustive(),

            Request::Claim {
                source,
                destination,
                amount,
                signer,
                ..
            } => formatter
                .debug_struct("Request::Claim")
                .field("source", source)
                .field("destination", destination)
                .field("amount", amount)
                .field("signer", signer)
                .finish_non_exhaustive(),

            Request::SystemTimestamp { .. } => formatter
                .debug_struct("Request::SystemTimestamp")
                .finish_non_exhaustive(),

            Request::ChainOwnership { .. } => formatter
                .debug_struct("Request::ChainOwnership")
                .finish_non_exhaustive(),

            Request::ReadValueBytes { id, key, .. } => formatter
                .debug_struct("Request::ReadValueBytes")
                .field("id", id)
                .field("key", key)
                .finish_non_exhaustive(),

            Request::ContainsKey { id, key, .. } => formatter
                .debug_struct("Request::ContainsKey")
                .field("id", id)
                .field("key", key)
                .finish_non_exhaustive(),

            Request::ReadMultiValuesBytes { id, keys, .. } => formatter
                .debug_struct("Request::ReadMultiValuesBytes")
                .field("id", id)
                .field("keys", keys)
                .finish_non_exhaustive(),

            Request::FindKeysByPrefix { id, key_prefix, .. } => formatter
                .debug_struct("Request::FindKeysByPrefix")
                .field("id", id)
                .field("key_prefix", key_prefix)
                .finish_non_exhaustive(),

            Request::FindKeyValuesByPrefix { id, key_prefix, .. } => formatter
                .debug_struct("Request::FindKeyValuesByPrefix")
                .field("id", id)
                .field("key_prefix", key_prefix)
                .finish_non_exhaustive(),

            Request::WriteBatch { id, batch, .. } => formatter
                .debug_struct("Request::WriteBatch")
                .field("id", id)
                .field("batch", batch)
                .finish_non_exhaustive(),

            Request::OpenChain { balance, .. } => formatter
                .debug_struct("Request::OpenChain")
                .field("balance", balance)
                .finish_non_exhaustive(),

            Request::CloseChain { application_id, .. } => formatter
                .debug_struct("Request::CloseChain")
                .field("application_id", application_id)
                .finish_non_exhaustive(),
        }
    }
}
