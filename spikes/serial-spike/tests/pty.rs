//! SM-1 behaviours exercised on Linux pseudo-terminals.
//! The PTY slave stands in for the radio's serial device; the master side is
//! "the radio". Closing the master emulates an unplugged USB radio. A symlink
//! in a temp dir emulates the stable `/dev/serial/by-id/...` path.
//!
//! Linux only: on macOS `serialport` sets the baud rate with the
//! `IOSSIOSPEED` ioctl, which pseudo-terminals reject (ENOTTY, "Not a
//! typewriter"; seen in CI on macos-14). Real USB CDC radios accept it, so the
//! macOS serial path needs a real-device test instead.
#![cfg(target_os = "linux")]

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::pty::openpty;
use nix::unistd::ttyname;

struct FakeRadio {
    master: File,
    slave_path: PathBuf,
    _slave: OwnedFd, // keep the pty alive until the master closes
}

fn fake_radio() -> FakeRadio {
    let pty = openpty(None, None).expect("openpty");
    let slave_path = ttyname(pty.slave.as_fd()).expect("ttyname");
    FakeRadio {
        master: File::from(pty.master),
        slave_path,
        _slave: pty.slave,
    }
}

fn master_readable(m: &File, ms: u16) -> bool {
    let mut fds = [PollFd::new(m.as_fd(), PollFlags::POLLIN)];
    poll(&mut fds, PollTimeout::from(ms)).expect("poll") > 0
}

fn open_port(path: &Path) -> serialport::Result<Box<dyn serialport::SerialPort>> {
    serialport::new(path.to_string_lossy(), 9600)
        .timeout(Duration::from_millis(100))
        .flow_control(serialport::FlowControl::None)
        .open()
}

#[test]
fn open_writes_nothing_to_the_radio() {
    let radio = fake_radio();
    let _port = open_port(&radio.slave_path).expect("open");
    // I1.2: no bytes are written outside Ready, and opening is not a write.
    assert!(
        !master_readable(&radio.master, 300),
        "opening the port emitted bytes"
    );
}

#[test]
fn second_open_is_refused_while_held() {
    // I1.1: exclusive ownership. serialport sets TIOCEXCL on Unix by default.
    let radio = fake_radio();
    let _first = open_port(&radio.slave_path).expect("first open");
    let second = open_port(&radio.slave_path);
    assert!(
        second.is_err(),
        "a second process/handle could open the radio port"
    );
}

#[test]
fn loopback_is_binary_transparent_including_flow_control_bytes() {
    // N2.4 / DR#253: 0x11 and 0x13 must never be swallowed by our side.
    let mut radio = fake_radio();
    let mut port = open_port(&radio.slave_path).expect("open");
    let all: Vec<u8> = (0..=255u8).cycle().take(4096).collect();

    port.write_all(&all).unwrap();
    port.flush().unwrap();
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while got.len() < all.len() && Instant::now() < deadline {
        if master_readable(&radio.master, 200) {
            let mut b = [0u8; 4096];
            let n = radio.master.read(&mut b).unwrap();
            got.extend_from_slice(&b[..n]);
        }
    }
    assert_eq!(got, all, "port -> radio corrupted bytes");

    radio.master.write_all(&all).unwrap();
    let mut back = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while back.len() < all.len() && Instant::now() < deadline {
        let mut b = [0u8; 4096];
        match port.read(&mut b) {
            Ok(n) => back.extend_from_slice(&b[..n]),
            Err(e) if e.kind() == ErrorKind::TimedOut => {}
            Err(e) => panic!("read: {e}"),
        }
    }
    assert_eq!(back, all, "radio -> port corrupted bytes");
}

#[test]
fn unplug_is_detected_and_replug_reopens_by_stable_path() {
    // SM-1: Ready --IoError/Eof/Hangup--> Lost --DeviceAppeared(same id)--> Opening.
    let dir = std::env::temp_dir().join(format!("aether-spike-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let by_id = dir.join("usb-Icom_ID-52PLUS_TEST-if00");

    let radio = fake_radio();
    let _ = std::fs::remove_file(&by_id);
    std::os::unix::fs::symlink(&radio.slave_path, &by_id).unwrap();
    let mut port = open_port(&by_id).expect("open via stable path");

    // Unplug: the radio side disappears.
    drop(radio);
    let t0 = Instant::now();
    let mut lost = None;
    while t0.elapsed() < Duration::from_secs(3) {
        let mut b = [0u8; 64];
        match port.read(&mut b) {
            Ok(0) => {
                lost = Some("eof");
                break;
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::TimedOut => {}
            Err(e) => {
                lost = Some(if e.raw_os_error() == Some(5) {
                    "EIO"
                } else {
                    "io error"
                });
                break;
            }
        }
    }
    let detect = t0.elapsed();
    assert!(lost.is_some(), "unplug not detected within 3 s");
    println!("unplug detected as {} after {:?}", lost.unwrap(), detect);
    drop(port);

    // Replug: a new device node appears; the stable path is re-pointed.
    let radio2 = fake_radio();
    std::fs::remove_file(&by_id).unwrap();
    std::os::unix::fs::symlink(&radio2.slave_path, &by_id).unwrap();
    let mut port2 = open_port(&by_id).expect("reopen via stable path after replug");
    port2.write_all(b"[SOB]x[EOB]").unwrap();
    assert!(
        master_readable(&radio2.master, 1000),
        "no data after replug"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn loopback_throughput_far_exceeds_dv_rates() {
    // The serial path must not be the bottleneck at Fast Data (~435 B/s).
    // Legacy D-RATS writes 8 bytes then flushes and polls (comm.py:439).
    let mut radio = fake_radio();
    let mut port = open_port(&radio.slave_path).expect("open");
    let data = vec![0x55u8; 64 * 1024];
    let t0 = Instant::now();
    let reader = std::thread::spawn(move || {
        let mut n = 0;
        let mut b = [0u8; 8192];
        while n < 64 * 1024 {
            n += radio.master.read(&mut b).unwrap();
        }
        n
    });
    for chunk in data.chunks(512) {
        port.write_all(chunk).unwrap();
    }
    port.flush().unwrap();
    let n = reader.join().unwrap();
    let rate = n as f64 / t0.elapsed().as_secs_f64();
    println!(
        "pty loopback: {n} B in {:?} = {:.0} B/s",
        t0.elapsed(),
        rate
    );
    assert!(rate > 10_000.0);
}
