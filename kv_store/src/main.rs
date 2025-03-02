use crate::kv::KVCommand;
use crate::server::Server;
use omnipaxos::*;
// use omnipaxos_storage::memory_storage::MemoryStorage;
use std::env;
use tokio;
use omnipaxos_storage::persistent_storage::{PersistentStorage, PersistentStorageConfig};
use std::sync::Arc;
use std::fs;
use tokio::sync::Mutex;

#[macro_use]
extern crate lazy_static;

mod database;
mod kv;
mod network;
mod server;

lazy_static! {
    pub static ref NODES: Vec<u64> = if let Ok(var) = env::var("NODES") {
        serde_json::from_str::<Vec<u64>>(&var).expect("wrong config format")
    } else {
        vec![]
    };
    pub static ref PID: u64 = if let Ok(var) = env::var("PID") {
        let x = var.parse().expect("PIDs must be u64");
        if x == 0 {
            panic!("PIDs cannot be 0")
        } else {
            x
        }
    } else {
        panic!("missing PID")
    };
}

type OmniPaxosKV = OmniPaxos<KVCommand, PersistentStorage<KVCommand>>;

#[tokio::main]
async fn main() {
    let server_config = ServerConfig {
        pid: *PID,
        election_tick_timeout: 5,
        ..Default::default()
    };
    let cluster_config = ClusterConfig {
        configuration_id: 1,
        nodes: (*NODES).clone(),
        ..Default::default()
    };
    let op_config = OmniPaxosConfig {
        server_config,
        cluster_config,
    };
    let storage_path = format!("/data/omnipaxos_storage_{}", *PID);
    let backup_path = format!("/data/omnipaxos_storage_backup_{}", *PID);
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

    let persistent_storage_primary = PersistentStorage::open(PersistentStorageConfig::with_path(storage_path.clone()));

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

    let omni_paxos_result = op_config.clone().build(persistent_storage);

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
                    let _ = std::fs::remove_file(format!("/data/omnipaxos_storage_{}/LOCK", *PID));
            
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
}
