// Copyright (c) Zefchain Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Conversions from types declared in [`linera-sdk`] to types generated by
//! [`wit-bindgen-guest-rust`].

use super::{contract_system_api as wit_system_api, wit_types};
use crate::{ApplicationCallOutcome, ExecutionOutcome, OutgoingMessage, SessionCallOutcome};
use linera_base::{
    crypto::CryptoHash,
    data_types::{Amount, Resources},
    identifiers::{Account, ApplicationId, ChannelName, Destination, MessageId, Owner, SessionId},
};

impl From<CryptoHash> for wit_system_api::CryptoHash {
    fn from(hash_value: CryptoHash) -> Self {
        let parts = <[u64; 4]>::from(hash_value);

        wit_system_api::CryptoHash {
            part1: parts[0],
            part2: parts[1],
            part3: parts[2],
            part4: parts[3],
        }
    }
}

impl From<Owner> for wit_system_api::CryptoHash {
    fn from(owner: Owner) -> Self {
        wit_system_api::CryptoHash::from(owner.0)
    }
}

impl From<Amount> for wit_system_api::Amount {
    fn from(host: Amount) -> Self {
        wit_system_api::Amount {
            lower_half: host.lower_half(),
            upper_half: host.upper_half(),
        }
    }
}

impl From<CryptoHash> for wit_types::CryptoHash {
    fn from(crypto_hash: CryptoHash) -> Self {
        let parts = <[u64; 4]>::from(crypto_hash);

        wit_types::CryptoHash {
            part1: parts[0],
            part2: parts[1],
            part3: parts[2],
            part4: parts[3],
        }
    }
}

impl From<Account> for wit_system_api::Account {
    fn from(account: Account) -> Self {
        wit_system_api::Account {
            chain_id: account.chain_id.0.into(),
            owner: account.owner.map(|owner| owner.into()),
        }
    }
}

impl From<ApplicationId> for wit_system_api::ApplicationId {
    fn from(application_id: ApplicationId) -> wit_system_api::ApplicationId {
        wit_system_api::ApplicationId {
            bytecode_id: application_id.bytecode_id.message_id.into(),
            creation: application_id.creation.into(),
        }
    }
}

impl From<SessionId> for wit_system_api::SessionId {
    fn from(session_id: SessionId) -> Self {
        wit_system_api::SessionId {
            application_id: session_id.application_id.into(),
            index: session_id.index,
        }
    }
}

impl From<MessageId> for wit_system_api::MessageId {
    fn from(message_id: MessageId) -> Self {
        wit_system_api::MessageId {
            chain_id: message_id.chain_id.0.into(),
            height: message_id.height.0,
            index: message_id.index,
        }
    }
}

impl From<log::Level> for wit_system_api::LogLevel {
    fn from(level: log::Level) -> Self {
        match level {
            log::Level::Trace => wit_system_api::LogLevel::Trace,
            log::Level::Debug => wit_system_api::LogLevel::Debug,
            log::Level::Info => wit_system_api::LogLevel::Info,
            log::Level::Warn => wit_system_api::LogLevel::Warn,
            log::Level::Error => wit_system_api::LogLevel::Error,
        }
    }
}

impl From<ApplicationCallOutcome<Vec<u8>, Vec<u8>, Vec<u8>>> for wit_types::ApplicationCallOutcome {
    fn from(outcome: ApplicationCallOutcome<Vec<u8>, Vec<u8>, Vec<u8>>) -> Self {
        wit_types::ApplicationCallOutcome {
            value: outcome.value,
            execution_outcome: outcome.execution_outcome.into(),
            create_sessions: outcome.create_sessions,
        }
    }
}

impl From<SessionCallOutcome<Vec<u8>, Vec<u8>, Vec<u8>>> for wit_types::SessionCallOutcome {
    fn from(outcome: SessionCallOutcome<Vec<u8>, Vec<u8>, Vec<u8>>) -> Self {
        wit_types::SessionCallOutcome {
            inner: outcome.inner.into(),
            new_state: outcome.new_state,
        }
    }
}

impl From<OutgoingMessage<Vec<u8>>> for wit_types::OutgoingMessage {
    fn from(message: OutgoingMessage<Vec<u8>>) -> Self {
        Self {
            destination: message.destination.into(),
            authenticated: message.authenticated,
            is_tracked: message.is_tracked,
            resources: message.resources.into(),
            message: message.message,
        }
    }
}

impl From<Resources> for wit_types::Resources {
    fn from(resources: Resources) -> Self {
        wit_types::Resources {
            fuel: resources.fuel,
            read_operations: resources.read_operations,
            write_operations: resources.write_operations,
            bytes_to_read: resources.bytes_to_read,
            bytes_to_write: resources.bytes_to_write,
            messages: resources.messages,
            message_size: resources.message_size,
            storage_size_delta: resources.storage_size_delta,
        }
    }
}

impl From<ExecutionOutcome<Vec<u8>>> for wit_types::ExecutionOutcome {
    fn from(outcome: ExecutionOutcome<Vec<u8>>) -> Self {
        let messages = outcome
            .messages
            .into_iter()
            .map(wit_types::OutgoingMessage::from)
            .collect();

        let subscribe = outcome
            .subscribe
            .into_iter()
            .map(|(subscription, chain_id)| (subscription.into(), chain_id.0.into()))
            .collect();

        let unsubscribe = outcome
            .unsubscribe
            .into_iter()
            .map(|(subscription, chain_id)| (subscription.into(), chain_id.0.into()))
            .collect();

        wit_types::ExecutionOutcome {
            messages,
            subscribe,
            unsubscribe,
        }
    }
}

impl From<Destination> for wit_types::Destination {
    fn from(destination: Destination) -> Self {
        match destination {
            Destination::Recipient(chain_id) => {
                wit_types::Destination::Recipient(chain_id.0.into())
            }
            Destination::Subscribers(subscription) => {
                wit_types::Destination::Subscribers(subscription.into())
            }
        }
    }
}

impl From<ChannelName> for wit_types::ChannelName {
    fn from(name: ChannelName) -> Self {
        wit_types::ChannelName {
            name: name.into_bytes(),
        }
    }
}
