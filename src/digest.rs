//! SHA-256 of files, directories and strings. Contents are hashed and never stored.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::bundle::{FileDigest, Input};

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_bytes(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// SHA-256 and length of everything readable from `r`.
pub fn sha256_reader(mut r: impl Read) -> io::Result<(String, u64)> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        h.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex(&h.finalize()), total))
}

pub fn sha256_file(path: &Path) -> io::Result<(String, u64)> {
    sha256_reader(File::open(path)?)
}

/// With `--redact`: a string is replaced by the first 16 hex digits of its SHA-256, so equal
/// strings stay equal across bundles while the text itself is not stored.
pub fn redact(s: &str) -> String {
    format!("sha256:{}", &sha256_bytes(s.as_bytes())[..16])
}

fn walk(root: &Path, rel: &Path, depth: usize, out: &mut Vec<(String, std::path::PathBuf)>) -> io::Result<()> {
    if depth > 64 {
        return Err(io::Error::other(format!("{}: directory nesting too deep", root.display())));
    }
    let mut entries: Vec<_> = fs::read_dir(root.join(rel))?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let r = rel.join(e.file_name());
        let full = root.join(&r);
        let meta = fs::metadata(&full)?; // follows symlinks
        if meta.is_dir() {
            walk(root, &r, depth + 1, out)?;
        } else if meta.is_file() {
            out.push((r.to_string_lossy().into_owned(), full));
        }
    }
    Ok(())
}

/// Digest of an `--input` path: a file, or a directory (every regular file below it, sorted).
pub fn input(path: &Path, redacted: bool) -> io::Result<Input> {
    let shown = |s: &str| if redacted { redact(s) } else { s.to_string() };
    let meta = fs::metadata(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    let name = path.to_string_lossy();
    if meta.is_dir() {
        let mut files = Vec::new();
        walk(path, Path::new(""), 0, &mut files)?;
        let mut listing = String::new();
        let mut digests = Vec::new();
        let mut bytes = 0;
        for (rel, full) in files {
            let (sha, n) = sha256_file(&full)?;
            // The directory digest covers names and contents, like `sha256sum` output.
            listing.push_str(&format!("{sha}  {rel}\n"));
            bytes += n;
            digests.push(FileDigest { path: shown(&rel), bytes: n, sha256: sha });
        }
        Ok(Input {
            path: shown(&name),
            kind: "dir".into(),
            bytes,
            sha256: sha256_bytes(listing.as_bytes()),
            files: digests,
        })
    } else {
        let (sha, n) = sha256_file(path)?;
        Ok(Input { path: shown(&name), kind: "file".into(), bytes: n, sha256: sha, files: Vec::new() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(sha256_bytes(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_bytes(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sha256_reader(&b"abc"[..]).unwrap(), (sha256_bytes(b"abc"), 3));
        assert_eq!(redact("abc"), "sha256:ba7816bf8f01cfea");
    }
}
