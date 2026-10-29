//! `aether-geo`: the position reports D-RATS stations exchange.
//!
//! Two text formats travel as chat lines (session 1, type 0), and Icom
//! radios with GPS-TX emit the same text on their data port outside any
//! frame (D-Rats @d0e7a39 `gps.py`, `transport.py` `_match_gps`):
//!
//! * **NMEA style**: `$GPGGA,…*CS\r\n` (optionally also `$GPRMC,…*CS`)
//!   followed by a station line `%-8.8s,%-20.20s\r\n` (callsign, comment).
//!   The comment may carry a DPRS symbol code and checksum
//!   (`BC  comment*5A`).
//! * **DPRS / GPS-A** (APRS over D-STAR):
//!   `$$CRC6CD1,CALL>APRATS,DSTAR*:@233208h4529.05N/12305.91W>comment\r`;
//!   the CRC is CRC-16/X-25 (Icom) over everything after the comma,
//!   including the `\r`.
//!
//! Aether parses both, checks every checksum, and reports why a sentence
//! was rejected instead of silently dropping it.

use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Nmea,
    Dprs,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Nmea => "nmea",
            Format::Dprs => "dprs",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    /// Callsign as sent, internal spaces turned into `-` (`K7TAY M` →
    /// `K7TAY-M`, as D-RATS does for DPRS).
    pub station: String,
    pub lat: f64,
    pub lon: f64,
    pub alt_m: Option<f64>,
    pub course_deg: Option<f64>,
    pub speed_kmh: Option<f64>,
    /// APRS symbol (table + code) for DPRS, or the DPRS code from an NMEA
    /// comment.
    pub symbol: Option<String>,
    pub comment: String,
    pub format: Format,
    /// Time of day of the fix (UTC seconds since midnight), when the
    /// sentence carries one.
    pub fix_utc_secs: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeoError {
    Checksum {
        sent: String,
        computed: String,
    },
    Malformed(&'static str),
    /// Well-formed but not a usable fix (receiver says "void", or the
    /// coordinates are out of range).
    NoFix(&'static str),
}

impl std::fmt::Display for GeoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GeoError::Checksum { sent, computed } => {
                write!(f, "checksum mismatch (sent {sent}, computed {computed})")
            }
            GeoError::Malformed(m) => write!(f, "malformed: {m}"),
            GeoError::NoFix(m) => write!(f, "no valid fix: {m}"),
        }
    }
}

/// XOR of the bytes (NMEA sentence checksum, and DPRS comment checksum).
pub fn xor_checksum(s: &str) -> u8 {
    s.bytes().fold(0, |a, b| a ^ b)
}

/// Icom GPS-A checksum: CRC-16/X-25 (reflected 0x1021 = 0x8408, init
/// 0xFFFF, final complement). `gps.py` `gpsa_checksum`.
pub fn icom_crc(s: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &b in s {
        let mut c = b;
        for _ in 0..8 {
            let xor = ((crc ^ c as u16) & 1) == 1;
            crc >>= 1;
            if xor {
                crc ^= 0x8408;
            }
            c >>= 1;
        }
    }
    !crc
}

fn normalize_station(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join("-")
}

/// `ddmm.mmmm` + hemisphere → signed degrees.
fn nmea_to_deg(v: &str, hemi: &str) -> Result<f64, GeoError> {
    let x: f64 = v
        .trim()
        .parse()
        .map_err(|_| GeoError::Malformed("coordinate"))?;
    let deg = (x / 100.0).trunc();
    let d = deg + (x - deg * 100.0) / 60.0;
    match hemi.trim() {
        "N" | "E" => Ok(d),
        "S" | "W" => Ok(-d),
        _ => Err(GeoError::Malformed("hemisphere")),
    }
}

