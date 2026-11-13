//! Fake capture helper for the native Host tests. `PIIK_HOST_FIXTURE` selects
//! the mode; it never touches a capture device or codec. Closing stdin ends
//! any mode.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

const VP8_KEY_8X8: &[u8] = &[
    0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x08, 0x00, 0x08, 0x00, 0x00, 0x47, 0x08, 0x85, 0x85, 0x88,
    0x85, 0x84, 0x88, 0x02, 0x02, 0x00, 0x0c, 0x0d, 0x60, 0x00, 0xfe, 0xff, 0xab, 0x50, 0x80,
];
const PCM_FRAME_BYTES: usize = 960 * 2 * 2;

#[derive(Default, Clone, Copy)]
struct Frame<'a> {
    kind: u8,
    key: bool,
    layer: u8,
    timestamp: Duration,
    duration: Duration,
    width: u16,
    height: u16,
    data: &'a [u8],
}

fn write(frame: Frame<'_>) {
    let mut header = [0u8; 32];
    header[..4].copy_from_slice(b"SMED");
    header[4] = 2;
    header[5] = frame.kind;
    header[6] = u8::from(frame.key);
    header[7] = frame.layer;
    let ticks = |value: Duration| (value.as_nanos() / 100) as u64;
    header[8..16].copy_from_slice(&ticks(frame.timestamp).to_be_bytes());
    header[16..24].copy_from_slice(&ticks(frame.duration).to_be_bytes());
    header[24..26].copy_from_slice(&frame.width.to_be_bytes());
    header[26..28].copy_from_slice(&frame.height.to_be_bytes());
    header[28..32].copy_from_slice(&(frame.data.len() as u32).to_be_bytes());
    let mut stdout = std::io::stdout().lock();
    if stdout
        .write_all(&header)
        .and_then(|()| stdout.write_all(frame.data))
        .and_then(|()| stdout.flush())
        .is_err()
    {
        std::process::exit(1);
    }
}

fn status(data: &str) {
    write(Frame {
        kind: 3,
        data: data.as_bytes(),
        ..Frame::default()
    });
}

/// Per-test settings live in `fixture.env` beside this executable, read at
/// each process start, so parallel tests never share process environment.
fn env(key: &str) -> String {
    let Some(file) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("fixture.env")))
    else {
        return String::new();
    };
    let settings = std::fs::read_to_string(file).unwrap_or_default();
    settings
        .lines()
        .filter_map(|line| line.split_once('='))
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value.to_owned())
        .unwrap_or_default()
}

fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

/// Resolves once stdin reaches EOF; the reader thread discards the input.
fn stdin_watch() -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut sink = [0u8; 4096];
        let mut stdin = std::io::stdin().lock();
        while matches!(stdin.read(&mut sink), Ok(read) if read > 0) {}
        let _ = tx.send(());
    });
    rx
}

fn stopped(stop: &mpsc::Receiver<()>, wait: Duration) -> bool {
    !matches!(
        stop.recv_timeout(wait),
        Err(mpsc::RecvTimeoutError::Timeout)
    )
}

/// Waits for `path` while stdin stays open; false when stopped first.
fn wait_for(stop: &mpsc::Receiver<()>, path: &str) -> bool {
    while !exists(path) {
        if stopped(stop, Duration::from_millis(5)) {
            return false;
        }
    }
    true
}

/// `--output w h fps bitrate` groups exactly as the host requested.
fn outputs(args: &[String]) -> String {
    let mut groups = Vec::new();
    for (index, arg) in args.iter().enumerate() {
        if arg == "--output" && index + 4 < args.len() {
            groups.push(format!(
                r#"{{"width":{},"height":{},"fps":{},"bitrate":{}}}"#,
                args[index + 1],
                args[index + 2],
                args[index + 3],
                args[index + 4]
            ));
        }
    }
    format!("[{}]", groups.join(","))
}

fn argument(args: &[String], key: &str) -> u32 {
    args.iter()
        .position(|arg| arg == key)
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or_default()
}

