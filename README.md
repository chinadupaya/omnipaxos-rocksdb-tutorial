# OmniPaxos Demo
This is a small demo of how to transform a simple single-server RocksDB service into a distributed version using OmniPaxos.

## Members
- Annysia Dupaya - Code, Report
- Gnanapalaniselvi - Report, Testing
- Tomas Sobral Teixeira

Related resources:
- [Blog post](https://omnipaxos.com/blog/building-distributed-rocksdb-with-omnipaxos-in-8-minutes/)
- [Video](https://youtu.be/4VqB0-KOsms)

## Requirements
We used the following:
- Docker v.4.19.0 (for demo)
- Rust 1.80.0 (for development)

# Demos

# Build new image in the kv_store folder:

```bash
$ docker build -t kvs .
```
Sometimes it's better to rebuild completely from scratch: 
```bash
$ docker build -t kvs . --no-cache
```
```bash
$ docker tag kvs:latest chinadupaya/kvs:latest
$ docker push chinadupaya/kvs:latest
```

# Build new image in the network_actor folder:
```bash
$ docker build -t kvs-network-actor .
$ docker tag kvs-network-actor:latest chinadupaya/kvs-network-actor
$ docker push chinadupaya/kvs-network-actor:latest
```

# With Minikube:
```bash
$ minikube start
```

```bash
$ kubectl create -f kube.yml 
```
Attach to the client (`network-actor`) to send requests to the cluster:
```bash
$ kubectl attach -it net
```
Cleanup:
```bash
$ minikube delete --all
```

## delete node
```bash
$ kubectl delete pod <kv-store-2>
```
Pod will be deleted and restart

## Add another pod:
- Edit nodes config
```bash
$ kubectl patch configmap kv-config --type merge -p '{"data":{"NODES":"[1,2,3,4]"}}'
$ kubectl patch configmap kv-config --type merge -p '{"data":{"CONFIG_ID":"2"}}'
```
- increase to four pods
```bash
$ kubectl scale statefulset kv-store --replicas=4
```
- Attach to network-actor and input `reconfigure <PID>`. This tells it to send a stopsign that a new node will be added
```bash
$ reconfigure 4
```

### Client
Example network-actor CLI command:
```
Type a command here <put/delete/get> <args>: put a 1
```
Asks the cluster to write { key: "a", value: "1" }.

To send a command to a specific server, include its port at the end of the command e.g.,
```
get a 8001
```
Reads the value associated with "a" from server `s1` listening on port 8001.

## Demo 0: Single server
(Make sure to `git checkout single-server` branch before running docker compose)
1. Propose some commands from client.
2. In another terminal, kill the server:
```bash
$ docker kill s1
```
3. In the client terminal, try sending commands again. No command succeeds because the only server is down.

## Demo 1: Fault-tolerance
(Make sure to `git checkout omnipaxos-replicated` branch before running docker compose)
1. Attach to a majority of the servers to observe the OmniPaxos log:
```bash
$ docker attach s2
```
```bash
$ docker attach s3
```
2. Repeat steps 1-3 from Demo 0 (kill `s1`). This time, in step 3, the commands should still be successful because we still have a majority of servers running (`s2` and `s3`).
3. Simulate disconnection between the remaining servers by pausing `s2`:
```bash
$ docker pause s2
```
4. Propose commands. ``Put`` and ``Delete`` will not be successful regardless of which server receives them because they cannot get committed in the log without being replicated by a majority. However, ``Get``s from `s3` still works since it's still running.
5. Propose multiple values to the same key at both servers, e.g.,
```
put a 2 8002
put a 3 8003
```
6. Unpause ``s2`` and see on the servers' terminals that the concurrent modifications are ordered identically in to the OmniPaxos log.
```bash
$ docker unpause s2
```

## Demo 2: Snapshot
(Make sure to `git checkout omnipaxos-snapshot` branch before running docker compose)
1. Attach to one of the servers e.g., ``s3`` to observe the OmniPaxos log:
```bash
$ docker attach s3
```
2. Propose 5 commands from the client and see how the entries get squashed into one snapshotted entry on the server. Propose 5 more commands to see the 5 new entries get snapshotted and merged with the old snapshot.