/// Signed degrees → (`ddmm.mm…` value, rounded to `decimals`, minutes
/// carried so they never print as 60).
fn deg_to_nmea(deg: f64, decimals: i32) -> f64 {
    let a = deg.abs();
    let mut d = a.trunc();
    let scale = 10f64.powi(decimals);
    let mut m = ((a - d) * 60.0 * scale).round() / scale;
    if m >= 60.0 {
        m -= 60.0;
        d += 1.0;
    }
    d * 100.0 + m
}

fn hhmmss(s: &str) -> Option<u32> {
    let s = s.split('.').next()?;
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s[r].parse::<u32>().ok();
    let (h, m, sec) = (n(0..2)?, n(2..4)?, n(4..6)?);
    (h < 24 && m < 60 && sec < 61).then_some(h * 3600 + m * 60 + sec)
}

fn check_range(p: &Position) -> Result<(), GeoError> {
    if !(-90.0..=90.0).contains(&p.lat) || !(-180.0..=180.0).contains(&p.lon) {
        return Err(GeoError::NoFix("coordinates out of range"));
    }
    Ok(())
}

/// Find and parse the first position report in `text`. `None` if the text
/// contains no position sentence at all.
pub fn parse(text: &str) -> Option<Result<Position, GeoError>> {
    if let Some(i) = text.find("$$CRC") {
        return Some(parse_dprs(&text[i..]));
    }
    let i = ["$GPGGA", "$GPRMC", "$GNGGA", "$GNRMC"]
        .iter()
        .filter_map(|t| text.find(t))
        .min()?;
    Some(parse_nmea(&text[i..]))
}

fn verify_nmea(sentence: &str) -> Result<&str, GeoError> {
    let body = sentence
        .strip_prefix('$')
        .ok_or(GeoError::Malformed("no $"))?;
    let star = body.rfind('*').ok_or(GeoError::Malformed("no *checksum"))?;
    let sent = body
        .get(star + 1..star + 3)
        .ok_or(GeoError::Malformed("short checksum"))?;
    let computed = format!("{:02X}", xor_checksum(&body[..star]));
    if !sent.eq_ignore_ascii_case(&computed) {
        return Err(GeoError::Checksum {
            sent: sent.to_string(),
            computed,
        });
    }
    Ok(&body[..star])
}

