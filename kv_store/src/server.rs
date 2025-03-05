use crate::database::Database;
use crate::kv::KVCommand;
use crate::{
    network::{Message, Network},
    OmniPaxosKV,
    NODES,
    PID as MY_PID,
    CONFIG_ID

};

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::thread::current;
use std::{env, fs};
use tokio::sync::Mutex;
// use omnipaxos::messages::ballot_leader_election::{BLEMessage, HeartbeatMsg };
use omnipaxos::messages::ballot_leader_election::*;

use omnipaxos_storage::persistent_storage::{PersistentStorage, PersistentStorageConfig};
use omnipaxos::{ClusterConfig, ServerConfig};
use omnipaxos::util::LogEntry;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::time;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum APIResponse {
    Decided(u64),
    Get(String, Option<String>),
}

pub struct Server {
    pub omni_paxos: OmniPaxosKV,
    pub network: Network,
    pub database: Database,
    pub last_decided_idx: u64,
    pub heartbeats: HashMap<u64, Instant>,
    pub expired_nodes: HashSet<u64>
}

impl Server {
    async fn process_incoming_msgs(&mut self) {
        let messages = self.network.get_received().await;
         // should ignore message from client (PID = 0)
        for msg in messages {
            match msg {
                Message::APIRequest(kv_cmd) => match kv_cmd {
                    KVCommand::Get(key) => {
                        let value = self.database.handle_command(KVCommand::Get(key.clone()));
                        let msg = Message::APIResponse(APIResponse::Get(key, value));
                        self.network.send(0, msg).await;
                    }
                    KVCommand::Reconfigure(key) => {
                        println!("Received reconfigure {}", key);
                        let new_configuration = ClusterConfig {
                            configuration_id: 2,
                            nodes: vec![1,2,3,4],
                            ..Default::default()
                        };
                        let metadata = None;
                        self.omni_paxos.reconfigure(new_configuration, metadata).expect("Failed to propose reconfiguration");
                    }
                    cmd => {
                        self.omni_paxos.append(cmd).unwrap();
                    }
                },
                Message::OmniPaxosMsg(msg) => {
                    // println!("omnipaxos msg {:?}", msg);
                    // if i am the leader
                    let leader = self.omni_paxos.get_current_leader();
                    let sender = msg.get_sender();
                    self.heartbeats.insert(sender, Instant::now());
                    self.expired_nodes.remove(&sender);
                    self.omni_paxos.handle_incoming(msg);
                }
                _ => {
                    println!("Received unimplemented msg {:?}", msg);
                },
            }
        }
    }

    async fn send_outgoing_msgs(&mut self) {
        let messages = self.omni_paxos.outgoing_messages();
        for msg in messages {
            let receiver = msg.get_receiver();
            println!("Trying to send message to {} ", receiver);
            self.network
                .send(receiver, Message::OmniPaxosMsg(msg))
                .await;
        }
    }

    async fn handle_decided_entries(&mut self) {
        let new_decided_idx = self.omni_paxos.get_decided_idx();
        if self.last_decided_idx < new_decided_idx as u64 {
            // let decided_entries = self
            //     .omni_paxos
            //     .read_decided_suffix(self.last_decided_idx as usize)
            //     .unwrap();
            // self.update_database(decided_entries);
            println!(
                "Reading decided suffix from index: {} (new_decided_idx: {})",
                self.last_decided_idx, new_decided_idx
            );
            if let Some(decided_entries) = self.omni_paxos.read_decided_suffix(self.last_decided_idx as usize) {
                self.update_database(decided_entries);
            } else {
                println!("Warning: No decided entries found at index {}", self.last_decided_idx);
                self.omni_paxos.reconnected(*MY_PID);
                // self.omni_paxos.read
            }
            self.last_decided_idx = new_decided_idx as u64;
            /*** reply client ***/
            let msg = Message::APIResponse(APIResponse::Decided(new_decided_idx as u64));
            self.network.send(0, msg).await;
            // snapshotting
            if new_decided_idx % 5 == 0 {
                println!(
                    "Log before: {:?}",
                    self.omni_paxos.read_decided_suffix(0).unwrap()
                );
                self.omni_paxos
                    .snapshot(Some(new_decided_idx), true)
                    .expect("Failed to snapshot");
                println!(
                    "Log after: {:?}\n",
                self.omni_paxos.read_decided_suffix(0).unwrap()
                );
            }
        }
    }

