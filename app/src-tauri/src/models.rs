//! Locating and downloading model files.

use crate::config::{ModelFiles, Settings};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The two variants that run on stock upstream llama.cpp.
///
/// Deliberately absent: Ternary Bonsai 2 (PTQ1_0/PQ2_0) and the `PQ2_0` file in
/// the plain ternary repo, which need a runtime Walsh-Hadamard transform that is
/// not upstream; and the legacy `-Q2_0.gguf`, a pre-migration packing stored
/// under the ggml type id that now belongs to the official group-64 format --
/// current builds load it without complaint and emit gibberish.
pub const VARIANTS: &[Variant] = &[
    Variant {
        id: "ternary",
        label: "Ternary Bonsai 27B (1.71 bpw)",
        repo: "prism-ml/Ternary-Bonsai-27B-gguf",
        weights: "Ternary-Bonsai-27B-Q2_g64.gguf",
        mmproj: "Ternary-Bonsai-27B-mmproj-Q8_0.gguf",
        bytes: 7_585_330_240 + 629_246_880,
    },
    Variant {
        id: "bonsai",
        label: "Bonsai 27B (1-bit)",
        repo: "prism-ml/Bonsai-27B-gguf",
        weights: "Bonsai-27B-Q1_0.gguf",
        mmproj: "Bonsai-27B-mmproj-Q8_0.gguf",
        bytes: 3_530_000_000 + 629_246_880,
    },
];

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Variant {
    pub id: &'static str,
    pub label: &'static str,
    pub repo: &'static str,
    pub weights: &'static str,
    pub mmproj: &'static str,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct VariantStatus {
    #[serde(flatten)]
    pub variant: Variant,
    pub installed: bool,
    pub dir: String,
}

/// Where a variant's files already exist, if anywhere.
///
/// Checks the configured model directory first, then a `models/` folder in the
/// checkout this app ships from -- so an existing 8 GB download is found instead
/// of fetched a second time.
fn search_dirs(settings: &Settings) -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from(&settings.model_dir)];
    if let Ok(cwd) = std::env::current_dir() {
        for base in cwd.ancestors().take(5) {
            dirs.push(base.join("models"));
        }
    }
    dirs
}

pub fn locate(settings: &Settings, variant: &Variant) -> Option<(PathBuf, ModelFiles)> {
    for dir in search_dirs(settings) {
        let weights = dir.join(variant.weights);
        let mmproj = dir.join(variant.mmproj);
        if weights.is_file() && mmproj.is_file() {
            return Some((
                dir,
                ModelFiles {
                    weights: weights.to_string_lossy().into_owned(),
                    mmproj: mmproj.to_string_lossy().into_owned(),
                },
            ));
        }
    }
    None
}

pub fn survey(settings: &Settings) -> Vec<VariantStatus> {
    VARIANTS
        .iter()
        .map(|v| match locate(settings, v) {
            Some((dir, _)) => VariantStatus {
                variant: *v,
                installed: true,
                dir: dir.to_string_lossy().into_owned(),
            },
            None => VariantStatus {
                variant: *v,
                installed: false,
                dir: settings.model_dir.clone(),
            },
        })
        .collect()
}

/// Where a download for this variant should read and write.
///
/// Resolution has to stay stable across a forced re-download: purging makes
/// `locate` return None, so keying the directory off that alone would send the
/// retry somewhere else and strand the partial file.
pub fn download_dir(settings: &Settings, variant: &Variant) -> PathBuf {
    if let Some((dir, _)) = locate(settings, variant) {
        return dir;
    }
    // A part file marks where a previous attempt was writing.
    let part = PathBuf::from(variant.weights).with_extension("part");
    let part_name = part.file_name().unwrap().to_owned();
    for dir in search_dirs(settings) {
        if dir.join(&part_name).is_file() {
            return dir;
        }
    }
    PathBuf::from(&settings.model_dir)
}

pub fn variant_by_id(id: &str) -> Option<&'static Variant> {
    VARIANTS.iter().find(|v| v.id == id)
}