fn parse_nmea(text: &str) -> Result<Position, GeoError> {
    let lines: Vec<&str> = text
        .split(['\r', '\n'])
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect();
    let mut gga: Option<Vec<&str>> = None;
    let mut rmc: Option<Vec<&str>> = None;
    let mut station_line = None;
    for l in &lines {
        if l.starts_with('$') {
            if station_line.is_some() {
                break;
            }
            let body = verify_nmea(l)?;
            let f: Vec<&str> = body.split(',').collect();
            match f[0].get(2..) {
                Some("GGA") if gga.is_none() => gga = Some(f),
                Some("RMC") if rmc.is_none() => rmc = Some(f),
                _ => {}
            }
        } else if l.contains(',') && station_line.is_none() {
            station_line = Some(*l);
        } else {
            break;
        }
    }
    let mut p = Position {
        station: String::new(),
        lat: 0.0,
        lon: 0.0,
        alt_m: None,
        course_deg: None,
        speed_kmh: None,
        symbol: None,
        comment: String::new(),
        format: Format::Nmea,
        fix_utc_secs: None,
    };
    if let Some(f) = &gga {
        if f.len() < 10 {
            return Err(GeoError::Malformed("GGA has too few fields"));
        }
        if f[6] == "0" || f[2].is_empty() {
            return Err(GeoError::NoFix("GGA fix quality 0"));
        }
        p.lat = nmea_to_deg(f[2], f[3])?;
        p.lon = nmea_to_deg(f[4], f[5])?;
        p.alt_m = f[9].parse().ok();
        p.fix_utc_secs = hhmmss(f[1]);
    }
    if let Some(f) = &rmc {
        if f.len() < 10 {
            return Err(GeoError::Malformed("RMC has too few fields"));
        }
        if f[2] != "A" {
            return Err(GeoError::NoFix("RMC status is void"));
        }
        if gga.is_none() {
            p.lat = nmea_to_deg(f[3], f[4])?;
            p.lon = nmea_to_deg(f[5], f[6])?;
            p.fix_utc_secs = hhmmss(f[1]);
        }
        p.speed_kmh = f[7].parse::<f64>().ok().map(|kn| kn * 1.852);
        p.course_deg = f[8].parse().ok();
    }
    if gga.is_none() && rmc.is_none() {
        return Err(GeoError::Malformed("no GGA or RMC sentence"));
    }
    if let Some(line) = station_line {
        let (sta, com) = line.split_once(',').unwrap_or((line, ""));
        let sta: String = sta.trim().chars().take(8).collect();
        let com: String = com.trim().chars().take(20).collect();
        p.station = normalize_station(&sta);
        p.comment = com.clone();
        // DPRS-coded comment: `CODE comment*XX`, checksum over
        // "%-8s,%s" % (station, comment-before-*) (aprs_icons.py).
        let n = com.len();
        if n >= 7 && com.is_char_boundary(n - 3) && com[n - 3..n - 1].contains('*') {
            let ast = com.rfind('*').unwrap_or(0);
            let sent = &com[ast + 1..];
            let computed = xor_checksum(&format!("{sta:<8},{}", &com[..ast]));
            if u8::from_str_radix(sent, 16).ok() != Some(computed) {
                return Err(GeoError::Checksum {
                    sent: sent.to_string(),
                    computed: format!("{computed:02X}"),
                });
            }
            p.symbol =
                Some(com.get(..4).unwrap_or("").trim().to_string()).filter(|s| !s.is_empty());
            p.comment = com.get(4..ast).unwrap_or("").trim().to_string();
        }
    }
    if p.station.is_empty() {
        return Err(GeoError::Malformed(
            "no station line after the NMEA sentence",
        ));
    }
    check_range(&p)?;
    Ok(p)
}

fn digits(s: &str, n: usize) -> bool {
    s.len() >= n && s.as_bytes()[..n].iter().all(u8::is_ascii_digit)
}

