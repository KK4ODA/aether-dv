//! CI-V transactions on the configured control port: read the radio's DV
//! Fast Data setting, or set it and read it back. Blocking; run it on a
//! blocking thread. Only the configured path is opened, only for the
//! duration of one transaction.

use std::time::{Duration, Instant};

use aether_civ::{fast_data_state, Deframer, Model, Reply};

fn transact(
    port: &mut dyn serialport::SerialPort,
    d: &mut Deframer,
    req: &[u8],
    cmd: &[u8],
    radio: u8,
    deadline: Instant,
) -> Result<Reply, String> {
    port.write_all(req)
        .and_then(|_| port.flush())
        .map_err(|e| format!("CI-V write failed: {e}"))?;
    let mut buf = [0u8; 64];
    while Instant::now() < deadline {
        match port.read(&mut buf) {
            Ok(n) => {
                for f in d.feed(&buf[..n]) {
                    if let Some(r) = f.reply_to(cmd, radio) {
                        return Ok(r);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(format!("CI-V read failed: {e}")),
        }
    }
    Err(format!(
        "no answer from the radio at CI-V address {radio:02X}h (check the CI-V address, baud rate and the radio's CI-V/USB settings)"
    ))
}

/// Read (and with `set`, first change) the Fast Data setting. Returns the
/// state the radio reports afterwards.
pub fn fast_data(
    path: &str,
    baud: u32,
    model: &Model,
    radio: u8,
    set: Option<bool>,
    timeout: Duration,
) -> Result<bool, String> {
    let mut port = serialport::new(path, baud)
        .timeout(Duration::from_millis(50))
        .open()
        .map_err(|e| format!("cannot open CI-V port {path}: {e}"))?;
    let mut d = Deframer::default();
    let cmd = model.fast_data_cmd;
    if let Some(on) = set {
        let req = aether_civ::set_fast_data(model, radio, on);
        match transact(
            &mut *port,
            &mut d,
            &req,
            cmd,
            radio,
            Instant::now() + timeout,
        )? {
            Reply::Ok => {}
            Reply::Ng => return Err("the radio refused the setting (NG)".into()),
            Reply::Data(_) => return Err("unexpected reply to the set command".into()),
        }
    }
    let req = aether_civ::read_fast_data(model, radio);
    match transact(
        &mut *port,
        &mut d,
        &req,
        cmd,
        radio,
        Instant::now() + timeout,
    )? {
        Reply::Ng => Err("the radio refused the read (NG)".into()),
        r => fast_data_state(&r).ok_or_else(|| format!("unexpected reply {r:?}")),
    }
}