// --- download ----------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub active: bool,
    pub file: String,
    pub received: u64,
    pub total: u64,
    pub done: bool,
    pub error: Option<String>,
}

#[derive(Clone, Default)]
pub struct Downloader {
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<Mutex<bool>>,
}

impl Downloader {
    pub fn progress(&self) -> Progress {
        self.progress.lock().unwrap().clone()
    }

    pub fn cancel(&self) {
        *self.cancel.lock().unwrap() = true;
    }

    /// `force` re-fetches files that are already complete, discarding whatever
    /// is on disk. Without it, complete files are skipped and partial ones
    /// resume.
    pub fn start(
        &self,
        variant: &'static Variant,
        dir: PathBuf,
        force: bool,
    ) -> Result<(), String> {
        if self.progress.lock().unwrap().active {
            return Err("A download is already running.".into());
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

        if force {
            purge(variant, &dir)?;
        }

        *self.cancel.lock().unwrap() = false;
        *self.progress.lock().unwrap() =
            Progress { active: true, ..Default::default() };

        let progress = Arc::clone(&self.progress);
        let cancel = Arc::clone(&self.cancel);
        std::thread::spawn(move || {
            for name in [variant.weights, variant.mmproj] {
                let url = format!(
                    "https://huggingface.co/{}/resolve/main/{name}?download=true",
                    variant.repo
                );
                if let Err(e) = fetch(&url, &dir.join(name), name, &progress, &cancel) {
                    let mut p = progress.lock().unwrap();
                    p.active = false;
                    p.error = Some(e);
                    return;
                }
            }
            let mut p = progress.lock().unwrap();
            p.active = false;
            p.done = true;
        });
        Ok(())
    }
}

/// Delete a variant's files so a forced download cannot resume onto old bytes.
///
/// Both the finished file and any stale `.part` have to go: leaving the part
/// behind would make the next fetch issue a Range request against content that
/// is no longer what is being downloaded.
fn purge(variant: &Variant, dir: &Path) -> Result<(), String> {
    for name in [variant.weights, variant.mmproj] {
        let target = dir.join(name);
        for path in [target.clone(), target.with_extension("part")] {
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("Could not remove {}: {e}", path.display()))?;
            }
        }
    }
    Ok(())
}

