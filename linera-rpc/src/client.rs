// Copyright (c) Zefchain Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;

use crate::{grpc_network::GrpcClient, simple_network::SimpleClient};
use linera_base::identifiers::ChainId;
use linera_chain::data_types::{BlockProposal, Certificate, HashedValue, LiteCertificate};
use linera_core::{
    data_types::{ChainInfoQuery, ChainInfoResponse},
    node::{CrossChainMessageDelivery, NodeError, NotificationStream, ValidatorNode},
};

#[derive(Clone)]
pub enum Client {
    Grpc(GrpcClient),
    Simple(SimpleClient),
}

impl From<GrpcClient> for Client {
    fn from(client: GrpcClient) -> Self {
        Self::Grpc(client)
    }
}

impl From<SimpleClient> for Client {
    fn from(client: SimpleClient) -> Self {
        Self::Simple(client)
    }
}

#[async_trait]
impl ValidatorNode for Client {
    async fn handle_block_proposal(
        &mut self,
        proposal: BlockProposal,
    ) -> Result<ChainInfoResponse, NodeError> {
        match self {
            Client::Grpc(grpc_client) => grpc_client.handle_block_proposal(proposal).await,
            Client::Simple(simple_client) => simple_client.handle_block_proposal(proposal).await,
        }
    }

    async fn handle_lite_certificate(
        &mut self,
        certificate: LiteCertificate<'_>,
        delivery: CrossChainMessageDelivery,
    ) -> Result<ChainInfoResponse, NodeError> {
        match self {
            Client::Grpc(grpc_client) => {
                grpc_client
                    .handle_lite_certificate(certificate, delivery)
                    .await
            }
            Client::Simple(simple_client) => {
                simple_client
                    .handle_lite_certificate(certificate, delivery)
                    .await
            }
        }
    }

    async fn handle_certificate(
        &mut self,
        certificate: Certificate,
        blobs: Vec<HashedValue>,
        delivery: CrossChainMessageDelivery,
    ) -> Result<ChainInfoResponse, NodeError> {
        match self {
            Client::Grpc(grpc_client) => {
                grpc_client
                    .handle_certificate(certificate, blobs, delivery)
                    .await
            }
            Client::Simple(simple_client) => {
                simple_client
                    .handle_certificate(certificate, blobs, delivery)
                    .await
            }
        }
    }

    async fn handle_chain_info_query(
        &mut self,
        query: ChainInfoQuery,
    ) -> Result<ChainInfoResponse, NodeError> {
        match self {
            Client::Grpc(grpc_client) => grpc_client.handle_chain_info_query(query).await,
            Client::Simple(simple_client) => simple_client.handle_chain_info_query(query).await,
        }
    }

    async fn subscribe(&mut self, chains: Vec<ChainId>) -> Result<NotificationStream, NodeError> {
        Ok(match self {
            Client::Grpc(grpc_client) => Box::pin(grpc_client.subscribe(chains).await?),
            Client::Simple(simple_client) => Box::pin(simple_client.subscribe(chains).await?),
        })
    }

    async fn get_version_info(&mut self) -> Result<linera_version::VersionInfo, NodeError> {
        Ok(match self {
            Client::Grpc(grpc_client) => grpc_client.get_version_info().await?,
            Client::Simple(simple_client) => simple_client.get_version_info().await?,
        })
    }
}
