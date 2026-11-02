use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::Session;

const READ_BUFFER: usize = 64 * 1024;

#[derive(Debug, Default)]
pub struct Stats {
    pub messages: AtomicU64,
    pub connections: AtomicU64,
}

pub async fn serve(listener: TcpListener, hostname: Arc<str>, stats: Arc<Stats>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        stats.connections.fetch_add(1, Ordering::Relaxed);
        let hostname = hostname.clone();
        let stats = stats.clone();
        tokio::spawn(async move {
            // A client dropping mid-session is routine for a sink, not an error.
            let _ = handle(stream, &hostname, &stats).await;
        });
    }
}

async fn handle(mut stream: TcpStream, hostname: &str, stats: &Stats) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    stream.write_all(&Session::greeting(hostname)).await?;

    let mut session = Session::new(hostname);
    let mut input = vec![0u8; READ_BUFFER];
    let mut output = Vec::with_capacity(1024);

    while !session.is_closed() {
        let read = stream.read(&mut input).await?;
        if read == 0 {
            break;
        }
        let before = session.messages;
        output.clear();
        session.feed(&input[..read], &mut output);
        stats
            .messages
            .fetch_add(session.messages - before, Ordering::Relaxed);
        if !output.is_empty() {
            stream.write_all(&output).await?;
        }
    }
    Ok(())
}
