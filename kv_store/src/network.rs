use kube::{api::{Api, WatchEvent, WatchParams, ListParams},Client, runtime::watcher};
use k8s_openapi::api::core::v1::Pod;
use futures::{Stream, StreamExt, TryStreamExt};
use omnipaxos::messages::Message as OPMessage;
use serde::{Deserialize, Serialize};
use serde_json;
use std::{collections::HashMap, hash::Hash};
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{tcp, TcpStream},
    sync::Mutex,
    time::Duration
};

use crate::{kv::KVCommand, server::APIResponse, NODES, PID as MY_PID};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Message {
    OmniPaxosMsg(OPMessage<KVCommand>),
    APIRequest(KVCommand),
    APIResponse(APIResponse),
}

pub struct Network {
    sockets: HashMap<u64, tcp::OwnedWriteHalf>,
    api_socket: Option<tcp::OwnedWriteHalf>,
    incoming_msg_buf: Arc<Mutex<Vec<Message>>>,
}

impl Network {
    fn get_my_api_addr() -> String {
        format!("net.default.svc.cluster.local:800{}", *MY_PID)
    }

    fn get_peer_addr(receiver_pid: u64) -> String {
        format!(
            "net.default.svc.cluster.local:80{}{}",
            *MY_PID, receiver_pid
        )
    }

    /// Sends the message to the receiver.
    /// u64 0 is the Client.
    pub(crate) async fn send(&mut self, receiver: u64, msg: Message) {
        let writer = if receiver == 0 {
            self.api_socket.as_mut()
        } else {
            self.sockets.get_mut(&receiver)
        };
        if let Some(writer) = writer {
            let mut data = serde_json::to_vec(&msg).expect("could not serialize msg");
            data.push(b'\n');
            writer.write_all(&data).await.unwrap();
        }
    }

    pub(crate) async fn check_pods(&mut self) -> u64 {
        let client = Client::try_default().await.unwrap();
        let pods: Api<Pod> = Api::default_namespaced(client);
        let pod_list = pods.list(&ListParams::default()).await.unwrap();
    
        // Collect the list of peer PIDs
        let peers: Vec<u64> = NODES
            .iter()
            .filter(|pid| **pid != *MY_PID)
            .cloned()
            .collect();

        let mut return_val: u64 = 0;
    
        for pod in pod_list.items {
            if let Some(pod_name) = &pod.metadata.name {
                if pod_name.starts_with("kv-store") && *pod_name != format!("kv-store-{}", *MY_PID - 1) {
                    
                    let parts: Vec<&str> = pod_name.split('-').collect();
                    if parts.len() < 3 {
                        println!("Skipping pod {}: Invalid name format", pod_name);
                        continue;
                    }
    
                    let temp_pid: u64 = match parts[2].parse::<u64>() {
                        Ok(pid) => pid + 1, // Ensure PID is non-zero
                        Err(_) => {
                            println!("Skipping pod {}: Invalid PID", pod_name);
                            continue;
                        }
                    };
    
                    // Check if temp_pid exists in peers
                    if peers.contains(&temp_pid) {
                        println!("Detected new peer pod: {} with PID {}", pod_name, temp_pid);
                    } else {
                        println!("Pod {} is not in the peer list, sending reconfigure request.", pod_name);
                        return_val = temp_pid;
                    }
                }
            }
        }
        return return_val;
    }
    
    /// Returns all messages received since last called.
    pub(crate) async fn get_received(&mut self) -> Vec<Message> {
        let mut buf = self.incoming_msg_buf.lock().await;
        let ret = buf.to_vec();
        buf.clear();
        ret
    }

    /// Constructs a new Network instance and connects the Sockets.
    pub(crate) async fn new() -> Self {
        let peers: Vec<u64> = NODES
            .iter()
            .filter(|pid| **pid != *MY_PID)
            .cloned()
            .collect();
        let mut peer_addrs = HashMap::new();
        for pid in &peers {
            peer_addrs.insert(*pid, Self::get_peer_addr(*pid));
        }
        println!("My API Addr: {}", Self::get_my_api_addr());
        let err_msg = format!("Could not connect to API at {}", Self::get_my_api_addr());
        let api_stream = TcpStream::connect(Self::get_my_api_addr())
            .await
            .expect(&err_msg);
        let (api_reader, api_writer) = api_stream.into_split();
        let api_socket = Some(api_writer);
        let incoming_msg_buf = Arc::new(Mutex::new(vec![]));
        let msg_buf = incoming_msg_buf.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(api_reader);
            let mut data = Vec::new();
            loop {
                data.clear();
                let bytes_read = reader.read_until(b'\n', &mut data).await;
                if bytes_read.is_err() {
                    // stream ended?
                    panic!("stream ended?")
                }
                let msg: Message =
                    serde_json::from_slice(&data).expect("could not deserialize msg");
                msg_buf.lock().await.push(msg);
            }
        });

        let mut sockets = HashMap::new();
        for peer in &peers {
            let addr = peer_addrs.get(&peer).unwrap().clone();
            println!("Connecting to {}", addr);
            let stream = loop {
                match TcpStream::connect(addr.clone()).await {
                    Ok(s) => break s,
                    Err(e) => {
                        println!("Failed to connect to {}: {}. Retrying...", addr, e);
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            };
            let (reader, writer) = stream.into_split();
            sockets.insert(*peer, writer);
            let msg_buf = incoming_msg_buf.clone();
            let peer_clone = peer.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(reader);
                let mut data = Vec::new();
                loop {
                    data.clear();
                    // println!("🔄 Waiting to receive data from {}", peer_clone);

                    let timeout_duration = tokio::time::Duration::from_secs(5);
                    match tokio::time::timeout(timeout_duration, reader.read_until(b'\n', &mut data)).await {
                        // The connection was closed (zero bytes read)
                        Ok(Ok(0)) => {
                            println!("Connection lost with {}. Attempting to reconnect...", peer_clone);
                            break;
                        }
                        // Successfully read data
                        Ok(Ok(_)) => {
                            // println!("Stream alive with {}", peer_clone);
                            if let Ok(msg) = serde_json::from_slice::<Message>(&data) {
                                msg_buf.lock().await.push(msg);
                            }
                        }
                        // Error reading data
                        Ok(Err(e)) => {
                            println!("Error reading from {}: {}", peer_clone, e);
                            break;
                        }
                        // Timeout occurred
                        Err(_) => {
                            println!("Timeout waiting for data from {}", peer_clone);
                        }
                    }
                }
            });
        }
        Self {
            sockets,
            api_socket,
            incoming_msg_buf,
        }
        
    }
}
