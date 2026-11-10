use serde_json::{Value, json};
use services::fcr::{Config, Gate};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use url::Url;

fn hash(n: u64) -> String {
    format!("0x{n:064x}")
}
struct Server {
    url: Url,
    state: Arc<Mutex<Value>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let state = Arc::new(Mutex::new(
            json!({"height":10,"fork":false,"incident":null,"stale":false}),
        ));
        let shared = state.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let thread = std::thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut headers = Vec::new();
                let mut byte = [0; 1];
                while !headers.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    headers.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&headers);
                let length = text
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                let query: Value = serde_json::from_slice(&body).unwrap();
                let query = query["query"].as_str().unwrap();
                let state = shared.lock().unwrap();
                let value = if query.contains("nodeInfo") {
                    json!({"data":{"nodeInfo":{"ethereumFinality":{"enabled":state["enabled"]!=false,"generation":if state["malformed"]==true {Value::Null}else {json!("0")},"checked_at":if state["stale"]==true {0} else {std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()},"incident":state["incident"]}}}})
                } else if query.contains("latestBlock") {
                    let height = state["height"].as_u64().unwrap();
                    json!({"data":{"chain":{"latestBlock":{"height":height.to_string(),"id":hash(height)},"consensusParameters":{"chainId":"9889"}}}})
                } else {
                    json!({"data":{"block":{"id":hash(if state["fork"]==true {999} else {10})}}})
                };
                let body = serde_json::to_vec(&value).unwrap();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        Self {
            url,
            state,
            stop,
            thread: Some(thread),
        }
    }
    fn config(&self) -> Config {
        Config {
            status_urls: vec![self.url.clone()],
            fuel_url: self.url.clone(),
            fuel_chain_id: "9889".into(),
            writer_identity: "test".into(),
            generation: "0".into(),
            freshness_seconds: 60,
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}

#[derive(Debug)]
struct MemoryJournal(Mutex<Value>);
#[async_trait::async_trait]
impl services::fcr::Journal for MemoryJournal {
    async fn load(&self) -> services::Result<Value> {
        Ok(self.0.lock().unwrap().clone())
    }
    async fn save(&self, state: Value) -> services::Result<()> {
        *self.0.lock().unwrap() = state;
        Ok(())
    }
}
fn journal() -> Arc<MemoryJournal> {
    Arc::new(MemoryJournal(Mutex::new(
        json!({"generation":"0","fuel_chain_id":"9889","writer_identity":"test","incident":null,"height":null,"hash":null,"progressed_at":0}),
    )))
}

#[tokio::test]
async fn fcr_lag_recovers_but_changed_high_water_persists() {
    let server = Server::new();
    let config = server.config();
    let store = journal();
    let gate = Gate::open(config.clone(), store.clone()).await.unwrap();
    gate.check().await.unwrap();
    server.state.lock().unwrap()["height"] = json!(9);
    assert!(gate.check().await.is_err());
    server.state.lock().unwrap()["height"] = json!(11);
    gate.check().await.unwrap();
    server.state.lock().unwrap()["fork"] = json!(true);
    assert!(gate.check().await.is_err());
    drop(gate);
    let gate = Gate::open(config, store).await.unwrap();
    assert!(gate.check().await.is_err());
}

#[tokio::test]
async fn fcr_positive_latch_survives_source_recovery_and_restart() {
    let server = Server::new();
    let config = server.config();
    let store = journal();
    let gate = Gate::open(config.clone(), store.clone()).await.unwrap();
    server.state.lock().unwrap()["incident"] = json!("canonical:failure");
    assert!(gate.check().await.is_err());
    server.state.lock().unwrap()["incident"] = Value::Null;
    assert!(gate.check().await.is_err());
    drop(gate);
    assert!(
        Gate::open(config, store)
            .await
            .unwrap()
            .check()
            .await
            .is_err()
    );
}

#[tokio::test]
async fn fcr_stale_checker_is_a_reversible_hold() {
    let server = Server::new();
    let config = server.config();
    let store = journal();
    let gate = Gate::open(config, store).await.unwrap();
    server.state.lock().unwrap()["stale"] = json!(true);
    assert!(gate.check().await.is_err());
    server.state.lock().unwrap()["stale"] = json!(false);
    gate.check().await.unwrap();
}

#[tokio::test]
async fn fcr_disabled_canonical_status_is_not_fresh() {
    let server = Server::new();
    let config = server.config();
    let store = journal();
    let gate = Gate::open(config, store).await.unwrap();
    server.state.lock().unwrap()["enabled"] = json!(false);
    assert!(gate.check().await.is_err());
}

#[tokio::test]
async fn fcr_malformed_generation_does_not_hide_a_positive() {
    let server = Server::new();
    let config = server.config();
    let store = journal();
    let gate = Gate::open(config, store.clone()).await.unwrap();
    {
        let mut state = server.state.lock().unwrap();
        state["malformed"] = json!(true);
        state["incident"] = json!("malformed-positive");
    }
    assert!(gate.check().await.is_err());
    assert_eq!(store.0.lock().unwrap()["incident"], "malformed-positive");
}
