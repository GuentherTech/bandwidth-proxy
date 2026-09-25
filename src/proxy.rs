use std::{
    collections::VecDeque,
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex as AsyncMutex, watch},
    task::JoinSet,
    time::{Instant, sleep, sleep_until, timeout},
};

const MAX_CONNECTIONS: usize = 256;
const MAX_EVENTS: usize = 100;
const NETWORK_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CHUNK: usize = 4096;
const MIN_CHUNK: usize = 64;
// Timer wake-ups can run late (about 15.6 ms on Windows). Later chunks may start
// up to this far in the past to pay that time back, so it must exceed the tick.
const CATCH_UP: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Download,
    Upload,
}

impl Direction {
    pub const ALL: [Self; 2] = [Self::Download, Self::Upload];

    pub fn label(self) -> &'static str {
        match self {
            Self::Download => "Download",
            Self::Upload => "Upload",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PerDirection<T> {
    pub download: T,
    pub upload: T,
}

impl<T> PerDirection<T> {
    pub fn get(&self, direction: Direction) -> &T {
        match direction {
            Direction::Download => &self.download,
            Direction::Upload => &self.upload,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub enabled: bool,
    pub bytes_per_second: PerDirection<u64>,
}

impl Limits {
    fn rate(&self, direction: Direction) -> Option<u64> {
        self.enabled
            .then(|| (*self.bytes_per_second.get(direction)).max(1))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub at: std::time::Instant,
    pub level: Level,
    pub text: String,
}

#[derive(Default)]
pub struct Stats {
    pub transferred: PerDirection<AtomicU64>,
    pub connections: AtomicUsize,
    pub listening: AtomicBool,
    status: Mutex<Option<Event>>,
    events: Mutex<VecDeque<Event>>,
}

// A panic while holding one of these locks leaves only plain data behind, so a
// poisoned lock is still safe to read and write.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Stats {
    /// Replaces the status line and also records it as an event.
    pub fn set_status(&self, level: Level, text: impl Into<String>) {
        let event = self.event(level, text);
        *lock(&self.status) = Some(event);
    }

    pub fn status(&self) -> Option<Event> {
        lock(&self.status).clone()
    }

    /// Copies the event history so the caller does not hold the lock while drawing.
    pub fn events(&self) -> Vec<Event> {
        lock(&self.events).iter().cloned().collect()
    }

    pub fn event(&self, level: Level, text: impl Into<String>) -> Event {
        let event = Event {
            at: std::time::Instant::now(),
            level,
            text: text.into(),
        };
        let mut events = lock(&self.events);
        if events.len() == MAX_EVENTS {
            events.pop_front();
        }
        events.push_back(event.clone());
        event
    }
}

// One gate per direction, shared by all connections, so connection pooling
// cannot multiply the configured bandwidth allowance.
#[derive(Default)]
struct Gate {
    next_free: AsyncMutex<Option<Instant>>,
}

impl Gate {
    async fn wait(&self, bytes: usize, direction: Direction, limits: &mut watch::Receiver<Limits>) {
        loop {
            if limits.borrow_and_update().rate(direction).is_none() {
                return;
            }
            tokio::select! {
                changed = limits.changed() => {
                    if changed.is_err() { return; }
                }
                mut next_free = self.next_free.lock() => {
                    let now = Instant::now();
                    let earliest = now.checked_sub(CATCH_UP).unwrap_or(now);
                    let start = next_free.map_or(now, |free| free.max(earliest));
                    loop {
                        let Some(rate) = limits.borrow_and_update().rate(direction) else {
                            *next_free = None;
                            return;
                        };
                        let deadline =
                            start + Duration::from_secs_f64(bytes as f64 / rate as f64);
                        tokio::select! {
                            () = sleep_until(deadline) => {
                                *next_free = Some(deadline);
                                return;
                            }
                            changed = limits.changed() => {
                                if changed.is_err() { return; }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Default)]
struct Gates {
    download: Gate,
    upload: Gate,
}

/// About 100 ms of traffic at `rate` bytes per second, so slow links trickle
/// instead of bursting, and one connection's chunk cannot hold the shared gate for long.
fn chunk_size(rate: Option<u64>) -> usize {
    rate.map_or(MAX_CHUNK, |rate| {
        usize::try_from(rate / 10).map_or(MAX_CHUNK, |size| size.clamp(MIN_CHUNK, MAX_CHUNK))
    })
}

async fn within<T>(future: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    timeout(NETWORK_TIMEOUT, future).await.unwrap_or_else(|_| {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("timed out after {} s", NETWORK_TIMEOUT.as_secs()),
        ))
    })
}

/// The destination as the user typed it, plus every address it resolved to.
struct Destination {
    name: String,
    addresses: Vec<SocketAddr>,
}

impl Destination {
    fn describe(&self) -> String {
        let addresses: Vec<_> = self.addresses.iter().map(ToString::to_string).collect();
        format!("{} ({})", self.name, addresses.join(", "))
    }
}

async fn pump<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut reader: R,
    mut writer: W,
    gate: &Gate,
    mut limits: watch::Receiver<Limits>,
    stats: &Stats,
    direction: Direction,
) -> io::Result<()> {
    let mut buffer = [0_u8; MAX_CHUNK];
    loop {
        let size = chunk_size(limits.borrow().rate(direction));
        let count = reader.read(&mut buffer[..size]).await?;
        if count == 0 {
            return writer.shutdown().await;
        }
        gate.wait(count, direction, &mut limits).await;
        writer.write_all(&buffer[..count]).await?;
        stats
            .transferred
            .get(direction)
            .fetch_add(count as u64, Ordering::Relaxed);
    }
}

struct ConnectionCount(Arc<Stats>);

impl Drop for ConnectionCount {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn relay(
    client: TcpStream,
    target: Arc<Destination>,
    gates: Arc<Gates>,
    limits: watch::Receiver<Limits>,
    stats: Arc<Stats>,
) -> io::Result<()> {
    stats.connections.fetch_add(1, Ordering::Relaxed);
    let _count = ConnectionCount(stats.clone());
    let upstream = within(TcpStream::connect(&target.addresses[..]))
        .await
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Could not reach {}: {error}", target.describe()),
            )
        })?;
    client.set_nodelay(true)?;
    upstream.set_nodelay(true)?;
    let (client_read, client_write) = client.into_split();
    let (server_read, server_write) = upstream.into_split();
    tokio::try_join!(
        pump(
            client_read,
            server_write,
            &gates.upload,
            limits.clone(),
            &stats,
            Direction::Upload,
        ),
        pump(
            server_read,
            client_write,
            &gates.download,
            limits,
            &stats,
            Direction::Download,
        ),
    )?;
    Ok(())
}

fn is_disconnect(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::UnexpectedEof
    )
}

pub async fn serve(
    port: u16,
    target: String,
    limits: watch::Receiver<Limits>,
    mut shutdown: watch::Receiver<bool>,
    stats: Arc<Stats>,
) -> io::Result<()> {
    // Never expose a database forwarder to other machines.
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    let addresses: Vec<SocketAddr> = tokio::select! {
        _ = shutdown.changed() => {
            stats.set_status(Level::Info, "Proxy stopped.");
            return Ok(());
        }
        resolved = within(tokio::net::lookup_host(&target)) => resolved
            .map_err(|error| {
                io::Error::new(error.kind(), format!("Could not resolve {target}: {error}"))
            })?
            .collect(),
    };
    if addresses.is_empty() || addresses.contains(&listener.local_addr()?) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{target} resolves to no address or to the proxy itself"),
        ));
    }
    let target = Arc::new(Destination {
        name: target,
        addresses,
    });
    stats.set_status(
        Level::Info,
        format!("Listening on {}", listener.local_addr()?),
    );
    stats.listening.store(true, Ordering::Relaxed);
    let gates = Arc::new(Gates::default());
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => match accepted {
                Ok((client, _)) if connections.len() >= MAX_CONNECTIONS => {
                    stats.event(Level::Error, format!(
                        "Connection limit reached ({MAX_CONNECTIONS}). New client disconnected."
                    ));
                    drop(client);
                }
                Ok((client, _)) => {
                    let relay = relay(
                        client,
                        target.clone(),
                        gates.clone(),
                        limits.clone(),
                        stats.clone(),
                    );
                    connections.spawn(relay);
                }
                Err(error) => {
                    stats.event(Level::Error, format!("Could not accept a client: {error}"));
                    sleep(Duration::from_millis(100)).await;
                }
            },
            Some(result) = connections.join_next(), if !connections.is_empty() => match result {
                Ok(Err(error)) if !is_disconnect(&error) => {
                    stats.event(Level::Error, format!("Connection error: {error}"));
                }
                Err(error) => {
                    stats.event(Level::Error, format!("Relay task failed: {error}"));
                }
                Ok(_) => {}
            },
        }
    }
    stats.listening.store(false, Ordering::Relaxed);
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    stats.set_status(
        Level::Info,
        "Proxy stopped. Existing connections were closed.",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(enabled: bool, download: u64, upload: u64) -> Limits {
        Limits {
            enabled,
            bytes_per_second: PerDirection { download, upload },
        }
    }

    fn destination(address: SocketAddr) -> Arc<Destination> {
        Arc::new(Destination {
            name: address.to_string(),
            addresses: vec![address],
        })
    }

    #[tokio::test]
    async fn tcp_relay_returns_response_after_client_half_close() {
        timeout(Duration::from_secs(5), async {
            let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let target = destination(upstream.local_addr().unwrap());
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut client = TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let (accepted, _) = listener.accept().await.unwrap();
            let (_sender, receiver) = watch::channel(limits(false, 1, 1));
            let stats = Arc::new(Stats::default());
            let worker = tokio::spawn(relay(
                accepted,
                target,
                Arc::new(Gates::default()),
                receiver,
                stats.clone(),
            ));
            let server = tokio::spawn(async move {
                let (mut connection, _) = upstream.accept().await.unwrap();
                let mut request = Vec::new();
                connection.read_to_end(&mut request).await.unwrap();
                assert_eq!(request, b"request");
                connection.write_all(b"response").await.unwrap();
                connection.shutdown().await.unwrap();
            });
            client.write_all(b"request").await.unwrap();
            client.shutdown().await.unwrap();
            let mut response = Vec::new();
            client.read_to_end(&mut response).await.unwrap();
            assert_eq!(response, b"response");
            server.await.unwrap();
            worker.await.unwrap().unwrap();
            assert_eq!(stats.connections.load(Ordering::Relaxed), 0);
            assert_eq!(stats.transferred.upload.load(Ordering::Relaxed), 7);
            assert_eq!(stats.transferred.download.load(Ordering::Relaxed), 8);
        })
        .await
        .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn upload_uses_its_own_rate() {
        let (_sender, mut receiver) = watch::channel(limits(true, 1, 8192));
        let gate = Gate::default();
        let start = Instant::now();
        gate.wait(4096, Direction::Upload, &mut receiver).await;
        assert_eq!(start.elapsed(), Duration::from_millis(500));
    }

    #[tokio::test(start_paused = true)]
    async fn cap_is_shared_across_connections() {
        let (_sender, receiver) = watch::channel(limits(true, 4096, 8192));
        let gate = Gate::default();
        let start = Instant::now();
        let mut first = receiver.clone();
        let mut second = receiver;
        tokio::join!(
            gate.wait(4096, Direction::Download, &mut first),
            gate.wait(4096, Direction::Download, &mut second)
        );
        assert_eq!(start.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn disabling_limit_releases_pending_traffic() {
        let (sender, mut receiver) = watch::channel(limits(true, 1, 1));
        let gate = Arc::new(Gate::default());
        let start = Instant::now();
        let task = tokio::spawn(async move {
            gate.wait(4096, Direction::Download, &mut receiver).await;
        });
        tokio::task::yield_now().await;
        sender.send(limits(false, 1, 1)).unwrap();
        task.await.unwrap();
        assert_eq!(start.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn changing_rate_releases_a_slow_pending_transfer() {
        let (sender, mut receiver) = watch::channel(limits(true, 1, 1));
        let gate = Arc::new(Gate::default());
        let start = Instant::now();
        let task = tokio::spawn(async move {
            gate.wait(4096, Direction::Download, &mut receiver).await;
        });
        tokio::task::yield_now().await;
        sender.send(limits(true, 4096, 1)).unwrap();
        task.await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn changing_rate_keeps_time_already_waited() {
        let (sender, mut receiver) = watch::channel(limits(true, 1024, 1));
        let gate = Arc::new(Gate::default());
        let start = Instant::now();
        let task = tokio::spawn(async move {
            gate.wait(4096, Direction::Download, &mut receiver).await;
        });
        sleep(Duration::from_secs(1)).await;
        sender.send(limits(true, 2048, 1)).unwrap();
        task.await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn late_wake_ups_do_not_accumulate() {
        let (_sender, mut receiver) = watch::channel(limits(true, 4096, 1));
        let gate = Gate::default();
        let start = Instant::now();
        for _ in 0..4 {
            gate.wait(4096, Direction::Download, &mut receiver).await;
            sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(start.elapsed(), Duration::from_millis(4020));
    }

    #[tokio::test(start_paused = true)]
    async fn sustained_concurrent_transfers_share_one_budget() {
        let (_sender, receiver) = watch::channel(limits(true, 8192, 4096));
        let gate = Arc::new(Gate::default());
        let start = Instant::now();
        let mut tasks = JoinSet::new();
        for _ in 0..4 {
            let gate = gate.clone();
            let mut receiver = receiver.clone();
            tasks.spawn(async move {
                for _ in 0..8 {
                    gate.wait(4096, Direction::Download, &mut receiver).await;
                }
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
        assert_eq!(start.elapsed(), Duration::from_secs(16));
    }

    #[tokio::test]
    async fn sustained_rate_holds_with_real_timers() {
        let (_sender, mut receiver) = watch::channel(limits(true, 1024 * 1024, 1));
        let gate = Gate::default();
        let start = Instant::now();
        for _ in 0..64 {
            gate.wait(4096, Direction::Download, &mut receiver).await;
        }
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(250), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(400), "{elapsed:?}");
    }

    #[tokio::test]
    async fn relay_holds_the_cap_over_real_sockets() {
        const BYTES: usize = 512 * 1024;
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = destination(upstream.local_addr().unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (accepted, _) = listener.accept().await.unwrap();
        let (_sender, receiver) = watch::channel(limits(true, 2 * 1024 * 1024, 1024 * 1024));
        let stats = Arc::new(Stats::default());
        let worker = tokio::spawn(relay(
            accepted,
            target,
            Arc::new(Gates::default()),
            receiver,
            stats.clone(),
        ));
        let server = tokio::spawn(async move {
            let (mut connection, _) = upstream.accept().await.unwrap();
            connection.write_all(&vec![7; BYTES]).await.unwrap();
            connection.shutdown().await.unwrap();
        });
        let start = Instant::now();
        let mut received = Vec::new();
        client.read_to_end(&mut received).await.unwrap();
        let elapsed = start.elapsed();
        assert_eq!(received.len(), BYTES);
        assert!(elapsed >= Duration::from_millis(240), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(450), "{elapsed:?}");
        client.shutdown().await.unwrap();
        server.await.unwrap();
        worker.await.unwrap().unwrap();
    }

    #[test]
    fn chunks_hold_about_a_tenth_of_a_second() {
        assert_eq!(chunk_size(None), MAX_CHUNK);
        assert_eq!(chunk_size(Some(1024)), 102);
        assert_eq!(chunk_size(Some(1)), MIN_CHUNK);
        assert_eq!(chunk_size(Some(1024 * 1024)), MAX_CHUNK);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_rates_trickle_instead_of_bursting() {
        let (_sender, receiver) = watch::channel(limits(true, 1024, 1024));
        let (mut input, reader) = tokio::io::duplex(8192);
        let (writer, mut output) = tokio::io::duplex(8192);
        input.write_all(&[7; 4096]).await.unwrap();
        let stats = Stats::default();
        let gate = Gate::default();
        let start = Instant::now();
        let mut buffer = [0; 4096];
        tokio::select! {
            _ = pump(reader, writer, &gate, receiver, &stats, Direction::Download) => {
                unreachable!("the input stays open");
            }
            read = output.read(&mut buffer) => {
                assert_eq!(read.unwrap(), 102);
                assert!(start.elapsed() < Duration::from_millis(200));
            }
        }
    }

    #[test]
    fn event_history_is_bounded() {
        let stats = Stats::default();
        for index in 0..150 {
            stats.event(Level::Info, format!("Event {index}"));
        }
        let events = stats.events.lock().unwrap();
        assert_eq!(events.len(), MAX_EVENTS);
        assert_eq!(events.front().unwrap().text, "Event 50");
        assert_eq!(events.back().unwrap().text, "Event 149");
    }

    #[tokio::test]
    async fn forwards_bytes_and_preserves_half_close() {
        let (_sender, receiver) = watch::channel(limits(false, 1, 1));
        let (mut input, reader) = tokio::io::duplex(32);
        let (writer, mut output) = tokio::io::duplex(32);
        let stats = Stats::default();
        let gate = Gate::default();
        let send = async {
            input.write_all(b"test payload").await.unwrap();
            input.shutdown().await.unwrap();
        };
        let mut received = Vec::new();
        let (pumped, (), read) = tokio::join!(
            pump(reader, writer, &gate, receiver, &stats, Direction::Download),
            send,
            output.read_to_end(&mut received),
        );
        pumped.unwrap();
        read.unwrap();
        assert_eq!(received, b"test payload");
        assert_eq!(stats.transferred.download.load(Ordering::Relaxed), 12);
    }
}