    fn update_database(&self, decided_entries: Vec<LogEntry<KVCommand>>) {
        for entry in decided_entries {
            match entry {
                LogEntry::Decided(cmd) => {
                    self.database.handle_command(cmd);
                }
                LogEntry::StopSign(stopsign, true) => {
                    println!("Received stopsign");
                    
                    let new_configuration = stopsign.next_config.clone(); 
                    if new_configuration.nodes.contains(&MY_PID) && stopsign.next_config.configuration_id > *CONFIG_ID{
                        // current configuration has been safely stopped. Start new instance
                        // force restart
                         // Remove lock to allow restart
                        // let _ = std::fs::remove_file(format!("/data/omnipaxos_storage_{}/LOCK", *PID));
                        
                        // Restart process (optional: implement restart logic)
                        // std::process::exit(1);

                        let storage_path = format!("/data/omnipaxos_storage_{}", *MY_PID);
                        let db_path = "/data/db";
                        fn remove_lock_file(path: &str) {
                            let lock_file = format!("{}/LOCK", path);
                            if std::path::Path::new(&lock_file).exists() {
                                println!("🛠 Removing stale lock file: {}", lock_file);
                                fs::remove_file(&lock_file).expect("Failed to remove lock file");
                            }
                        }
                        remove_lock_file(&storage_path);
                        remove_lock_file(db_path);
                        
                        let persistent_storage: PersistentStorage<KVCommand> = PersistentStorage::open(PersistentStorageConfig::with_path(storage_path.clone()));
                        let current_config = ServerConfig {
                            pid: *MY_PID,
                            ..Default::default()
                        };
                        let new_paxos_result = new_configuration.build_for_server(current_config, persistent_storage);
                        if let Ok(new_omni_paxos) = new_paxos_result {
                            println!("✅ Reconfiguration successful, new OmniPaxos instance created!");
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub(crate) async fn run(&mut self) {
        let mut msg_interval = time::interval(Duration::from_millis(1));
        let mut tick_interval = time::interval(Duration::from_millis(10));
        loop {
            tokio::select! {
                biased;
                _ = msg_interval.tick() => {
                    self.process_incoming_msgs().await;
                    self.send_outgoing_msgs().await;
                    self.handle_decided_entries().await;
                },
                _ = tick_interval.tick() => {
                    self.omni_paxos.tick();
                    let leader = self.omni_paxos.get_current_leader();
                    let now = Instant::now();
                    let expired_nodes: Vec<u64> = self.heartbeats
                        .iter()
                        .filter(|(_, last_seen)| now.duration_since(**last_seen) >= Duration::from_millis(100))
                        .map(|(sender_id, _)| *sender_id)
                        .collect();

                    // Mark nodes as expired if they aren't already
                    for sender_id in &expired_nodes {
                        if !self.expired_nodes.contains(sender_id) {
                            // println!("Node {} is unresponsive. Marking for reconnection...", sender_id);
                            self.expired_nodes.insert(*sender_id);
                        }
                    }

                    // Keep retrying reconnection for expired nodes
                    for sender_id in &self.expired_nodes {
                        println!("Trying to reconnect to node {}...", sender_id);
                        self.omni_paxos.reconnected(*sender_id);
                    }

                    // Remove expired heartbeats from tracking
                    for sender_id in expired_nodes {
                        self.heartbeats.remove(&sender_id);
                    }
                    if leader.is_none() {
                        // println!("No leader detected! Reconnecting node");
                        let _ = std::fs::remove_file(format!("/data/omnipaxos_storage_{}/LOCK", *MY_PID));
                                
                        // Restart process (optional: implement restart logic)
                        self.omni_paxos.reconnected(*MY_PID);
                    }
                },
                else => (),
            }
        }
    }

    pub async fn new(omni_paxos: OmniPaxosKV, db_path: &str) -> Self {
        Self {
            omni_paxos,
            network: Network::new().await,
            database: Database::new(db_path),
            last_decided_idx: 0,
            heartbeats: HashMap::new(),
            expired_nodes: HashSet::new()
            
        }
    }
}
