use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use rsmtp_sink::server::{serve, Stats};
use tokio::net::TcpSocket;

const USAGE: &str = "usage: rsmtp-sink [--threads N] [--hostname NAME] host:port backlog";

struct Options {
    threads: usize,
    hostname: String,
    address: String,
    backlog: u32,
}

fn parse_args() -> Result<Options, String> {
    let mut threads = 1;
    let mut hostname = String::from("rsmtp-sink");
    let mut positional = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--threads" => {
                threads = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|&n| n > 0)
                    .ok_or("--threads needs a positive number")?;
            }
            "--hostname" => hostname = args.next().ok_or("--hostname needs a value")?,
            _ => positional.push(arg),
        }
    }

    let [address, backlog] = <[String; 2]>::try_from(positional).map_err(|_| USAGE)?;
    let backlog = backlog.parse().map_err(|_| "backlog must be a number")?;
    Ok(Options { threads, hostname, address, backlog })
}

fn main() -> ExitCode {
    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    // One thread by default, matching Postfix smtp-sink's single event loop.
    let runtime = if options.threads == 1 {
        tokio::runtime::Builder::new_current_thread()
    } else {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.worker_threads(options.threads);
        builder
    }
    .enable_io()
    .build();

    let runtime = match runtime {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("cannot start runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run(options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(options: Options) -> std::io::Result<()> {
    let address = tokio::net::lookup_host(&options.address)
        .await?
        .next()
        .ok_or_else(|| std::io::Error::other("address did not resolve"))?;

    let socket = if address.is_ipv4() { TcpSocket::new_v4()? } else { TcpSocket::new_v6()? };
    socket.set_reuseaddr(true)?;
    socket.bind(address)?;
    let listener = socket.listen(options.backlog)?;

    let stats = Arc::new(Stats::default());
    let hostname: Arc<str> = options.hostname.into();

    tokio::select! {
        () = serve(listener, hostname, stats.clone()) => {}
        result = tokio::signal::ctrl_c() => result?,
    }

    eprintln!(
        "connections={} messages={}",
        stats.connections.load(Ordering::Relaxed),
        stats.messages.load(Ordering::Relaxed)
    );
    Ok(())
}
