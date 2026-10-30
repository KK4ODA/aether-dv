//! Support bundle: one `.tar` file an operator can attach to a bug report.
//!
//! Written with a minimal ustar writer (no compression; captures are
//! already small). The caller decides the contents; [`README`] documents
//! what a bundle never contains.

use std::io::Write;
use std::path::Path;

pub const README: &str = "Aether DV support bundle

about.json      engine version, OS, ports and their state (including each
                radio port's route and channel), configuration (passwords
                removed)
events.jsonl    the engine log (newest last)
transfers.json  file and form transfers (names, sizes, states)
messages.json   message delivery states (ids, addresses, states; no content)
captures/       wire captures (.aecap), only if you chose to include them.
                Captures contain the traffic itself: chat text, files and
                forms as they went over the air or the ratflector. On radio
                ports with CI-V they also hold the CI-V traffic and a
                timeline; read them with `aetherdv-cap show` or `timing`.

Never included: message bodies or subjects, received files, ratflector
passwords, the API token.
";

/// Append one regular file to a tar stream.
pub fn tar_file(out: &mut impl Write, name: &str, data: &[u8], mtime: u64) -> std::io::Result<()> {
    let mut h = [0u8; 512];
    let (prefix, base) = match name.len() {
        0..=100 => ("", name),
        _ => match name[..name.len().min(156)].rfind('/') {
            Some(i) if name.len() - i - 1 <= 100 && i <= 155 => (&name[..i], &name[i + 1..]),
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("name too long for tar: {name}"),
                ))
            }
        },
    };
    h[..base.len()].copy_from_slice(base.as_bytes());
    h[100..108].copy_from_slice(b"0000644\0");
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
    h[136..148].copy_from_slice(format!("{:011o}\0", mtime).as_bytes());
    h[148..156].copy_from_slice(b"        ");
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    out.write_all(&h)?;
    out.write_all(data)?;
    let pad = (512 - data.len() % 512) % 512;
    out.write_all(&vec![0u8; pad])
}

/// Write a complete tar of `files` (name, content) to `path`.
pub fn write_tar(path: &Path, files: &[(String, Vec<u8>)], mtime: u64) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    for (name, data) in files {
        tar_file(&mut f, name, data, mtime)?;
    }
    f.write_all(&[0u8; 1024])?;
    f.flush()?;
    f.get_ref().sync_all()
}

/// Write a gzip-compressed tar (`.tar.gz`) of `files` to `path`: what a
/// radio test produces, small enough to upload or attach.
pub fn write_tar_gz(path: &Path, files: &[(String, Vec<u8>)], mtime: u64) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut gz = flate2::write::GzEncoder::new(f, flate2::Compression::default());
    for (name, data) in files {
        tar_file(&mut gz, name, data, mtime)?;
    }
    gz.write_all(&[0u8; 1024])?;
    let mut f = gz.finish()?;
    f.flush()?;
    f.get_ref().sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse our own output the way `tar` does: header fields, checksum,
    /// padding and the end-of-archive marker.
    #[test]
    fn gzip_bundle_decompresses_to_the_same_tar() {
        use std::io::Read;
        let d = tempfile::tempdir().unwrap();
        let files = vec![("summary.txt".to_string(), b"hello".to_vec())];
        let (t, g) = (d.path().join("a.tar"), d.path().join("a.tar.gz"));
        write_tar(&t, &files, 1).unwrap();
        write_tar_gz(&g, &files, 1).unwrap();
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(std::fs::File::open(&g).unwrap())
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, std::fs::read(&t).unwrap());
    }

    #[test]
    fn tar_is_well_formed() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("b.tar");
        let long = format!("captures/{}.aecap", "x".repeat(90));
        let files = vec![
            ("README.txt".to_string(), README.as_bytes().to_vec()),
            (long.clone(), vec![7u8; 513]),
            ("empty".to_string(), vec![]),
        ];
        write_tar(&p, &files, 1_700_000_000).unwrap();
        let t = std::fs::read(&p).unwrap();
        assert_eq!(t.len() % 512, 0);
        let mut off = 0;
        for (name, data) in &files {
            let h = &t[off..off + 512];
            let mut sum: u32 = h.iter().map(|&b| b as u32).sum();
            sum -= h[148..156].iter().map(|&b| b as u32).sum::<u32>();
            sum += 8 * b' ' as u32;
            let stored =
                u32::from_str_radix(std::str::from_utf8(&h[148..154]).unwrap(), 8).unwrap();
            assert_eq!(sum, stored);
            assert_eq!(&h[257..262], b"ustar");
            let field = |r: std::ops::Range<usize>| {
                let s = &h[r];
                String::from_utf8(s[..s.iter().position(|&b| b == 0).unwrap_or(s.len())].to_vec())
                    .unwrap()
            };
            let full = match field(345..500) {
                p if p.is_empty() => field(0..100),
                p => format!("{p}/{}", field(0..100)),
            };
            assert_eq!(&full, name);
            let size = usize::from_str_radix(&field(124..135), 8).unwrap();
            assert_eq!(size, data.len());
            assert_eq!(&t[off + 512..off + 512 + size], &data[..]);
            off += 512 + size.div_ceil(512) * 512;
        }
        assert!(t[off..].iter().all(|&b| b == 0) && t.len() - off == 1024);
    }

    #[test]
    fn overlong_names_are_refused() {
        let mut v = Vec::new();
        assert!(tar_file(&mut v, &"y".repeat(300), b"", 0).is_err());
    }
}