fn parse_dprs(text: &str) -> Result<Position, GeoError> {
    let rest = text
        .strip_prefix("$$CRC")
        .ok_or(GeoError::Malformed("no $$CRC"))?;
    let sent = rest.get(..4).ok_or(GeoError::Malformed("short CRC"))?;
    let body = rest
        .get(4..)
        .and_then(|r| r.strip_prefix(','))
        .ok_or(GeoError::Malformed("no comma after CRC"))?;
    let body = match body.find('\r') {
        Some(i) => &body[..=i],
        None => body.split('\n').next().unwrap_or(""),
    };
    let sent_v = u16::from_str_radix(sent, 16).map_err(|_| GeoError::Malformed("CRC not hex"))?;
    let computed = icom_crc(body.as_bytes());
    let ok = computed == sent_v
        || (!body.ends_with('\r') && icom_crc(format!("{body}\r").as_bytes()) == sent_v);
    if !ok {
        return Err(GeoError::Checksum {
            sent: sent.to_string(),
            computed: format!("{computed:04X}"),
        });
    }
    let body = body.trim_end_matches('\r');
    let (station, after) = body.split_once('>').ok_or(GeoError::Malformed("no '>'"))?;
    let (_path, data) = after.split_once(':').ok_or(GeoError::Malformed("no ':'"))?;
    // Timestamp: @/ + 6 digits + h|z|/ ; or ! / = without time.
    let (fix_utc_secs, mut d) = match data.chars().next() {
        Some('@') | Some('/') if digits(&data[1..], 6) => {
            let suffix = data.as_bytes().get(7).copied();
            let t = match suffix {
                Some(b'h') => hhmmss(&data[1..7]),
                Some(b'z') | Some(b'/') => None, // day-hour-minute; day not modelled
                _ => return Err(GeoError::Malformed("timestamp suffix")),
            };
            (t, &data[8..])
        }
        Some('!') | Some('=') => (None, &data[1..]),
        _ => return Err(GeoError::Malformed("position data type")),
    };
    // Latitude: 1-4 digits, '.', 2 digits, N/S.
    let dot = d.find('.').ok_or(GeoError::Malformed("latitude"))?;
    if !(1..=4).contains(&dot) || !digits(&d[..dot], dot) || !digits(&d[dot + 1..], 2) {
        return Err(GeoError::Malformed("latitude"));
    }
    let lat = nmea_to_deg(&d[..dot + 3], d.get(dot + 3..dot + 4).unwrap_or(""))?;
    d = &d[dot + 4..];
    // Optional symbol table character, then a 5.2 longitude.
    let table = if digits(d, 5) { None } else { d.chars().next() };
    if let Some(t) = table {
        d = &d[t.len_utf8()..];
    }
    if !digits(d, 5) || d.as_bytes().get(5) != Some(&b'.') || !digits(&d[6..], 2) {
        return Err(GeoError::Malformed("longitude"));
    }
    let lon = nmea_to_deg(&d[..8], d.get(8..9).unwrap_or(""))?;
    d = &d[9..];
    let code = d.chars().next();
    if let Some(c) = code {
        d = &d[c.len_utf8()..];
    }
    // Optional APRS course/speed `ccc/sss` (knots), then free text that may
    // contain `/A=nnnnnn` (feet).
    let mut course_deg = None;
    let mut speed_kmh = None;
    if d.len() >= 7 && digits(d, 3) && d.as_bytes()[3] == b'/' && digits(&d[4..], 3) {
        course_deg = d[..3].parse().ok();
        speed_kmh = d[4..7].parse::<f64>().ok().map(|kn| kn * 1.852);
        d = &d[7..];
    }
    let mut comment = d.to_string();
    let mut alt_m = None;
    if let Some(i) = comment.find("/A=") {
        if digits(&comment[i + 3..], 6) {
            alt_m = comment[i + 3..i + 9]
                .parse::<f64>()
                .ok()
                .map(|ft| ft * 0.3048);
            comment.replace_range(i..i + 9, "");
        }
    }
    let p = Position {
        station: normalize_station(station),
        lat,
        lon,
        alt_m,
        course_deg,
        speed_kmh,
        symbol: match (table, code) {
            (Some(t), Some(c)) => Some(format!("{t}{c}")),
            (None, Some(c)) => Some(c.to_string()),
            _ => None,
        },
        comment: comment.trim().to_string(),
        format: Format::Dprs,
        fix_utc_secs,
    };
    check_range(&p)?;
    Ok(p)
}

/// Our own position, to send.
#[derive(Debug, Clone)]
pub struct Report<'a> {
    pub station: &'a str,
    pub lat: f64,
    pub lon: f64,
    pub alt_m: Option<f64>,
    /// APRS symbol: table + code, e.g. `/>` (car), `/-` (house).
    pub symbol: &'a str,
    pub comment: &'a str,
    /// UTC seconds since midnight.
    pub utc_secs: u32,
}

/// DPRS sentence as D-RATS `to_aprs` writes it: `$$CRCxxxx,…\r\n`
/// (legacy ends with `\r` inside the CRC and a final `\n`).
pub fn format_dprs(r: &Report<'_>) -> String {
    let (h, m, s) = (
        r.utc_secs / 3600 % 24,
        r.utc_secs / 60 % 60,
        r.utc_secs % 60,
    );
    let mut sym = r.symbol.chars();
    let table = sym.next().unwrap_or('/');
    let code = sym.next().unwrap_or('>');
    let mut body = format!(
        "{}>APRATS,DSTAR*:/{h:02}{m:02}{s:02}h{:07.2}{}{table}{:08.2}{}{code}",
        normalize_station(r.station),
        deg_to_nmea(r.lat, 2),
        if r.lat >= 0.0 { 'N' } else { 'S' },
        deg_to_nmea(r.lon, 2),
        if r.lon >= 0.0 { 'E' } else { 'W' },
    );
    let mut room = 43;
    if let Some(a) = r.alt_m {
        let _ = write!(
            body,
            "/A={:06}",
            (a / 0.3048).round().clamp(0.0, 999_999.0) as i64
        );
        room -= 9;
    }
    body.extend(
        r.comment
            .chars()
            .filter(|c| *c != '\r' && *c != '\n')
            .take(room),
    );
    body.push('\r');
    format!("$$CRC{:04X},{body}\n", icom_crc(body.as_bytes()))
}

