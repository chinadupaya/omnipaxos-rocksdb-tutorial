use kube::{api::{Api, WatchEvent, WatchParams, ListParams},Client, runtime::watcher};
use k8s_openapi::api::core::v1::Pod;
use futures::{StreamExt, TryStreamExt};
use std::{collections::HashMap, hash::Hash};
use omnipaxos::messages::Message as OPMessage;
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader },
    net::{tcp, TcpStream},
    sync::{Mutex},
};

use serde::{Deserialize, Serialize};
use serde_json;
use futures::Stream;

use crate::{kv::KVCommand, server::APIResponse, NODES, PID as MY_PID};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Message {
    OmniPaxosMsg(OPMessage<KVCommand>),
    APIRequest(KVCommand),
    APIResponse(APIResponse),
}

pub struct Network {
    sockets: Arc<Mutex<HashMap<u64, tcp::OwnedWriteHalf>>>,
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

    /// Returns all messages received since last called.
    pub(crate) async fn get_received(&mut self) -> Vec<Message> {
        // println!("Getting received messages");
        let mut buf = self.incoming_msg_buf.lock().await;
        let ret = buf.to_vec();
        buf.clear();
        ret
    }
    pub async fn connect_pod(&mut self, pod_name: &str, pod_id: u64) {
        let my_connection;
        if pod_name.starts_with("net") {
            println!("My API Addr: {}", Self::get_my_api_addr());
            my_connection = Self::get_my_api_addr();
        } else {
            println!("Get peer address: {:?}", Self::get_peer_addr(pod_id));
            my_connection = Self::get_peer_addr(pod_id);
        }
        let stream = TcpStream::connect(my_connection.clone()).await.unwrap(); // name resolution error occurs here
        let (read_half, write_half) = stream.into_split();
        let pod_name_copy = pod_name.to_string();
        if pod_name.starts_with("kv-store") {
            println!("kv-store pod");
            let mut sockets = self.sockets.lock().await;
            sockets.insert(pod_id, write_half);
            let incoming_msg_buf = self.incoming_msg_buf.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(read_half);
                let mut data = Vec::new();
                loop {
                    data.clear();
                    println!("Waiting for data from {}", pod_name_copy);
                    let bytes_read = reader.read_until(b'\n', &mut data).await;
                    if bytes_read.is_err() {
                        // stream ended?
                        println!("stream ended error {}" , pod_name_copy);
                        panic!("stream ended?")
                    } else if bytes_read.unwrap() == 0 {
                        println!("stream ended {}" , pod_name_copy);
                        // panic!("stream ended?")
                    } else {
                        println!("stream alive with {}" , pod_name_copy);
                    }
                    println!("Bytes received from {}: {:?}", pod_name_copy, data);
                    let msg: Message =
                        serde_json::from_slice(&data).expect("could not deserialize msg");
                    let mut msg_buf = incoming_msg_buf.lock().await;
                    msg_buf.push(msg);
                }
            });
        } else if pod_name.starts_with("net") {
            println!("net pod");
            self.api_socket = Some(write_half);
            let msg_buf = self.incoming_msg_buf.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(read_half);
                let mut data = Vec::new();
                loop {
                    data.clear();
                    println!("Waiting for data from {}", pod_name_copy);
                    let bytes_read = reader.read_until(b'\n', &mut data).await;
                    if bytes_read.is_err() {
                        // stream ended?
                        println!("stream ended error {}" , pod_name_copy);
                        panic!("stream ended?")
                    } else if bytes_read.unwrap() == 0 {
                        println!("stream ended {}" , pod_name_copy);
                        // panic!("stream ended?")
                    } else {
                        println!("stream alive with {}" , pod_name_copy);
                    }
                    let msg: Message =
                        serde_json::from_slice(&data).expect("could not deserialize msg");
                    msg_buf.lock().await.push(msg);
                }
            });
        }
    }


    /// Sends the message to the receiver.
    /// u64 0 is the Client.
    pub(crate) async fn send(&mut self, receiver: u64, msg: Message) {
        println!("Receier of send is: {:?}", receiver);
        if receiver == 0 {
            if let Some(writer) = self.api_socket.as_mut() {
                let mut data = serde_json::to_vec(&msg).expect("could not serialize msg");
                data.push(b'\n');
                writer.write_all(&data).await.unwrap();
            }
        } else {
            // Lock the mutex, and get a mutable reference to the writer
            let mut sockets = self.sockets.lock().await; // Lock the mutex
            if let Some(writer) = sockets.get_mut(&receiver) {
                let mut data = serde_json::to_vec(&msg).expect("could not serialize msg");
                data.push(b'\n');
                writer.write_all(&data).await.unwrap();
            }
        }
    }
    pub(crate) async fn new() -> Self {
        Self {
            sockets: Arc::new(Mutex::new(HashMap::new())),
            api_socket: None,
            incoming_msg_buf: Arc::new(Mutex::new(vec![])),
        }
    }

    pub async fn listen(&mut self) {
        let client = Client::try_default().await.unwrap();
        let pods: Api<Pod> = Api::default_namespaced(client);
        let lp = WatchParams::default();
        let mut stream: std::pin::Pin<Box<dyn Stream<Item = Result<WatchEvent<Pod>, kube::Error>> + Send>> = pods.watch(&lp, "0").await.unwrap().boxed();

        while let Some(status) = stream.try_next().await.unwrap() {
            match status {
                WatchEvent::Added(pod) => {
                    println!("Pod ADDED: {}",pod.metadata.name.as_deref().unwrap_or("unknown"));
                    if let Some(pod_name) = pod.metadata.name { 
                        // println!("Pod name {:?}", pod_name);
                
                        if (pod_name.starts_with("kv-store") || pod_name.starts_with("net")) && pod_name != format!("kv-store-{}", *MY_PID - 1) {
                            let mut pod_id = 0; // defaults to client

                            let parts: Vec<&str> = pod_name.split('-').collect();
                            if parts.len() >= 3 {
                                let mut x = parts[2].parse().expect("PIDs must be u64");
                                x += 1;
                                pod_id = x;
                            }
                            println!("{} is attempting connection to pod {}", format!("kv-store-{}", *MY_PID - 1), pod_name);
                            self.connect_pod(&pod_name, pod_id).await;
                        }
                            
                        
                    }

                }
                WatchEvent::Modified(pod) => {
                    println!("Pod MODIFIED: {}", pod.metadata.name.as_deref().unwrap_or("unknown"));
                    // if let Some(pod_name) = pod.metadata.name {
                    //     println!("Pod name {:?}", pod_name);
                    //     println!("Pod status {:?}", pod.status);
                        
                    // }
                }
                WatchEvent::Deleted(pod) => {
                    println!("Pod DELETED: {}", pod.metadata.name.as_deref().unwrap_or("unknown"));
                }
                _ => {}
            }
        }
    }
}