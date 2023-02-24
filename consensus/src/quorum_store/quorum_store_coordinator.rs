// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{
    quorum_store::{
        batch_coordinator::BatchCoordinatorCommand, batch_generator::BatchGeneratorCommand,
        batch_store::BatchStoreCommand, proof_coordinator::ProofCoordinatorCommand,
        proof_manager::ProofManagerCommand,
    },
    round_manager::VerifiedEvent,
};
use aptos_channels::aptos_channel;
use aptos_consensus_types::proof_of_store::LogicalTime;
use aptos_crypto::HashValue;
use aptos_logger::prelude::*;
use aptos_types::{account_address::AccountAddress, PeerId};
use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

pub enum CoordinatorCommand {
    CommitNotification(LogicalTime, Vec<HashValue>),
    Shutdown(futures_channel::oneshot::Sender<()>),
}

pub struct QuorumStoreCoordinator {
    my_peer_id: PeerId,
    batch_generator_cmd_tx: mpsc::Sender<BatchGeneratorCommand>,
    batch_coordinator_cmd_tx: mpsc::Sender<BatchCoordinatorCommand>,
    remote_batch_coordinator_cmd_tx: Vec<mpsc::Sender<BatchCoordinatorCommand>>,
    proof_coordinator_cmd_tx: mpsc::Sender<ProofCoordinatorCommand>,
    proof_manager_cmd_tx: mpsc::Sender<ProofManagerCommand>,
    batch_store_cmd_tx: mpsc::Sender<BatchStoreCommand>,
    quorum_store_msg_tx: aptos_channel::Sender<AccountAddress, VerifiedEvent>,
}

impl QuorumStoreCoordinator {
    pub(crate) fn new(
        my_peer_id: PeerId,
        batch_generator_cmd_tx: mpsc::Sender<BatchGeneratorCommand>,
        batch_coordinator_cmd_tx: mpsc::Sender<BatchCoordinatorCommand>,
        remote_batch_coordinator_cmd_tx: Vec<mpsc::Sender<BatchCoordinatorCommand>>,
        proof_coordinator_cmd_tx: mpsc::Sender<ProofCoordinatorCommand>,
        proof_manager_cmd_tx: mpsc::Sender<ProofManagerCommand>,
        batch_store_cmd_tx: mpsc::Sender<BatchStoreCommand>,
        quorum_store_msg_tx: aptos_channel::Sender<AccountAddress, VerifiedEvent>,
    ) -> Self {
        Self {
            my_peer_id,
            batch_generator_cmd_tx,
            batch_coordinator_cmd_tx,
            remote_batch_coordinator_cmd_tx,
            proof_coordinator_cmd_tx,
            proof_manager_cmd_tx,
            batch_store_cmd_tx,
            quorum_store_msg_tx,
        }
    }

    pub async fn start(self, mut rx: futures_channel::mpsc::Receiver<CoordinatorCommand>) {
        while let Some(cmd) = rx.next().await {
            match cmd {
                CoordinatorCommand::CommitNotification(logical_time, digests) => {
                    self.proof_manager_cmd_tx
                        .send(ProofManagerCommand::CommitNotification(
                            logical_time,
                            digests,
                        ))
                        .await
                        .expect("Failed to send to ProofManager");
                    // TODO: need a callback or not?

                    self.batch_generator_cmd_tx
                        .send(BatchGeneratorCommand::CommitNotification(logical_time))
                        .await
                        .expect("Failed to send to BatchGenerator");
                },
                CoordinatorCommand::Shutdown(ack_tx) => {
                    // Note: Shutdown is done from the back of the quorum store pipeline to the
                    // front, so senders are always shutdown before receivers. This avoids sending
                    // messages through closed channels during shutdown.
                    // Oneshots that send data in the reverse order of the pipeline must assume that
                    // the receiver could be unavailable during shutdown, and resolve this without
                    // panicking.

                    let (network_listener_shutdown_tx, network_listener_shutdown_rx) =
                        oneshot::channel();
                    match self.quorum_store_msg_tx.push(
                        self.my_peer_id,
                        VerifiedEvent::Shutdown(network_listener_shutdown_tx),
                    ) {
                        Ok(()) => debug!("QS: shutdown network listener sent"),
                        Err(err) => panic!("Failed to send to NetworkListener, Err {:?}", err),
                    };
                    network_listener_shutdown_rx
                        .await
                        .expect("Failed to stop NetworkListener");

                    let (batch_generator_shutdown_tx, batch_generator_shutdown_rx) =
                        oneshot::channel();
                    self.batch_generator_cmd_tx
                        .send(BatchGeneratorCommand::Shutdown(batch_generator_shutdown_tx))
                        .await
                        .expect("Failed to send to BatchGenerator");
                    batch_generator_shutdown_rx
                        .await
                        .expect("Failed to stop BatchGenerator");

                    let (batch_coordinator_shutdown_tx, batch_coordinator_shutdown_rx) =
                        oneshot::channel();
                    self.batch_coordinator_cmd_tx
                        .send(BatchCoordinatorCommand::Shutdown(
                            batch_coordinator_shutdown_tx,
                        ))
                        .await
                        .expect("Failed to send to BatchCoordinator");
                    batch_coordinator_shutdown_rx
                        .await
                        .expect("Failed to stop BatchCoordinator");

                    for remote_batch_coordinator_cmd_tx in self.remote_batch_coordinator_cmd_tx {
                        let (
                            remote_batch_coordinator_shutdown_tx,
                            remote_batch_coordinator_shutdown_rx,
                        ) = oneshot::channel();
                        remote_batch_coordinator_cmd_tx
                            .send(BatchCoordinatorCommand::Shutdown(
                                remote_batch_coordinator_shutdown_tx,
                            ))
                            .await
                            .expect("Failed to send to Remote BatchCoordinator");
                        remote_batch_coordinator_shutdown_rx
                            .await
                            .expect("Failed to stop Remote BatchCoordinator");
                    }

                    let (batch_store_shutdown_tx, batch_store_shutdown_rx) = oneshot::channel();
                    self.batch_store_cmd_tx
                        .send(BatchStoreCommand::Shutdown(batch_store_shutdown_tx))
                        .await
                        .expect("Failed to send to BatchStore");
                    batch_store_shutdown_rx
                        .await
                        .expect("Failed to stop BatchStore");

                    let (proof_coordinator_shutdown_tx, proof_coordinator_shutdown_rx) =
                        oneshot::channel();
                    self.proof_coordinator_cmd_tx
                        .send(ProofCoordinatorCommand::Shutdown(
                            proof_coordinator_shutdown_tx,
                        ))
                        .await
                        .expect("Failed to send to ProofCoordinator");
                    proof_coordinator_shutdown_rx
                        .await
                        .expect("Failed to stop ProofCoordinator");

                    let (proof_manager_shutdown_tx, proof_manager_shutdown_rx) = oneshot::channel();
                    self.proof_manager_cmd_tx
                        .send(ProofManagerCommand::Shutdown(proof_manager_shutdown_tx))
                        .await
                        .expect("Failed to send to ProofManager");
                    proof_manager_shutdown_rx
                        .await
                        .expect("Failed to stop ProofManager");

                    ack_tx
                        .send(())
                        .expect("Failed to send shutdown ack from QuorumStore");
                    break;
                },
            }
        }
    }
}