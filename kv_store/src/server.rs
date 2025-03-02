use crate::database::Database;
use crate::kv::KVCommand;
use crate::{
    network::{Message, Network},
    OmniPaxosKV,
    NODES,
    PID as MY_PID
};

use std::sync::Arc;
use std::fs;
use tokio::sync::Mutex;

use omnipaxos_storage::persistent_storage::{PersistentStorage, PersistentStorageConfig};
use omnipaxos::{ClusterConfig, ServerConfig};
use omnipaxos::util::LogEntry;
use serde::{Deserialize, Serialize};
use std::time::Duration;
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
}

impl Server {
    async fn process_incoming_msgs(&mut self) {
        let messages = self.network.get_received().await;
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
                        // let nodes_copy: Vec<u64> = NODES
                        // .iter()
                        // .cloned()
                        // .collect();
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
            self.network
                .send(receiver, Message::OmniPaxosMsg(msg))
                .await;
        }
    }

    async fn handle_decided_entries(&mut self) {
        let new_decided_idx = self.omni_paxos.get_decided_idx();
        if self.last_decided_idx < new_decided_idx as u64 {
            let decided_entries = self
                .omni_paxos
                .read_decided_suffix(self.last_decided_idx as usize)
                .unwrap();
            self.update_database(decided_entries);
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

    async fn update_database(&self, decided_entries: Vec<LogEntry<KVCommand>>) {
        for entry in decided_entries {
            match entry {
                LogEntry::Decided(cmd) => {
                    self.database.handle_command(cmd);
                }
                LogEntry::StopSign(stopsign, true) => {
                    let current_config = ServerConfig {
                        pid: *MY_PID,
                        ..Default::default()
                    };
                    let new_configuration = stopsign.next_config; 
                    if new_configuration.nodes.contains(&MY_PID) {
                        // current configuration has been safely stopped. Start new instance

                        let storage_path = format!("/data/omnipaxos_storage_{}", *MY_PID);
                        let backup_path = format!("/data/omnipaxos_storage_backup_{}", *MY_PID);
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

                        let persistent_storage_primary: PersistentStorage<KVCommand> = PersistentStorage::open(PersistentStorageConfig::with_path(storage_path.clone()));

                        let persistent_storage = if let PersistentStorage { .. } = persistent_storage_primary {
                            println!("✅ Primary PersistentStorage opened successfully.");
                            persistent_storage_primary
                        } else {
                            println!("⚠️ WARNING: Primary storage failed, switching to backup...");
                            let persistent_storage_backup = PersistentStorage::open(PersistentStorageConfig::with_path(backup_path.clone()));
                            if let PersistentStorage { .. } = persistent_storage_backup {
                                println!("✅ Backup PersistentStorage opened successfully.");
                                persistent_storage_backup
                            } else {
                                panic!("❌ CRITICAL: Failed to open both primary and backup PersistentStorage!");
                            }
                        };
                        let omni_paxos_result = new_configuration.build_for_server(current_config.clone(), persistent_storage);

                        // use new_omnipaxos

                        if let Ok(omni_paxos) = omni_paxos_result {
                            // ✅ Use Arc<Mutex<T>> to allow multiple async tasks to access `server`
                            let server = Arc::new(Mutex::new(Server::new(omni_paxos, db_path).await));
                    
                            // ✅ Clone `server` to avoid move issues
                            let server_clone = Arc::clone(&server);
                    
                            tokio::spawn(async move {
                                loop {
                                    let decided_idx;
                                    let last_decided_idx;
                                    let leader;
                    
                                    {
                                        let server_guard = server_clone.lock().await;
                                        decided_idx = server_guard.omni_paxos.get_decided_idx();
                                        last_decided_idx = server_guard.last_decided_idx as usize;
                                        leader = server_guard.omni_paxos.get_current_leader();
                                    }
                    
                                    // ✅ Ensure logs are in sync
                                    if decided_idx < last_decided_idx {
                                        println!("⚠️ Node is behind, requesting missing logs...");
                                        let mut server_guard = server_clone.lock().await;
                                        server_guard
                                            .omni_paxos
                                            .trim(Some(last_decided_idx))
                                            .expect("Trim failed!");
                                    }
                    
                                    // ✅ Log leader status instead of calling trigger_election()
                                    if leader.is_none() {
                                         println!("No leader detected! Restarting node to trigger election");
                                
                                // Remove lock to allow restart
                                        let _ = std::fs::remove_file(format!("/data/omnipaxos_storage_{}/LOCK", *MY_PID));
                                
                                // Restart process (optional: implement restart logic)
                                    std::process::exit(1);
                            }
                    
                                    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                                }
                            });
                    
                            // ✅ Start the server
                            server.lock().await.run().await;
                        } else {
                            if let Err(e) = omni_paxos_result {
                                println!("Failed to initialize OmniPaxos: {:?}", e);
                            }
                        }
                        // ...
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
        }
    }
}
