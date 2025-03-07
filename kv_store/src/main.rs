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
    pub static ref CONFIG_ID: u32 = if let Ok(var) = env::var("CONFIG_ID") {
        let x = var.parse().expect("PIDs must be u32");
        if x == 0 {
            panic!("CONFIG_IDs cannot be 0")
        } else {
            x
        }
    } else {
        panic!("missing CONFIG_ID")
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
        configuration_id: (*CONFIG_ID).clone(),
        nodes: (*NODES).clone(),
        ..Default::default()
    };
    let op_config = OmniPaxosConfig {
        server_config,
        cluster_config,
    };
    let storage_path = format!("/data/omnipaxos_storage_{}_{}", *PID, *CONFIG_ID);
    let db_path = format!("data/db{}",*CONFIG_ID);

    fn remove_lock_file(path: &str) {
        let lock_file = format!("{}/LOCK", path);
        if std::path::Path::new(&lock_file).exists() {
            println!("🛠 Removing stale lock file: {}", lock_file);
            fs::remove_file(&lock_file).expect("Failed to remove lock file");
        }
    }

    remove_lock_file(&storage_path);
    remove_lock_file(&db_path);

    let persistent_storage_primary = PersistentStorage::open(PersistentStorageConfig::with_path(storage_path.clone()));

    let persistent_storage = if let PersistentStorage { .. } = persistent_storage_primary {
        println!("✅ Primary PersistentStorage opened successfully.");
        persistent_storage_primary
    } else {
        panic!("!!Failed to open PersistentStorage!!");
    };

    let omni_paxos_result = op_config.clone().build(persistent_storage);

    if let Ok(omni_paxos) = omni_paxos_result {
        let server = Arc::new(Mutex::new(Server::new(omni_paxos, &db_path).await));

        // ✅ Start the server
        server.lock().await.run().await;
    } else {
        if let Err(e) = omni_paxos_result {
            println!("Failed to initialize OmniPaxos: {:?}", e);
        }
    }
}
