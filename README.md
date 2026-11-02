# rsmtp-sink

A small SMTP test sink in Rust, modelled on Postfix's `smtp-sink`. It speaks SMTP, accepts mail and throws it away. People use tools like this to load-test mail clients and relays.

**This is not a mail server.** It doesn't store, queue or deliver mail, and it has no TLS or authentication.

## Is it faster?

Yes, in this benchmark. Against Apple's build of Postfix 3.2.2 `smtp-sink`, it used about 1.8–3.5x less CPU per message and handled about 2.3–4.1x more messages per second.

That does **not** show that Rust is faster than C, or that a Rust mail server would beat Postfix. See [What this doesn't show](#what-this-doesnt-show).

| Scenario | Postfix msg/s | Rust msg/s | Throughput | Postfix CPU/msg | Rust CPU/msg | CPU saving |
|---|---:|---:|---:|---:|---:|---:|
| 1KB, new connection each message | 5,949 | 13,399 | 2.3x | 87.5 µs | 47.8 µs | 1.8x |
| 1KB, reused connections | 8,928 | 27,520 | 3.1x | 56.2 µs | 29.7 µs | 1.9x |
| 1MB, reused connections | 282 | 978 | 3.5x | 3,469 µs | 987 µs | 3.5x |
| 1KB, reused, 6 client processes | 8,761 | 36,012 | 4.1x | 59.8 µs | 27.1 µs | 2.2x |

Figures are medians of 7 rounds. Both servers ran on one thread. Peak memory was about 2–4 MB for Rust and 3–11 MB for Postfix. All runs finished without client errors, and the Rust server's message count matched what was sent. The Postfix sink's count wasn't checked.

**CPU per message is the number to trust.** For small messages Postfix's CPU use works out to only about half a core, so it wasn't saturated, and the throughput gap may partly reflect waiting rather than work. I didn't find out why.

Four threads (`--threads 4`) only clearly helped with 1MB messages (2,718 msg/s). With small messages they used more CPU and were no faster, or slower. Full data is in [`results/results.json`](results/results.json).

## What this doesn't show

- **Rust vs C:** Apple's compiler flags are unknown, and the Postfix version is old. A build of current Postfix with `-O3` could narrow the gap. I tried to run that comparison, but the environment blocked compiling downloaded source, so **it's still untested**.
- **Why it's faster:** the design of each program differs, not just the language. A 5-second profile of `smtp-sink` spent most samples in system calls (`poll`, `write`, `read`, `sigprocmask`), which points towards I/O design. My first guess was read size, but that turned out wrong: shrinking Rust's 64KB read buffer to 4KB barely changed anything (978 to 916 msg/s at 1MB, 31,473 to 31,062 at 1KB). The cause is unconfirmed.
- **Real mail servers:** a sink discards mail. A real server's time goes largely on writing to disk safely, DNS, TLS and filtering, and none of that is measured here.
- **Real networks:** everything ran over loopback on one laptop (Apple M5 Pro, 18 cores), so the load generator and server shared the machine. It wasn't idle either (load average about 5), and some rounds were noticeably slower, as the ranges in the results show.

## Differences from Postfix `smtp-sink`

This is a new implementation written from observed behaviour, not a line-by-line translation of the Postfix code. It covers the core protocol only, with none of `smtp-sink`'s fault-injection or dump options.

`bench/compat.py` sends 12 conversations to both servers, and 9 get identical reply codes. In the other 3, this server is stricter or more accurate:

| Conversation | Postfix | rsmtp-sink |
|---|---|---|
| `VRFY someone` | 250 | 252 |
| `MAIL` before `HELO`/`EHLO` | 250 | 503 |
| `MAIL` with no argument | 250 | 501 |

## Usage

```bash
cargo build --release
```

```bash
./target/release/rsmtp-sink 127.0.0.1:2525 1024
```

The arguments are the listen address and connection backlog. Optional flags are `--threads N` and `--hostname NAME`. On Ctrl-C it prints connection and message counts.

## Reproducing the results

This needs macOS, which includes the Postfix tools at `/usr/libexec/postfix`. On Linux, install Postfix and change the paths in the scripts.

```bash
cargo test
```

```bash
python3 bench/compat.py
```

```bash
python3 bench/bench.py --rounds 7
```

## License

MIT