fn backend(args: &[String]) {
    for key in ["PIIK_CAPTURE_STARTING", "PIIK_CAPTURE_ACTIVE"] {
        status(&env(key));
        if key == "PIIK_CAPTURE_STARTING"
            && let Ok(layer) = env("PIIK_CAPTURE_UNAVAILABLE").parse::<u8>()
        {
            write(Frame {
                kind: 6,
                layer,
                data: b"fixture",
                ..Frame::default()
            });
        }
    }
    let _ = args;
    let failure = env("PIIK_CAPTURE_LIVE_FAILURE");
    if !failure.is_empty() {
        if failure == "control" {
            // A final buffered frame can reach the reader before exit is joined.
            write(Frame {
                kind: 5,
                timestamp: Duration::from_secs(1),
                duration: Duration::from_secs(1) / 30,
                ..Frame::default()
            });
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::io::stderr().write_all(
            b"stage=windows-runtime\nhresult=0x887a0005\ndetail=private capture detail\n",
        );
        std::process::exit(2);
    }
    let stop = stdin_watch();
    let _ = stop.recv();
}

fn media(args: &[String]) {
    let stop = stdin_watch();
    let mode = args.get(1).map(String::as_str).unwrap_or_default();
    let microphone = mode == "--capture-microphone";
    if microphone {
        if args.len() == 4 && args[2] == "--device" && args[3] == "missing" {
            std::process::exit(2);
        }
        let marker = env("PIIK_MICROPHONE_WAIT");
        if !marker.is_empty() {
            let _ = std::fs::write(&marker, b"");
            let _ = stop.recv();
            return;
        }
    }
    let audio = microphone || mode == "--capture-audio";
    let mut device_failure = String::new();
    let marker = env("PIIK_VIDEO_DEVICE_FAILURE");
    // Only the first video process fails; a replacement uses the same fixture.
    if !audio
        && !marker.is_empty()
        && std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(format!("{marker}.owner"))
            .is_ok()
    {
        device_failure = marker;
    }
    let gate = env("PIIK_VIDEO_START_GATE");
    if audio && !microphone && !gate.is_empty() {
        if !exists(&format!("{gate}.active")) {
            let _ = std::fs::write(format!("{gate}.early-audio"), b"");
            std::process::exit(2);
        }
        let _ = std::fs::write(format!("{gate}.audio"), format!("{}:{}", args[3], args[4]));
    }
    if !audio && env("PIIK_VIDEO_FAIL") == "1" {
        let _ = std::io::stderr().write_all(
            b"stage=mft-set-output-type\nhresult=0xc00d36b4\ndetail=private capture target\n",
        );
        std::process::exit(2);
    }
    if audio && !microphone && args[2] == "exclude" {
        if env("PIIK_EXCLUSION_FAIL") == "1" {
            std::process::exit(2);
        }
        let started = env("PIIK_EXCLUSION_STARTED");
        if !started.is_empty() {
            let _ = std::fs::write(started, format!("{}:{}", args[3], args[4]));
        }
    }
    if audio && env("PIIK_AUDIO_STARTUP") == "no-ready" {
        let _ = stop.recv();
        return;
    }
    let (width, height, fps) = if audio {
        (0, 0, 50)
    } else {
        (
            argument(args, "--width"),
            argument(args, "--height"),
            argument(args, "--fps").max(1),
        )
    };
    let period = if audio {
        Duration::from_millis(20)
    } else {
        Duration::from_secs(1) / fps
    };
    let active = if audio {
        r#"{"state":"active","audio":true}"#.to_owned()
    } else {
        format!(
            r#"{{"state":"active","codec":"vp8","width":{width},"height":{height},"fps":{fps},"outputs":{}}}"#,
            outputs(args)
        )
    };
    let wait_marker = env("PIIK_AUDIO_WAIT_FOR_VIDEO");
    if audio && !wait_marker.is_empty() && !wait_for(&stop, &wait_marker) {
        return;
    }
    // Only the startup capture (source 1 at the default width) can be gated.
    if !audio && !gate.is_empty() && args.get(3).is_some_and(|id| id == "1") && width == 1280 {
        status(&format!(
            r#"{{"state":"starting","codec":"vp8","adapterIndex":0,"adapterName":"Fixture","adapterIdentity":"fixture","encoderName":"Fixture VP8","encoderIdentity":"fixture-vp8","outputs":{}}}"#,
            outputs(args)
        ));
        loop {
            if exists(&format!("{gate}.fail")) {
                let _ = std::io::stderr().write_all(b"stage=fixture-video-initialization\n");
                std::process::exit(2);
            }
            if exists(&format!("{gate}.release")) {
                break;
            }
            if stopped(&stop, Duration::from_millis(5)) {
                return;
            }
        }
    }
    if !audio && !gate.is_empty() {
        let _ = std::fs::write(format!("{gate}.active"), b"");
    }
    status(&active);
    if !audio && !wait_marker.is_empty() {
        // Exceed the OS pipe buffer before allowing audio initialization.
        let mut data = vec![0u8; 256 * 1024];
        data[..VP8_KEY_8X8.len()].copy_from_slice(VP8_KEY_8X8);
        write(Frame {
            kind: 5,
            duration: period,
            ..Frame::default()
        });
        write(Frame {
            kind: 4,
            key: true,
            layer: 1,
            duration: period,
            width: 8,
            height: 8,
            data: &data,
            ..Frame::default()
        });
        let _ = std::fs::write(&wait_marker, b"");
    }
    if audio && env("PIIK_AUDIO_STARTUP") == "silent" {
        let _ = stop.recv();
        return;
    }
    let pcm = vec![0u8; PCM_FRAME_BYTES];
    let mut pts = period;
    loop {
        if stopped(&stop, period) {
            return;
        }
        if audio {
            write(Frame {
                kind: 1,
                timestamp: pts,
                duration: period,
                data: &pcm,
                ..Frame::default()
            });
            pts += period;
            continue;
        }
        if !device_failure.is_empty() && exists(&device_failure) {
            if env("PIIK_VIDEO_DEVICE_CONTROL") == "1" {
                write(Frame {
                    kind: 5,
                    timestamp: pts,
                    duration: period,
                    ..Frame::default()
                });
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = std::io::stderr().write_all(b"stage=capture-device-lost\nhresult=0x887a0005\n");
            std::process::exit(2);
        }
        write(Frame {
            kind: 5,
            timestamp: pts,
            duration: period,
            ..Frame::default()
        });
        for layer in 0..2 {
            write(Frame {
                kind: 4,
                key: true,
                layer,
                timestamp: pts,
                duration: period,
                width: 8,
                height: 8,
                data: VP8_KEY_8X8,
            });
        }
        pts += period;
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match env("PIIK_HOST_FIXTURE").as_str() {
        "backend" => backend(&args),
        "echo" => {
            let (mut stdin, mut stdout) = (std::io::stdin().lock(), std::io::stdout().lock());
            let mut buffer = [0u8; 4096];
            while let Ok(read @ 1..) = stdin.read(&mut buffer) {
                if stdout
                    .write_all(&buffer[..read])
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    return;
                }
            }
        }
        "drain" => {
            let _ = stdin_watch().recv();
        }
        _ => media(&args),
    }
}
