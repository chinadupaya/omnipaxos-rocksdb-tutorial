use crate::kv::KVCommand;
use crate::server::Server;
use omnipaxos::*;
use omnipaxos_storage::persistent_storage::{PersistentStorage, PersistentStorageConfig};
use std::env;
use tokio;

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
    // user-defined configuration
    let my_path = "my_storage";
    let log_store_options = rocksdb::Options::default();
    let mut state_store_options = rocksdb::Options::default();
    state_store_options.create_missing_column_families(true); // required
    state_store_options.create_if_missing(true); // required

    // generate default configuration and set user-defined options
    let mut my_config = PersistentStorageConfig::default();
    my_config.set_path(my_path.to_string());
    my_config.set_database_options(state_store_options);
    my_config.set_log_options(log_store_options);

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
    let omni_paxos = op_config
        .build(PersistentStorage::open(my_config))
        .expect("failed to build OmniPaxos");
    let mut server = Server {
        omni_paxos,
        network: network::Network::new().await,
        database: database::Database::new(format!("db_{}", *PID).as_str()),
        last_decided_idx: 0,
    };
    server.run().await;
}