/// NMEA-style report as D-RATS `to_nmea_gga` writes it (time in UTC here;
/// D-RATS uses local time).
pub fn format_nmea(r: &Report<'_>) -> String {
    let (h, m, s) = (
        r.utc_secs / 3600 % 24,
        r.utc_secs / 60 % 60,
        r.utc_secs % 60,
    );
    let data = format!(
        "GPGGA,{h:02}{m:02}{s:02},{:.3},{},{:.3},{},1,3,0,{},M,0,M,,",
        deg_to_nmea(r.lat, 3),
        if r.lat >= 0.0 { 'N' } else { 'S' },
        deg_to_nmea(r.lon, 3),
        if r.lon >= 0.0 { 'E' } else { 'W' },
        r.alt_m.unwrap_or(0.0).round() as i64,
    );
    let station: String = r.station.chars().take(8).collect();
    let comment: String = r.comment.chars().take(20).collect();
    format!(
        "${data}*{:02x}\r\n{station:<8},{comment:<20}\r\n",
        xor_checksum(&data)
    )
}

/// Great-circle distance in km.
pub fn distance_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    6371.0088 * 2.0 * a.sqrt().atan2((1.0 - a).sqrt())
}

/// Initial bearing from point 1 to point 2, degrees 0-360.
pub fn bearing_deg(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dl = (lon2 - lon1).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    (y.atan2(x).to_degrees() + 360.0) % 360.0
}

/// Extracts position sentences from a radio's raw output outside frames
/// (GPS-TX), as `transport.py` `_match_gps` does. Bounded buffer.
#[derive(Debug, Default)]
pub struct Scanner {
    buf: String,
}