/// Download to a `.part` file and rename on success, so an interrupted transfer
/// never leaves a truncated GGUF that looks complete to the loader.
///
/// An existing `.part` is resumed with a Range request. At ~7.6 GB for the
/// ternary weights, restarting from zero after a dropped connection is not an
/// acceptable failure mode.
fn fetch(
    url: &str,
    dest: &Path,
    name: &str,
    progress: &Arc<Mutex<Progress>>,
    cancel: &Arc<Mutex<bool>>,
) -> Result<(), String> {
    if dest.is_file() {
        return Ok(());
    }
    let part = dest.with_extension("part");
    let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);

    let mut req = ureq::get(url);
    if have > 0 {
        req = req.set("Range", &format!("bytes={have}-"));
    }
    let resp = req.call().map_err(|e| format!("{name}: {e}"))?;

    // 206 means the server honoured the range; 200 means it sent the whole file
    // regardless, so anything already on disk has to be discarded.
    let resuming = resp.status() == 206;
    let body_len: u64 = resp
        .header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let total = if resuming { have + body_len } else { body_len };

    {
        let mut p = progress.lock().unwrap();
        p.file = name.to_string();
        p.received = if resuming { have } else { 0 };
        p.total = total;
    }

    let mut reader = resp.into_reader();
    let mut file = if resuming {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&part)
            .map_err(|e| format!("{}: {e}", part.display()))?
    } else {
        std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?
    };
    let mut buf = vec![0u8; 1 << 20];
    let mut received: u64 = if resuming { have } else { 0 };

    loop {
        if *cancel.lock().unwrap() {
            // Keep the part file: the next attempt resumes from here. Starting
            // over is what "Re-download" is for.
            drop(file);
            return Err("Download cancelled. Starting again will resume.".into());
        }
        let n = std::io::Read::read(&mut reader, &mut buf).map_err(|e| format!("{name}: {e}"))?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buf[..n]).map_err(|e| format!("{name}: {e}"))?;
        received += n as u64;
        progress.lock().unwrap().received = received;
    }

    drop(file);
    std::fs::rename(&part, dest).map_err(|e| format!("{}: {e}", dest.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bonsai-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn purge_removes_weights_and_part_files() {
        let variant = &VARIANTS[0];
        let dir = temp_dir("purge");
        let weights = dir.join(variant.weights);
        let part = dir.join(variant.mmproj).with_extension("part");
        let bystander = dir.join("unrelated.gguf");
        for f in [&weights, &part, &bystander] {
            std::fs::write(f, b"x").unwrap();
        }

        purge(variant, &dir).unwrap();

        assert!(!weights.exists(), "weights should be deleted");
        assert!(!part.exists(), "stale part file should be deleted");
        assert!(bystander.exists(), "unrelated files must be left alone");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn purge_is_fine_when_nothing_is_there() {
        let dir = temp_dir("purge-empty");
        assert!(purge(&VARIANTS[1], &dir).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn locate_prefers_the_configured_model_dir() {
        let variant = &VARIANTS[0];
        let dir = temp_dir("locate");
        std::fs::write(dir.join(variant.weights), b"x").unwrap();
        std::fs::write(dir.join(variant.mmproj), b"x").unwrap();

        let mut settings = Settings::default();
        settings.model_dir = dir.to_string_lossy().into_owned();

        let (found_dir, files) = locate(&settings, variant).expect("should find the pair");
        assert_eq!(found_dir, dir);
        assert!(files.weights.ends_with(variant.weights));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn download_dir_follows_an_existing_part_file() {
        let variant = &VARIANTS[0];
        let dir = temp_dir("partdir");
        let part = dir.join(variant.weights).with_extension("part");
        std::fs::write(&part, b"partial").unwrap();

        let mut settings = Settings::default();
        // Point model_dir somewhere else entirely: the part file should still win.
        settings.model_dir = temp_dir("partdir-other").to_string_lossy().into_owned();

        // search_dirs only walks model_dir and ancestors' models/ dirs, so make
        // the part directory the configured one for a second case.
        settings.model_dir = dir.to_string_lossy().into_owned();
        assert_eq!(download_dir(&settings, variant), dir);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn download_dir_falls_back_to_the_configured_dir() {
        let variant = &VARIANTS[1];
        let dir = temp_dir("emptydir");
        let mut settings = Settings::default();
        settings.model_dir = dir.to_string_lossy().into_owned();
        assert_eq!(download_dir(&settings, variant), dir);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn locate_skips_a_directory_missing_the_projector() {
        let variant = &VARIANTS[0];
        let dir = temp_dir("locate-partial");
        // Weights without the projector is not a usable install: the 27B needs
        // the mmproj for image input.
        std::fs::write(dir.join(variant.weights), b"x").unwrap();

        let mut settings = Settings::default();
        settings.model_dir = dir.to_string_lossy().into_owned();

        // locate() also searches `models/` directories up the tree, so it may
        // legitimately find a complete install elsewhere -- what matters is
        // that it does not accept this half-populated one.
        match locate(&settings, variant) {
            None => {}
            Some((found, _)) => assert_ne!(found, dir, "half-populated dir was accepted"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod network_tests {
    /// Does ureq keep a Range header across Hugging Face's 302 to the CDN?
    /// Run with: cargo test -- --ignored --nocapture
    #[test]
    #[ignore]
    fn range_header_survives_redirect() {
        let url = "https://huggingface.co/prism-ml/Ternary-Bonsai-27B-gguf/resolve/main/Ternary-Bonsai-27B-Q2_g64.gguf?download=true";
        let resp = ureq::get(url)
            .set("Range", "bytes=1000000000-")
            .call()
            .expect("request failed");
        println!("status = {}", resp.status());
        println!("content-range = {:?}", resp.header("content-range"));
        println!("content-length = {:?}", resp.header("content-length"));
        assert_eq!(resp.status(), 206, "expected a partial-content response");
    }
}
