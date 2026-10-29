//! Phase B serial spike (SM-1). Throwaway code; findings go into
//! `docs/phase-b/README.md` and the real `aether-radio` crate.
//!
//! Commands are deliberately **receive-only**: nothing here ever writes to a
//! radio, so it is safe to point at a real IC-9700 / ID-52 PLUS for hardware
//! test HW-1 (passive receive).
//!
//! ```text
//! serial-spike list                      # enumerate ports; opens nothing
//! serial-spike listen <port> [baud]      # exclusive open, hex-dump RX with timestamps,
//!                                        # report unplug and wait for replug
//! ```

use std::io::{ErrorKind, Read};
use std::time::{Duration, Instant};

fn list() {
    match serialport::available_ports() {
        Ok(ports) if ports.is_empty() => println!("no serial ports found"),
        Ok(ports) => {
            for p in ports {
                let detail = match &p.port_type {
                    serialport::SerialPortType::UsbPort(u) => format!(
                        "USB {:04x}:{:04x} serial={} maker={} product={}",
                        u.vid,
                        u.pid,
                        u.serial_number.as_deref().unwrap_or("-"),
                        u.manufacturer.as_deref().unwrap_or("-"),
                        u.product.as_deref().unwrap_or("-")
                    ),
                    serialport::SerialPortType::BluetoothPort => "Bluetooth".into(),
                    serialport::SerialPortType::PciPort => "PCI".into(),
                    serialport::SerialPortType::Unknown => "unknown".into(),
                };
                println!("{:<28} {}", p.port_name, detail);
            }
        }
        Err(e) => eprintln!("enumeration failed: {e}"),
    }
}

fn hexline(t: f64, data: &[u8]) {
    let hex: Vec<String> = data.iter().map(|b| format!("{b:02x}")).collect();
    let txt: String = data
        .iter()
        .map(|&b| {
            if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '.'
            }
        })
        .collect();
    println!("{t:10.3}  {:<48}  {txt}", hex.join(" "));
}

fn listen(path: &str, baud: u32) -> ! {
    let start = Instant::now();
    loop {
        let port = serialport::new(path, baud)
            .timeout(Duration::from_millis(200))
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None) // never let the driver swallow 0x11/0x13
            .open();
        let mut port = match port {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "{:10.3}  open {path} failed: {e}; retrying in 2 s",
                    start.elapsed().as_secs_f64()
                );
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        // The TH-D75 stays silent without DTR (macrats); TM-D750 untested.
        let _ = port.write_data_terminal_ready(true);
        eprintln!(
            "{:10.3}  opened {path} @ {baud} 8N1, receive-only",
            start.elapsed().as_secs_f64()
        );
        let mut buf = [0u8; 256];
        loop {
            match port.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => hexline(start.elapsed().as_secs_f64(), &buf[..n]),
                Err(e) if e.kind() == ErrorKind::TimedOut => {}
                Err(e) => {
                    eprintln!(
                        "{:10.3}  LOST: {e} ({:?}); waiting for the device to return",
                        start.elapsed().as_secs_f64(),
                        e.kind()
                    );
                    break;
                }
            }
        }
        drop(port);
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("list") => list(),
        Some("listen") if args.len() >= 3 => {
            let baud = args.get(3).and_then(|b| b.parse().ok()).unwrap_or(9600);
            listen(&args[2], baud)
        }
        _ => {
            eprintln!("usage: serial-spike list | listen <port> [baud]");
            std::process::exit(2);
        }
    }
}
