//! Fake capture helper for the process tests. `PIIK_CAPTURE_STOP_FIXTURE`
//! selects the behavior; it never touches a capture device or codec.

use std::io::{Read, Write};
use std::time::Duration;

fn envelope(kind: u8, duration_100ns: u64, width: u16, height: u16, data: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(32 + data.len());
    frame.extend_from_slice(b"SMED");
    frame.extend_from_slice(&[2, kind, 0, 0]);
    frame.extend_from_slice(&0u64.to_be_bytes());
    frame.extend_from_slice(&duration_100ns.to_be_bytes());
    frame.extend_from_slice(&width.to_be_bytes());
    frame.extend_from_slice(&height.to_be_bytes());
    frame.extend_from_slice(&(data.len() as u32).to_be_bytes());
    frame.extend_from_slice(data);
    frame
}

fn read_stop() -> bool {
    let mut header = [0u8; 32];
    let mut stdin = std::io::stdin().lock();
    if stdin.read_exact(&mut header).is_err() || &header[..4] != b"SMED" || header[5] != 7 {
        return false;
    }
    let size = u32::from_be_bytes([header[28], header[29], header[30], header[31]]) as usize;
    let mut payload = vec![0; size];
    stdin.read_exact(&mut payload).is_ok() && payload == b"Q"
}

fn main() {
    let mode = std::env::var("PIIK_CAPTURE_STOP_FIXTURE").unwrap_or_default();
    match mode.as_str() {
        "startup-exit" => std::process::exit(3),
        "startup-stage" => {
            let _ = std::io::stderr().write_all(
                b"result=window-capture-failed\nstage=mft-output-timeout\nhresult=0x80004005\ndetail=private window title C:\\private\\capture.exe\n",
            );
            std::process::exit(2);
        }
        _ => {}
    }
    let mut stdout = std::io::stdout().lock();
    if mode == "audio-ready" || mode == "audio-invalid" {
        let state: &[u8] = if mode == "audio-ready" {
            br#"{"state":"active","audio":true}"#
        } else {
            br#"{"state":"active","audio":false}"#
        };
        let _ = stdout.write_all(&envelope(3, 0, 0, 0, state));
        let _ = stdout.flush();
        drop(stdout);
        std::process::exit(if read_stop() { 0 } else { 4 });
    }
    if stdout
        .write_all(&envelope(3, 0, 0, 0, b"ready"))
        .and_then(|()| stdout.flush())
        .is_err()
    {
        std::process::exit(2);
    }
    if mode == "unresponsive" {
        std::thread::sleep(Duration::from_secs(10));
        std::process::exit(3);
    }
    if mode.starts_with("blocked-output-") {
        // A producer can fill stdout before returning to its control reader.
        let frame = envelope(4, 333_333, 1280, 720, &vec![0; 1024 * 1024]);
        if stdout
            .write_all(&frame)
            .and_then(|()| stdout.flush())
            .is_err()
        {
            std::process::exit(6);
        }
    }
    drop(stdout);
    if !read_stop() {
        std::process::exit(4);
    }
    // A platform capture must get time to release its session before exit.
    std::thread::sleep(Duration::from_millis(50));
    let marker = std::env::var_os("PIIK_CAPTURE_STOP_MARKER").unwrap_or_default();
    if std::fs::write(marker, b"released").is_err() {
        std::process::exit(5);
    }
}
