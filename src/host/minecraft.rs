//! The Minecraft bank on the host: finding the user's installed game sounds, decoding them and
//! writing `minecraft-local.sf2` with its sidecar (the file and decoder halves of
//! `Audio/MinecraftAssets.swift` and `MinecraftImport`; everything that is arithmetic is in
//! `crate::sound::minecraft`).
//!
//! `MinecraftImport.decode` is AVFoundation (`AVAudioFile` + `AVAudioConverter`); here the Ogg
//! Vorbis objects go through symphonia (feature `decode`), the file's first channel is kept
//! (what the converter's default downmix does) and the sample-rate conversion is the
//! windowed sinc of [`crate::sound::minecraft::import::resample`]. Most of the game's
//! sounds are 48 kHz, so it is the usual path. Against Swift's bank built from the same game
//! files the Rust one agrees on every measured root and cent correction and, per sound, to
//! 46..106 dB below the signal (the two decoders and the two resamplers); the exception is a
//! sound whose Vorbis stream goes over full scale: symphonia's decoder clips at +-1.0, Apple's
//! does not, so it is peak-normalised differently.
//!
//! Licensing, as in Swift: the output is built from the user's own game assets, stays in their
//! SoundFont folder and is never committed, shipped or published.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::sound::minecraft as mc;
use crate::sound::minecraft::assets::{AssetFs, MinecraftAssets, Unavailable};
use crate::sound::minecraft::import::Report;
use crate::sound::minecraft::Sidecar;

use crate::host::HostError;

/// The real file system as an [`AssetFs`].
pub struct StdFs;

impl AssetFs for StdFs {
    fn exists(&self, path: &str) -> bool {
        Path::new(path).exists()
    }
    fn list(&self, dir: &str) -> Option<Vec<String>> {
        std::fs::read_dir(dir).ok().map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
    }
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }
}

/// Where the assets are looked for, in order (`MinecraftAssets.defaultRoot`): `$DJ_MINECRAFT_ASSETS`
/// (the only one when set); on macOS PrismLauncher's `~/Library/Application Support/PrismLauncher/assets`;
/// elsewhere PrismLauncher's `~/.local/share/PrismLauncher/assets` (`$XDG_DATA_HOME` honoured),
/// then the vanilla launcher's `~/.minecraft/assets`.
pub fn default_roots_in(
    override_dir: Option<PathBuf>,
    xdg_data_home: Option<PathBuf>,
    home: Option<PathBuf>,
    macos: bool,
) -> Vec<PathBuf> {
    if let Some(d) = override_dir {
        return vec![d];
    }
    let mut v = Vec::new();
    if macos {
        if let Some(h) = &home {
            v.push(h.join("Library/Application Support/PrismLauncher/assets"));
        }
    } else {
        if let Some(x) = xdg_data_home {
            v.push(x.join("PrismLauncher/assets"));
        } else if let Some(h) = &home {
            v.push(h.join(".local/share/PrismLauncher/assets"));
        }
        if let Some(h) = &home {
            v.push(h.join(".minecraft/assets"));
        }
    }
    v
}

/// [`default_roots_in`] with the environment passed in.
pub fn default_roots() -> Vec<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
    };
    default_roots_in(
        var("DJ_MINECRAFT_ASSETS"),
        var("XDG_DATA_HOME"),
        var("HOME"),
        crate::host::platform::IS_MACOS,
    )
}

/// The first candidate that holds an `objects` folder, else the first (so the error names a
/// folder the user can recognise).
pub fn default_root() -> String {
    let roots = default_roots();
    roots
        .iter()
        .find(|r| r.join("objects").exists())
        .or(roots.first())
        .map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_else(|| "~/.minecraft/assets".into())
}

/// `MinecraftAssets.locate(root:index:)` on the real file system.
pub fn locate(root: &str, index: Option<&str>) -> Result<MinecraftAssets, Unavailable> {
    MinecraftAssets::locate(&StdFs, root, index)
}

