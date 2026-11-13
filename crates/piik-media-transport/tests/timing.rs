use std::time::{Duration, Instant};

use piik_media_transport::{ConnEvent, ConnId, Connection, Connections, Shard, ShardApp, spawn};
use tokio::sync::oneshot;

struct App;

impl ShardApp for App {
    type Command = oneshot::Sender<()>;

    fn on_command(&mut self, now: Instant, ready: Self::Command, conns: &mut Connections) {
        conns.insert(Connection::new(str0m::Rtc::builder(), now));
        let _ = ready.send(());
    }

    fn on_event(&mut self, _: Instant, _: ConnId, _: ConnEvent, _: &mut Connections) {}
}

#[tokio::test]
async fn active_shards_keep_millisecond_timer_resolution() {
    let (first, first_task) = spawn(Shard::new(App));
    let (second, second_task) = spawn(Shard::new(App));
    for handle in [&first, &second] {
        let (ready, received) = oneshot::channel();
        handle.commands.send(ready).await.unwrap();
        received.await.unwrap();
    }
    // Retiring one owner must not withdraw the surviving shard's request.
    drop(first);
    first_task.await.unwrap();
    let start = Instant::now();
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let elapsed = start.elapsed();
    drop(second);
    second_task.await.unwrap();
    assert!(
        elapsed < Duration::from_millis(400),
        "millisecond media deadlines were quantized to the Windows default: {elapsed:?}"
    );
}
