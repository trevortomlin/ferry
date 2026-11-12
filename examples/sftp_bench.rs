//! Protocol-level benchmark: list a big directory, download and upload a big
//! file, against a real SFTP server. Reports wall time, CPU and peak memory.
//!
//!   cargo run --release --example sftp_bench -- HOST:PORT:USER:KEY BIG_DIR BIG_FILE

use std::time::Instant;

use ferry::model::connection::{Auth, Connection, ConnectionId, Protocol};
use ferry::remote::{Collect, Ignore, Provider};
use ferry::secrets::Credentials;

fn cpu() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let t = |v: libc::timeval| v.tv_sec as f64 + v.tv_usec as f64 / 1e6;
    t(u.ru_utime) + t(u.ru_stime)
}

fn peak_mb() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    u.ru_maxrss as f64 / 1024.
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parts: Vec<&str> = args[0].split(':').collect();
    let (dir, file) = (args[1].clone(), args[2].clone());
    let mut c = Connection::new(ConnectionId(1), Protocol::Sftp);
    c.host = parts[0].into();
    c.port = parts[1].parse().unwrap();
    c.username = parts[2].into();
    c.auth = Auth::KeyFile {
        path: parts[3].into(),
    };
    let creds = Credentials::SshKeyFile {
        username: c.username.clone(),
        path: parts[3].into(),
        passphrase: None,
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut p = Provider::connect(&c, creds).await.expect("connect");
        for round in 0..2 {
            let (t, c0) = (Instant::now(), cpu());
            let mut sink = Collect::default();
            p.list(&dir, &mut sink).await.expect("list");
            println!(
                "list      {:>7} entries  {:>7.1} ms  cpu {:>6.1} ms{}",
                sink.entries.len(),
                t.elapsed().as_secs_f64() * 1e3,
                (cpu() - c0) * 1e3,
                if round == 0 { "  (warm-up)" } else { "" }
            );
        }
        let tmp = std::env::temp_dir().join(format!("ferry-bench-{}", std::process::id()));
        let size = p.size_of(&file).await.unwrap();
        let (t, c0) = (Instant::now(), cpu());
        p.download(&file, &tmp, 0, &mut Ignore)
            .await
            .expect("download");
        let secs = t.elapsed().as_secs_f64();
        println!(
            "download  {:>7.0} MB      {:>7.1} ms  cpu {:>6.1} ms  {:>6.0} MB/s",
            size as f64 / 1048576.,
            secs * 1e3,
            (cpu() - c0) * 1e3,
            size as f64 / 1048576. / secs
        );
        let remote = format!("{file}.uploaded");
        let (t, c0) = (Instant::now(), cpu());
        p.upload(&tmp, &remote, 0, &mut Ignore)
            .await
            .expect("upload");
        let secs = t.elapsed().as_secs_f64();
        println!(
            "upload    {:>7.0} MB      {:>7.1} ms  cpu {:>6.1} ms  {:>6.0} MB/s",
            size as f64 / 1048576.,
            secs * 1e3,
            (cpu() - c0) * 1e3,
            size as f64 / 1048576. / secs
        );
        // Before the comparisons below, which read both files into memory.
        println!("peak rss  {:.1} MB  (through both transfers)", peak_mb());
        let same = match (std::fs::read(&remote), std::fs::read(&tmp)) {
            (Ok(a), Ok(b)) => a == b,
            _ => p.size_of(&remote).await.unwrap() == size,
        };
        // When the server is this machine, the file is right here to compare.
        if let Ok(original) = std::fs::read(&file) {
            println!(
                "download byte-identical: {}",
                std::fs::read(&tmp).unwrap() == original
            );
        }
        let _ = p.remove(&remote, false).await;
        let _ = std::fs::remove_file(&tmp);
        println!("round trip intact: {same}");
    });
}