/// `MinecraftAssets.unavailableReason()` for [`default_root`]: why the assets can't be used,
/// or none when they can.
pub fn assets_unavailable_reason() -> Option<String> {
    MinecraftAssets::unavailable_reason(&StdFs, &default_root())
}

/// `MinecraftBank.Sidecar.load(dir:)`.
pub fn load_sidecar(dir: &Path) -> Option<Sidecar> {
    let text = std::fs::read_to_string(dir.join(mc::SIDECAR_NAME)).ok()?;
    Sidecar::parse(&text)
}

/// `MinecraftBank.isInstalled(dir:)`: the bank and its sidecar are in the SoundFont folder.
pub fn is_installed(dir: &Path) -> bool {
    dir.join(mc::FILE_NAME).exists() && load_sidecar(dir).is_some()
}

/// `MinecraftBank.unavailableReason(dir:)`: why the palette can't play here, or none: the bank
/// was never imported (and whether the game's assets are there to import it from).
pub fn unavailable_reason(dir: &Path) -> Option<String> {
    if is_installed(dir) {
        return None;
    }
    assets_unavailable_reason()
        .or_else(|| Some("not imported yet: run `make minecraft` (djctl minecraft import)".into()))
}

/// `MinecraftImport.decode`: a sound file as mono float at 44.1 kHz, peak-normalised to
/// -1 dBFS. Like AVAudioFile it drops the encoder's lead-in and padding (Ogg Vorbis' granule
/// positions: without that most of the game's sounds start 20 ms early), and like the
/// AVAudioConverter default downmix it keeps only the file's first channel (it does not
/// average).
#[cfg(feature = "decode")]
pub fn decode_sound(path: &Path) -> Result<Vec<f32>, HostError> {
    use crate::sound::minecraft::import::{peak_normalise, resample, SAMPLE_RATE};
    let native = crate::host::decode::decode_native(path, true)?;
    let mut pcm = native.left;
    if let Some(from) = native.rate {
        if (from - SAMPLE_RATE).abs() > 0.5 {
            pcm = resample(&pcm, from, SAMPLE_RATE);
        }
    }
    peak_normalise(&mut pcm);
    Ok(pcm)
}

#[cfg(not(feature = "decode"))]
pub fn decode_sound(_: &Path) -> Result<Vec<f32>, HostError> {
    Err(HostError::Disabled("decode"))
}

/// What [`run`] wrote.
#[derive(Clone, Debug)]
pub struct Imported {
    /// The bank, `minecraft-local.sf2` in the SoundFont folder.
    pub path: PathBuf,
    pub report: Report,
}

fn io(path: &Path, e: std::io::Error) -> HostError {
    HostError::Backend(format!("{}: {e}", path.display()))
}

/// Write `bytes` to `path` whole or not at all (a temp file in the same folder, renamed).
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), HostError> {
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp).map_err(|e| io(&tmp, e))?;
    f.write_all(bytes).map_err(|e| io(&tmp, e))?;
    f.sync_all().map_err(|e| io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| io(path, e))
}

/// `MinecraftImport.run(assets:to:)`: build the bank from `assets` into `dir` (the SoundFont
/// folder), overwriting an earlier build. The caller rescans the folder
/// ([`crate::host::sampler::Library::scan`]), as `DrumKits.rescan()` does in Swift.
pub fn run(assets: &MinecraftAssets, dir: &Path) -> Result<Imported, HostError> {
    if !cfg!(feature = "decode") {
        return Err(HostError::Disabled("decode"));
    }
    let mut decode = |p: &str| decode_sound(Path::new(p)).map_err(|e| e.to_string());
    let built = crate::sound::minecraft::import::build(assets, &StdFs, &mut decode)
        .map_err(|e| HostError::Backend(e.to_string()))?;
    std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    let out = dir.join(mc::FILE_NAME);
    write_atomic(&out, &built.sf2)?;
    write_atomic(
        &dir.join(mc::SIDECAR_NAME),
        built.sidecar.file_text().as_bytes(),
    )?;
    Ok(Imported {
        path: out,
        report: built.report,
    })
}