impl Scanner {
    const MAX: usize = 1024;

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        loop {
            let Some(start) = self.buf.find('$') else {
                self.buf.clear();
                break;
            };
            if start > 0 {
                self.buf.drain(..start);
            }
            match self.take_one() {
                Some(Some(s)) => out.push(s),
                Some(None) => {
                    // Not a sentence start: skip this '$'.
                    self.buf.drain(..1);
                }
                None => break, // incomplete; wait for more
            }
        }
        if self.buf.len() > Self::MAX {
            let cut = self.buf.len() - Self::MAX;
            let cut = (cut..self.buf.len())
                .find(|&i| self.buf.is_char_boundary(i))
                .unwrap_or(self.buf.len());
            self.buf.drain(..cut);
        }
        out
    }

    /// `Some(Some(sentence))` extracted, `Some(None)` not a sentence here,
    /// `None` need more bytes. The buffer starts with '$'.
    fn take_one(&mut self) -> Option<Option<String>> {
        let b = &self.buf;
        if b.starts_with("$$CRC") {
            let end = b.find('\r')?;
            let s = b[..=end].to_string();
            self.buf.drain(..=end);
            return Some(Some(s));
        }
        if b.len() < 6 {
            return if "$$CRC".starts_with(b.as_str()) || b.starts_with("$G") || b == "$" {
                None
            } else {
                Some(None)
            };
        }
        if !b.starts_with("$G") {
            return Some(None);
        }
        // One or two `$G…*hh` sentences, then a station line of 8 + 1 + 20.
        let mut i = 0;
        for _ in 0..2 {
            if !b[i..].starts_with("$G") {
                break;
            }
            let star = b[i..].find('*')? + i;
            if b.len() < star + 3 {
                return None;
            }
            i = star + 3;
            while b[i..].starts_with(['\r', '\n']) {
                i += 1;
            }
        }
        let line = &b[i..];
        let need = 8 + 1 + 20;
        if line.len() < need {
            return None;
        }
        if line.as_bytes()[8] != b',' {
            return Some(None);
        }
        let end = i + need;
        let s = b[..end].to_string();
        self.buf.drain(..end);
        Some(Some(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Sample sentences from D-Rats @d0e7a39 transport.py (FakeSerial).
    const GGA_K7HIO: &str =
        "$GPGGA,075519,4531.254,N,12259.400,W,1,3,0,0.0,M,0,M,,*55\r\nK7HIO   ,GPS Info\r";
    const DPRS_HILLS: &str = "$$CRC6CD1,Hills-Water-Treat-Plt>APRATS,DSTAR*:@233208h4529.05N/12305.91W>Washington County ARES;Hills Water Treat Pl\r\n";
    const GGA_RMC_K7TAY: &str = "$GPGGA,023531.36,4531.4940,N,12254.9766,W,1,07,1.3,63.7,M,-21.4,M,,*64\r\n$GPRMC,023531.36,A,4531.4940,N,12254.9766,W,0.00,113.7,010808,17.4,E,A*27\r\nK7TAY M ,/10-13/\r";

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn checksums() {
        assert_eq!(icom_crc(b"123456789"), 0x906E); // CRC-16/X-25 check value
        assert_eq!(
            xor_checksum("GPGGA,075519,4531.254,N,12259.400,W,1,3,0,0.0,M,0,M,,"),
            0x55
        );
    }

    #[test]
    fn legacy_samples_parse() {
        let p = parse(GGA_K7HIO).unwrap().unwrap();
        assert_eq!(
            (p.station.as_str(), p.comment.as_str()),
            ("K7HIO", "GPS Info")
        );
        assert!(close(p.lat, 45.0 + 31.254 / 60.0) && close(p.lon, -(122.0 + 59.4 / 60.0)));
        assert_eq!(p.fix_utc_secs, Some(7 * 3600 + 55 * 60 + 19));
        assert_eq!(p.format, Format::Nmea);

        let p = parse(DPRS_HILLS).unwrap().unwrap();
        assert_eq!(p.station, "Hills-Water-Treat-Plt");
        assert!(close(p.lat, 45.0 + 29.05 / 60.0) && close(p.lon, -(123.0 + 5.91 / 60.0)));
        assert_eq!(p.symbol.as_deref(), Some("/>"));
        assert_eq!(p.comment, "Washington County ARES;Hills Water Treat Pl");
        assert_eq!(p.fix_utc_secs, Some(23 * 3600 + 32 * 60 + 8));

        let p = parse(GGA_RMC_K7TAY).unwrap().unwrap();
        assert_eq!(p.station, "K7TAY-M");
        assert_eq!(p.alt_m, Some(63.7));
        assert_eq!(p.course_deg, Some(113.7));
        assert_eq!(p.speed_kmh, Some(0.0));
        assert_eq!(p.comment, "/10-13/");
    }

    #[test]
    fn corrupted_sentences_are_rejected_with_a_reason() {
        let bad = GGA_K7HIO.replace("4531.254", "4531.255");
        assert!(matches!(parse(&bad), Some(Err(GeoError::Checksum { .. }))));
        let bad = DPRS_HILLS.replace("County", "C0unty");
        assert!(matches!(parse(&bad), Some(Err(GeoError::Checksum { .. }))));
        let void = "$GPRMC,023531.36,V,4531.4940,N,12254.9766,W,0.00,113.7,010808,17.4,E,A*30\r\nK7TAY   ,x\r";
        assert!(matches!(parse(void), Some(Err(GeoError::NoFix(_)))));
        assert!(parse("just a chat line with $5").is_none());
        assert!(matches!(parse("$GPGGA,075519"), Some(Err(_))));
    }

    #[test]
    fn our_output_round_trips_and_matches_legacy_shape() {
        let r = Report {
            station: "KK4ODA",
            lat: 34.0 + 59.99999 / 60.0, // minutes round up to 60: must carry
            lon: -84.5,
            alt_m: Some(300.0),
            symbol: "/-",
            comment: "EOC net control",
            utc_secs: 13 * 3600 + 5 * 60 + 9,
        };
        let d = format_dprs(&r);
        assert!(d.starts_with("$$CRC") && d.ends_with("\r\n"), "{d:?}");
        assert!(
            d.contains(
                "KK4ODA>APRATS,DSTAR*:/130509h3500.00N/08430.00W-/A=000984EOC net control\r"
            ),
            "{d:?}"
        );
        let p = parse(&d).unwrap().unwrap();
        assert!(close(p.lat, 35.0) && close(p.lon, -84.5));
        assert_eq!(
            (p.symbol.as_deref(), p.comment.as_str()),
            (Some("/-"), "EOC net control")
        );
        assert!((p.alt_m.unwrap() - 300.0).abs() < 0.2);

        let n = format_nmea(&r);
        assert!(
            n.starts_with("$GPGGA,130509,3500.000,N,8430.000,W,1,3,0,300,M,0,M,,*"),
            "{n:?}"
        );
        assert!(
            n.ends_with("\r\nKK4ODA  ,EOC net control     \r\n"),
            "{n:?}"
        );
        let p = parse(&n).unwrap().unwrap();
        assert!(close(p.lat, 35.0) && close(p.lon, -84.5));
        assert_eq!(p.station, "KK4ODA");
    }

    #[test]
    fn dprs_code_in_nmea_comment_is_checked() {
        // "%-8s,%s" % ("KK4ODA", "BC  mobile") XOR -> checksum suffix.
        let com = "BC  mobile";
        let cs = xor_checksum(&format!("{:<8},{}", "KK4ODA", com));
        let data = "GPGGA,120000,3400.000,N,8400.000,W,1,3,0,0,M,0,M,,";
        let s = format!(
            "${data}*{:02X}\r\nKK4ODA  ,{com}*{cs:02X}\r\n",
            xor_checksum(data)
        );
        let p = parse(&s).unwrap().unwrap();
        assert_eq!(
            (p.symbol.as_deref(), p.comment.as_str()),
            (Some("BC"), "mobile")
        );
        let bad = s.replace(&format!("*{cs:02X}"), &format!("*{:02X}", cs ^ 1));
        assert!(matches!(parse(&bad), Some(Err(GeoError::Checksum { .. }))));
    }

    #[test]
    fn scanner_extracts_radio_output_across_reads() {
        let mut sc = Scanner::default();
        let stream = format!("noise{GGA_K7HIO}       xx{DPRS_HILLS}tail$");
        // The K7HIO station line is padded to 20 in real output.
        let stream = stream.replace("K7HIO   ,GPS Info\r", "K7HIO   ,GPS Info            \r");
        let mut got = Vec::new();
        for chunk in stream.as_bytes().chunks(7) {
            got.extend(sc.feed(chunk));
        }
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(parse(&got[0]).unwrap().unwrap().station, "K7HIO");
        assert_eq!(
            parse(&got[1]).unwrap().unwrap().station,
            "Hills-Water-Treat-Plt"
        );
        // Garbage never grows the buffer without bound.
        for _ in 0..100 {
            sc.feed(&[b'$'; 100]);
        }
        assert!(sc.buf.len() <= Scanner::MAX);
    }

    #[test]
    fn distance_and_bearing() {
        // Atlanta to Chattanooga, roughly 170 km to the NNW.
        let d = distance_km(33.749, -84.388, 35.0456, -85.3097);
        assert!((d - 168.0).abs() < 5.0, "{d}");
        let b = bearing_deg(33.749, -84.388, 35.0456, -85.3097);
        assert!((b - 330.0).abs() < 5.0, "{b}");
    }
}
