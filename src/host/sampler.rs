//! Sampled instruments on the host (the cross-platform half of `Audio/Sampler.swift`).
//!
//! What ports: discovering SoundFont banks, reading their preset lists, building the
//! installed-content description the pure ranking code (`crate::voices`, `crate::sound`)
//! works from, and — behind the `sf2` feature — playing a bank's presets through rustysynth
//! with the same load-time checks `SamplerInstrument.normalize` runs: level-match to the
//! lead voice's reference RMS, turn away silent or out-of-tune sounds; and `SamplerPartLevels`'
//! host half: how loud each part plays on a loaded instrument against its synth voice
//! ([`Sf2Sampler::measure_part_levels`], cached by [`PartLevelCache`]).
//!
//! What cannot: **EXS** (Logic / GarageBand sampler instruments, read by AUSampler),
//! **DLS** (`gs_instruments.dls`, Apple's own General MIDI bank), AUSampler's process-wide
//! lifecycle lock and loaded-instrument ceiling, the `active` / `epoch` handshake of
//! `SamplerBank`'s eviction (it guards AUSampler's teardown; the slot cache itself, with its
//! reference counts and LRU reuse, is ported in `deck::samplers`), and scanning Apple's library
//! folders. On Linux, and in any build without those folders, the EXS pools are simply empty and
//! every category falls through to its General MIDI programs from the SoundFonts, which is the
//! order Swift uses when a Mac has no Logic content installed.
//!
//! A SoundFont file is read and parsed once and shared ([`load_sound_font`]): a preset is a few kB
//! of synthesizer over the one `Arc<SoundFont>`, so a bank of 24 slots on a 215 MB file costs the
//! file once, and an evicted slot that loads again does not read it again.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::sound::{Installed, Sounds};
use crate::voices::bank::{sf2_presets, BankPreset, ReadAt};
use crate::voices::{self, drumkits, VoiceCatalog};

/// A scanned folder as a host reports and uses it ([`Library::for_host`]).
pub struct HostLibrary {
    /// What to hand the engine: None when the folder holds no `.sf2` (synth voices).
    pub library: Option<std::sync::Arc<Library>>,
    /// General SoundFonts found (what a general-purpose palette draws on).
    pub sound_fonts: usize,
    /// Banks beyond those: the local-only ones (`minecraft-local.sf2`).
    pub local_only: usize,
}

/// A file as a [`ReadAt`] (a 200 MB bank costs a few kB of reads for its preset list).
pub struct FileAt(RefCell<File>);

impl FileAt {
    pub fn open(path: &Path) -> std::io::Result<FileAt> {
        Ok(FileAt(RefCell::new(File::open(path)?)))
    }
}

impl ReadAt for FileAt {
    fn read_at(&self, offset: u64, len: usize) -> Option<Vec<u8>> {
        let mut f = self.0.borrow_mut();
        f.seek(SeekFrom::Start(offset)).ok()?;
        let mut buf = Vec::new();
        f.by_ref().take(len as u64).read_to_end(&mut buf).ok()?;
        Some(buf)
    }
}

/// The preset list of a SoundFont file, without its samples (`BankPresets.sf2`).
pub fn bank_presets(path: &Path) -> Vec<BankPreset> {
    FileAt::open(path)
        .map(|f| sf2_presets(&f))
        .unwrap_or_default()
}

/// Where SoundFonts are looked for, in order: `$DJ_SOUNDFONTS` (the only one when set),
/// then `~/.local/share/beats-btw-dj/soundfonts` (`$XDG_DATA_HOME` honoured), then the
/// macOS Application Support folders the Swift app uses. `SamplerSource.soundFontDir`.
pub fn sound_font_dirs() -> Vec<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
    };
    sound_font_dirs_in(var("DJ_SOUNDFONTS"), var("XDG_DATA_HOME"), var("HOME"))
}

/// [`sound_font_dirs`] with the environment passed in.
pub fn sound_font_dirs_in(
    override_dir: Option<PathBuf>,
    xdg_data_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Vec<PathBuf> {
    if let Some(d) = override_dir {
        return vec![d];
    }
    let mut v = Vec::new();
    if let Some(x) = xdg_data_home {
        v.push(x.join("beats-btw-dj/soundfonts"));
    } else if let Some(h) = &home {
        v.push(h.join(".local/share/beats-btw-dj/soundfonts"));
    }
    if let Some(h) = &home {
        v.push(h.join("Library/Application Support/beats-btw-dj/soundfonts"));
        v.push(h.join(concat!("Library/Application Support/", env!("DJRUST_FORMAT_NS"), "/soundfonts")));
    }
    v
}

/// The first existing SoundFont folder, else the first candidate (it is created by whoever
/// installs a bank).
pub fn sound_font_dir() -> PathBuf {
    let dirs = sound_font_dirs();
    dirs.iter()
        .find(|d| d.is_dir())
        .or(dirs.first())
        .cloned()
        .unwrap_or_default()
}

/// What is installed in one SoundFont folder, ready for the pure ranking code.
pub struct Library {
    pub dir: PathBuf,
    pub installed: Installed,
    pub catalog: VoiceCatalog,
    /// Each .sf2's presets, by file name.
    pub presets: BTreeMap<String, Vec<BankPreset>>,
}

impl Library {
    /// Scan `dir`: the files, every bank's presets, the kits among them, the Minecraft
    /// sidecar when its bank is there. Missing folders are an empty library.
    pub fn scan(dir: &Path) -> Library {
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        let mut presets = BTreeMap::new();
        for f in files.iter().filter(|f| f.to_lowercase().ends_with(".sf2")) {
            presets.insert(f.clone(), bank_presets(&dir.join(f)));
        }
        // kits come from every bank, local-only ones included, sorted by name (`DrumKits.banks`)
        let banks: Vec<(String, Vec<BankPreset>)> = presets
            .iter()
            .map(|(f, p)| (dir.join(f).to_string_lossy().into_owned(), p.clone()))
            .collect();
        let dir_s = dir.to_string_lossy().into_owned();
        let minecraft = if files
            .iter()
            .any(|f| f == crate::sound::minecraft::FILE_NAME)
        {
            crate::host::minecraft::load_sidecar(dir)
        } else {
            None
        };
        let installed = Installed {
            dir: dir_s,
            files,
            // Apple's gs_instruments.dls exists only on a Mac, and only AUSampler reads it
            gm_bank: false,
            kits: drumkits::scan(&banks),
            // only asked when the bank is not installed (`MinecraftBank.unavailableReason`)
            minecraft_assets_missing: if minecraft.is_some() {
                None
            } else {
                crate::host::minecraft::assets_unavailable_reason()
            },
            minecraft,
        };
        let catalog = VoiceCatalog::with_installed(BTreeMap::new(), installed.sound_fonts());
        Library {
            dir: dir.to_path_buf(),
            installed,
            catalog,
            presets,
        }
    }

    /// The context the pure functions are asked in.
    pub fn sounds(&self) -> Sounds<'_> {
        Sounds {
            catalog: &self.catalog,
            installed: &self.installed,
        }
    }

    /// The SoundFont banks in voicing order (paths).
    pub fn sound_fonts(&self) -> Vec<String> {
        self.installed.sound_fonts()
    }

    /// Is any `.sf2` in the folder, a local-only bank (`minecraft-local.sf2`) included? A folder
    /// holding only the Minecraft bank has no general SoundFont (`sound_fonts` is empty, which
    /// is what a general-purpose palette needs), but the Minecraft palette is playable there
    /// (`MinecraftBank.isInstalled` reads the folder directly): a host hands the engine its
    /// library whenever this holds, not only when `sound_fonts` does.
    pub fn has_banks(&self) -> bool {
        self.installed
            .files
            .iter()
            .any(|f| f.to_lowercase().ends_with(".sf2"))
    }

    /// What a host does with a scanned folder: the library goes to the engine
    /// ([`EngineConfig::library`](crate::host::deck::EngineConfig::library)) whenever [`has_banks`](Self::has_banks),
    /// not only when a general SoundFont is there. The one gate `djapp` and `djctl play` share, so
    /// a folder holding only the Minecraft bank is a library in both.
    pub fn for_host(self) -> HostLibrary {
        let sound_fonts = self.installed.sound_fonts().len();
        let banks = self
            .installed
            .files
            .iter()
            .filter(|f| f.to_lowercase().ends_with(".sf2"))
            .count();
        HostLibrary {
            library: self.has_banks().then(|| std::sync::Arc::new(self)),
            sound_fonts,
            local_only: banks.saturating_sub(sound_fonts),
        }
    }

    /// `bank:program name` for every preset of a bank.
    pub fn list_presets(&self, file: &str) -> Vec<String> {
        self.presets
            .get(file)
            .map(|v| v.iter().map(format_preset).collect())
            .unwrap_or_default()
    }

    /// For a category: which of its General MIDI programs each installed bank has (the
    /// floor of the fallback ranking), as `(bank file, program, GM name, preset name or
    /// None when the bank lacks it)`.
    pub fn category_programs(&self, category: &str) -> Vec<(String, i64, String, Option<String>)> {
        let programs = self.catalog.gm.get(category).cloned().unwrap_or_default();
        let mut out = Vec::new();
        for path in self.sound_fonts() {
            let file = voices::last_component(&path).to_string();
            for p in &programs {
                let found = self
                    .presets
                    .get(&file)
                    .and_then(|v| v.iter().find(|x| x.bank == 0 && x.program == *p))
                    .map(|x| x.name.clone());
                out.push((file.clone(), *p, voices::general_midi_name(*p), found));
            }
        }
        out
    }
}

/// `128:0 Standard`-style line for a preset.
pub fn format_preset(p: &BankPreset) -> String {
    format!("{:03}:{:03} {}", p.bank, p.program, p.name)
}

#[cfg(feature = "sf2")]
pub use synth::*;

#[cfg(feature = "sf2")]
mod synth {
    use super::*;
    use crate::host::HostError;
    use crate::json::{JsonObject, JsonValue};
    use crate::musicality::loudness::KWeighting;
    use crate::voices::pitch::pitch_class_error;
    use crate::voices::sampler as rules;
    use crate::voices::SamplerSource;
    use crate::wave::SamplerPartLevels;
    use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
    use std::io::BufReader;
    use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
    use std::time::SystemTime;

    fn be(e: impl std::fmt::Display) -> HostError {
        HostError::Backend(e.to_string())
    }

    /// A parsed font and the file it was read from, as it was (`Weak`: a font nothing plays any
    /// more is freed, not kept for the process's life).
    struct CachedFont {
        modified: Option<SystemTime>,
        len: u64,
        font: Weak<SoundFont>,
    }

    /// Every SoundFont parsed and still in use, by the path it was opened by.
    static FONTS: Mutex<BTreeMap<PathBuf, CachedFont>> = Mutex::new(BTreeMap::new());

    /// Load a SoundFont (the whole file: samples are kept in memory, like Swift's
    /// `StreamFromDisk` off, so renders do not depend on disk timing), once per file: while any
    /// sampler holds it, the next ask for the same path returns the same `Arc<SoundFont>`
    /// (`Synthesizer::new` clones it), as long as the file's modification time and size are
    /// those it was read with, so a reinstalled bank is read again. A file that has vanished
    /// since is no reason to refuse a font that is still in memory. Two threads asking for one font
    /// at once read it once (the second waits for the first); a different font waits too.
    pub fn load_sound_font(path: &Path) -> Result<Arc<SoundFont>, HostError> {
        let mut fonts = FONTS.lock().unwrap_or_else(PoisonError::into_inner);
        let now = std::fs::metadata(path)
            .ok()
            .map(|m| (m.modified().ok(), m.len()));
        if let Some(c) = fonts.get(path) {
            if now.is_none_or(|n| n == (c.modified, c.len)) {
                if let Some(font) = c.font.upgrade() {
                    return Ok(font);
                }
            }
        }
        let mut f = File::open(path).map_err(be)?;
        let meta = f.metadata().map_err(be)?;
        // (buffered: the parser reads a chunk header and a field at a time)
        let font = SoundFont::new(&mut BufReader::new(&mut f))
            .map(Arc::new)
            .map_err(be)?;
        fonts.retain(|_, c| c.font.strong_count() > 0);
        fonts.insert(
            path.to_path_buf(),
            CachedFont {
                modified: meta.modified().ok(),
                len: meta.len(),
                font: Arc::downgrade(&font),
            },
        );
        Ok(font)
    }

    /// One note to render: `at` frames in, held `frames`.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct NoteEvent {
        pub at: usize,
        pub frames: usize,
        pub key: u8,
        pub velocity: u8,
    }

    /// What `normalize` last measured (`SamplerInstrument.lastCheck`).
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Check {
        pub rms_db: f64,
        pub cents: f64,
        pub held: bool,
    }

    /// One SoundFont preset, rendered by hand (`SamplerInstrument`): MIDI in at exact
    /// frames, stereo float out, a gain set by [`Sf2Sampler::normalize`].
    pub struct Sf2Sampler {
        pub source: SamplerSource,
        synth: Synthesizer,
        /// The parsed bank the synthesizer plays from, shared with every sampler of the same file
        /// ([`load_sound_font`]).
        font: Arc<SoundFont>,
        sample_rate: u32,
        /// Output gain; `normalize` sets it before the sound is used.
        pub gain: f64,
        channel: i32,
        pub last_check: Option<Check>,
        /// What [`measure_part_levels`](Sf2Sampler::measure_part_levels) found: how loud each part
        /// (`SamplerPartLevels::GROUPS`) plays on this instrument against its synth voice,
        /// K-weighted, at the instrument's `gain` (dB). Empty until measured (and for kits)
        /// (`partLevelDB`).
        pub part_level_db: BTreeMap<String, f64>,
    }

    impl Sf2Sampler {
        /// Open `source` (an `Sf2` melodic program or a `Kit`) at `sample_rate` (≥ 16 kHz).
        /// EXS and Apple's GM bank are Apple-only and refused with that said.
        pub fn open(source: &SamplerSource, sample_rate: u32) -> Result<Sf2Sampler, HostError> {
            let (path, bank, program) = match source {
                SamplerSource::Sf2(p, n) => (p, 0, *n),
                SamplerSource::Kit {
                    path,
                    bank,
                    program,
                } => (path, *bank, *program),
                SamplerSource::Exs(_) => {
                    return Err(HostError::Backend(
                        "EXS needs AUSampler (macOS only)".into(),
                    ))
                }
                SamplerSource::Gm(_) => {
                    return Err(HostError::Backend(
                        "Apple's gs_instruments.dls needs AUSampler (macOS only)".into(),
                    ))
                }
            };
            let font = load_sound_font(Path::new(path))?;
            Self::from_font(source.clone(), font, bank, program, sample_rate)
        }

        /// A preset of an already loaded font (banks are shared between samplers).
        pub fn from_font(
            source: SamplerSource,
            font: Arc<SoundFont>,
            bank: i64,
            program: i64,
            sample_rate: u32,
        ) -> Result<Sf2Sampler, HostError> {
            if !font.get_presets().iter().any(|p| {
                p.get_bank_number() as i64 == bank && p.get_patch_number() as i64 == program
            }) {
                return Err(HostError::Backend(format!(
                    "no preset {bank}:{program} in the bank"
                )));
            }
            let mut settings = SynthesizerSettings::new(sample_rate as i32);
            // AUSampler adds no reverb or chorus of its own; the deck's sends do that
            settings.enable_reverb_and_chorus = false;
            let mut synth = Synthesizer::new(&font, &settings).map_err(be)?;
            // a percussion bank plays on the percussion channel; a melodic bank on channel 0
            let percussion = bank >= 128;
            let channel = if percussion { 9 } else { 0 };
            if !percussion {
                synth.process_midi_message(channel, 0xB0, 0, bank as i32);
            }
            synth.process_midi_message(channel, 0xC0, program as i32, 0);
            Ok(Sf2Sampler {
                source,
                synth,
                font,
                sample_rate,
                gain: 1.0,
                channel,
                last_check: None,
                part_level_db: BTreeMap::new(),
            })
        }

        pub fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        /// The parsed SoundFont this sampler plays a preset of; the same `Arc` for every sampler
        /// opened on the file while one of them lives ([`load_sound_font`]).
        pub fn font(&self) -> &Arc<SoundFont> {
            &self.font
        }

        pub fn note_on(&mut self, key: u8, velocity: u8) {
            self.synth
                .note_on(self.channel, key as i32, velocity as i32);
        }

        pub fn note_off(&mut self, key: u8) {
            self.synth.note_off(self.channel, key as i32);
        }

        /// MIDI "all notes off" (CC 123): every key is released and the voices ring out their
        /// release tails, as AUSampler's do. `silence` is the hard one (CC 120 and a reset).
        pub fn all_notes_off(&mut self) {
            self.synth.note_off_all(false);
        }

        /// Silence everything and reset the controllers (`send(0xB0, 123)` + `au.reset()`).
        pub fn silence(&mut self) {
            self.synth.note_off_all(true);
            self.synth.reset_all_controllers_channel(self.channel);
        }

        /// Render `left.len()` frames straight into `left` / `right` (× gain not applied), with
        /// no allocation: the deck's render path (`render_adding` makes scratch of its own).
        pub fn render_into(&mut self, left: &mut [f32], right: &mut [f32]) {
            self.synth.render(left, right);
        }

        /// Render `frames` and add them, × gain, into `left` / `right`
        /// (`renderAdding`). `right = None` adds the mid to `left`, as Swift's mono callers
        /// get it.
        pub fn render_adding(
            &mut self,
            frames: usize,
            left: &mut [f32],
            right: Option<&mut [f32]>,
        ) {
            let mut l = vec![0f32; frames];
            let mut r = vec![0f32; frames];
            self.synth.render(&mut l, &mut r);
            let g = self.gain as f32;
            match right {
                Some(out_r) => {
                    for i in 0..frames {
                        left[i] += g * l[i];
                        out_r[i] += g * r[i];
                    }
                }
                None => {
                    for i in 0..frames {
                        left[i] += g * 0.5 * (l[i] + r[i]);
                    }
                }
            }
        }

        /// Play `notes` (frame-exact MIDI) for `frames` frames into stereo buffers, × gain.
        pub fn render_notes(&mut self, notes: &[NoteEvent], frames: usize) -> (Vec<f32>, Vec<f32>) {
            let mut events: Vec<(usize, bool, u8, u8)> = Vec::new();
            for n in notes {
                events.push((n.at, true, n.key, n.velocity));
                events.push((n.at + n.frames, false, n.key, 0));
            }
            // note-offs before note-ons at one frame, as phraseRMS orders them
            events.sort_by_key(|e| (e.0, e.1));
            let (mut l, mut r) = (vec![0f32; frames], vec![0f32; frames]);
            let mut done = 0;
            let mut i = 0;
            while done < frames {
                while i < events.len() && events[i].0 <= done {
                    let (_, on, key, vel) = events[i];
                    if on {
                        self.note_on(key, vel);
                    } else {
                        self.note_off(key);
                    }
                    i += 1;
                }
                let next = events.get(i).map(|e| e.0).unwrap_or(frames).min(frames);
                let n = (next - done).clamp(1, frames - done);
                let (a, b) = (&mut l[done..done + n], &mut r[done..done + n]);
                self.render_adding(n, a, Some(b));
                done += n;
            }
            (l, r)
        }

        /// RMS (dB, mid) of `phrase` at velocity 100 over `seconds`, then silenced
        /// (`phraseRMS`). Gain applies, as in Swift.
        fn phrase_rms(&mut self, phrase: &[rules::PhraseNote], seconds: f64) -> f64 {
            let sr = self.sample_rate as f64;
            let notes: Vec<NoteEvent> = phrase
                .iter()
                .map(|p| NoteEvent {
                    at: (p.at * sr).round() as usize,
                    frames: ((p.at + p.seconds) * sr).round() as usize
                        - (p.at * sr).round() as usize,
                    key: p.note,
                    velocity: 100,
                })
                .collect();
            let total = (seconds * sr) as usize;
            let (l, r) = self.render_notes(&notes, total);
            self.silence();
            let sum: f64 = l
                .iter()
                .zip(&r)
                .map(|(a, b)| {
                    let m = 0.5 * (*a as f64 + *b as f64);
                    m * m
                })
                .sum();
            rules::rms_db(sum, total)
        }

        /// Off the audio thread, before use: play the reference phrase, measure it, set
        /// `gain` to land on [`rules::TARGET_RMS_DB`], then check tuning on a held A3.
        /// Returns false for a sound not fit to play — silent, or out of tune past
        /// [`rules::MAX_DETUNE_CENTS`]. `tuning: false` (a local-only bank) checks the level
        /// only. `SamplerInstrument.normalize`.
        pub fn normalize(&mut self, tuning: bool) -> bool {
            self.gain = 1.0;
            let rms = self.phrase_rms(&rules::REFERENCE_PHRASE, rules::REFERENCE_SECONDS);
            self.gain = rules::gain_for(rms, rules::TARGET_RMS_DB);
            if !tuning {
                self.last_check = Some(Check {
                    rms_db: rms,
                    cents: 0.0,
                    held: true,
                });
                return rules::tonal_fit(rms, false, 0.0, 0.0);
            }
            // tuning: one held A3, measured 0.1–0.4 s in
            let sr = self.sample_rate as f64;
            let total = (0.4 * sr) as usize;
            let (l, r) = self.render_notes(
                &[NoteEvent {
                    at: 0,
                    frames: total + 1,
                    key: 57,
                    velocity: 100,
                }],
                total,
            );
            self.silence();
            let mid: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect();
            let (cents, _, level) = pitch_class_error(&mid[(0.1 * sr) as usize..], 57, sr);
            self.last_check = Some(Check {
                rms_db: rms,
                cents,
                held: level > 0.0,
            });
            rules::tonal_fit(rms, true, level, cents)
        }

        /// `normalize` for a drum kit: level-matched on the kit phrase to
        /// [`rules::KIT_TARGET_RMS_DB`], and turned away if its kick, snare or closed hat is
        /// silent. No tuning check. `normalizeKit`.
        pub fn normalize_kit(&mut self) -> bool {
            self.gain = 1.0;
            let rms = self.phrase_rms(&rules::kit_phrase(), rules::KIT_SECONDS);
            let mut quietest = Vec::new();
            for note in rules::KIT_VOICES {
                let one = [rules::PhraseNote {
                    note,
                    at: 0.0,
                    seconds: 0.3,
                }];
                quietest.push(self.phrase_rms(&one, rules::KIT_VOICE_SECONDS));
            }
            let quietest = rules::quietest_of(quietest);
            self.gain = rules::gain_for(rms, rules::KIT_TARGET_RMS_DB);
            self.last_check = Some(Check {
                rms_db: rms.min(quietest),
                cents: 0.0,
                held: quietest > rules::SILENT_RMS_DB,
            });
            rules::kit_fit(rms, quietest)
        }

        /// Off the audio thread, after `normalize` and before use: each part probe's K-weighted
        /// level here (unity gain, the mean of [`SamplerPartLevels::PASSES`] passes, from `cache`
        /// when it has this sound at this rate) plus `gain`, less the synth voice's on the same
        /// probe, kept in [`part_level_db`](Self::part_level_db). `SamplerInstrument
        /// .measurePartLevels`.
        pub fn measure_part_levels(&mut self, cache: Option<&PartLevelCache>) {
            let sr = self.sample_rate as f64;
            let key = part_level_cache_key(&self.source, self.sample_rate);
            let mut raw = key.as_deref().and_then(|k| cache?.levels(k));
            let whole = |m: &BTreeMap<String, f64>| {
                m.len() == SamplerPartLevels::GROUPS.len()
                    && SamplerPartLevels::GROUPS.iter().all(|g| m.contains_key(*g))
            };
            if !raw.as_ref().is_some_and(whole) {
                let mut m = BTreeMap::new();
                for g in SamplerPartLevels::GROUPS {
                    let (notes, seconds) = SamplerPartLevels::probe(g);
                    m.insert(g.to_string(), self.unity_level(&notes, seconds));
                }
                if let (Some(k), Some(c)) = (key.as_deref(), cache) {
                    c.store(&m, k);
                }
                raw = Some(m);
            }
            let raw = raw.unwrap_or_default();
            let gain_db = 20.0 * self.gain.log10();
            let mut out = BTreeMap::new();
            for g in SamplerPartLevels::GROUPS {
                out.insert(
                    g.to_string(),
                    raw[g] + gain_db - synth_level_db(g, sr),
                );
            }
            self.part_level_db = out;
        }

        /// K-weighted mean square (dB) of `phrase` (`(note, at s, length s)`) at velocity 100 and
        /// unity gain over `seconds`, averaged over [`SamplerPartLevels::PASSES`], the unit
        /// silenced and reset after each (`unityLevel`; mono, as `renderAdding(frames, left, nil)`
        /// plays it).
        fn unity_level(&mut self, phrase: &[(i64, f64, f64)], seconds: f64) -> f64 {
            let saved = self.gain;
            self.gain = 1.0;
            let sr = self.sample_rate as f64;
            let total = (seconds * sr) as usize;
            let notes: Vec<NoteEvent> = phrase
                .iter()
                .map(|&(note, at, length)| {
                    let on = (at * sr).round() as usize;
                    NoteEvent {
                        at: on,
                        frames: ((at + length) * sr).round() as usize - on,
                        key: note as u8,
                        velocity: 100,
                    }
                })
                .collect();
            let mut mean = 0.0;
            for _ in 0..SamplerPartLevels::PASSES {
                let mut k = KWeighting::new(sr);
                let (l, r) = self.render_notes(&notes, total);
                let mut sum = 0.0;
                for (a, b) in l.iter().zip(&r) {
                    let y = k.process((0.5f32 * (a + b)) as f64);
                    sum += y * y;
                }
                self.silence();
                mean += 10.0 * (sum / total.max(1) as f64).max(1e-20).log10();
            }
            self.gain = saved;
            mean / SamplerPartLevels::PASSES as f64
        }
    }

    /// `SamplerPartLevels.synthLevelDB`: the K-weighted level (dB) of `group`'s plain synth voice
    /// on its probe at `sample_rate`, computed once per rate.
    pub fn synth_level_db(group: &str, sample_rate: f64) -> f64 {
        static CACHE: OnceLock<Mutex<BTreeMap<(u64, String), f64>>> = OnceLock::new();
        let cache = CACHE.get_or_init(Default::default);
        let key = (sample_rate.to_bits(), group.to_string());
        if let Some(l) = cache.lock().unwrap().get(&key) {
            return *l;
        }
        let l = SamplerPartLevels::synth_level_db(group, sample_rate);
        cache.lock().unwrap().insert(key, l);
        l
    }

    /// What a measurement is cached under: the sound (its file's path, size and modification
    /// time, so a reinstalled sound is measured again), the rate and the probes' `VERSION`. None
    /// for kits (not measured) and sounds whose file can't be read
    /// (`SamplerPartLevels.cacheKey`).
    pub fn part_level_cache_key(source: &SamplerSource, sample_rate: u32) -> Option<String> {
        let (file, tag) = match source {
            SamplerSource::Exs(p) => (p.clone(), "exs".to_string()),
            SamplerSource::Gm(n) => (GM_BANK.to_string(), format!("gm#{n}")),
            SamplerSource::Sf2(p, n) => (p.clone(), format!("sf2#{n}")),
            SamplerSource::Kit { .. } => return None,
        };
        let meta = std::fs::metadata(&file).ok()?;
        let size = meta.len() as i64;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        Some(format!(
            "{tag}|{file}|{size}|{mtime}|{sample_rate}|v{}",
            SamplerPartLevels::VERSION
        ))
    }

    /// Apple's General MIDI bank (`SamplerSource.gmBank`), the file a `gm#n` key names.
    const GM_BANK: &str =
        "/System/Library/Components/CoreAudio.component/Contents/Resources/gs_instruments.dls";

    /// Part levels measured at unity gain (dB by group), by [`part_level_cache_key`], in memory
    /// and, when it has a file, in a JSON file read on first use and rewritten on each store.
    /// Measured levels only: the `gain` and the synth reference are applied fresh at every load
    /// (`SamplerPartLevels.Cache`).
    ///
    /// The file is the levels *this* synth measured: the Swift app's cache
    /// (`~/Library/Application Support/DJApp/sampler-part-levels.json`) holds AUSampler's for the
    /// same keys, so it is never the default here: only `$DJ_SAMPLER_PART_CACHE` (a path; empty or
    /// unset: in memory only) names one.
    pub struct PartLevelCache {
        pub path: Option<PathBuf>,
        entries: Mutex<Option<BTreeMap<String, BTreeMap<String, f64>>>>,
    }

    impl PartLevelCache {
        /// `path` None: in memory only.
        pub fn new(path: Option<PathBuf>) -> PartLevelCache {
            PartLevelCache {
                path,
                entries: Mutex::new(None),
            }
        }

        /// The process's cache (`.shared`): in memory, and in `$DJ_SAMPLER_PART_CACHE` when that
        /// names a file.
        pub fn shared() -> Arc<PartLevelCache> {
            static SHARED: OnceLock<Arc<PartLevelCache>> = OnceLock::new();
            SHARED
                .get_or_init(|| {
                    let path = std::env::var_os("DJ_SAMPLER_PART_CACHE")
                        .filter(|p| !p.is_empty())
                        .map(PathBuf::from);
                    Arc::new(PartLevelCache::new(path))
                })
                .clone()
        }

        pub fn levels(&self, key: &str) -> Option<BTreeMap<String, f64>> {
            let mut g = self.entries.lock().unwrap();
            g.get_or_insert_with(|| self.load()).get(key).cloned()
        }

        pub fn store(&self, levels: &BTreeMap<String, f64>, key: &str) {
            let mut g = self.entries.lock().unwrap();
            let all = g.get_or_insert_with(|| self.load());
            all.insert(key.to_string(), levels.clone());
            let Some(path) = &self.path else { return };
            let json = JsonValue::Object(JsonObject::from_pairs(all.iter().map(|(k, v)| {
                (
                    k.as_str(),
                    JsonValue::Object(JsonObject::from_pairs(
                        v.iter().map(|(g, l)| (g.as_str(), JsonValue::Number(*l))),
                    )),
                )
            })));
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            // (written beside and renamed over, so a reader never sees half a file)
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, json.js_stringified()).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }

        fn load(&self) -> BTreeMap<String, BTreeMap<String, f64>> {
            let Some(path) = &self.path else {
                return BTreeMap::new();
            };
            let Ok(text) = std::fs::read_to_string(path) else {
                return BTreeMap::new();
            };
            let Ok(JsonValue::Object(o)) = JsonValue::parse(&text) else {
                return BTreeMap::new();
            };
            let mut all = BTreeMap::new();
            for k in o.keys() {
                let Some(JsonValue::Object(v)) = o.get(k) else {
                    continue;
                };
                let levels: BTreeMap<String, f64> = v
                    .keys()
                    .filter_map(|g| Some((g.to_string(), v.get(g)?.as_f64()?)))
                    .collect();
                all.insert(k.to_string(), levels);
            }
            all
        }
    }

    /// `SamplerSource.isAvailable`: the file the source names is on disk.
    pub fn source_available(source: &SamplerSource) -> bool {
        match source {
            SamplerSource::Exs(p)
            | SamplerSource::Sf2(p, _)
            | SamplerSource::Kit { path: p, .. } => Path::new(p).exists(),
            SamplerSource::Gm(_) => Path::new(GM_BANK).exists(),
        }
    }

    /// Why a sound that loaded was turned away by its load-time checks, in the words
    /// `SamplerBank.loadFailure(for:)` gives (`last` is what `normalize` / `normalize_kit` left in
    /// [`Sf2Sampler::last_check`]): a kit without a kick, snare or hat; a tonal sound that is
    /// silent, or too short to tune, or out of tune.
    pub fn check_failure(source: &SamplerSource, last: Option<Check>) -> String {
        if source.is_kit() {
            return format!(
                "a kit without a kick, snare or hat (quietest {:.1} dB)",
                last.map_or(-200.0, |c| c.rms_db)
            );
        }
        if last.is_some_and(|c| c.rms_db <= rules::SILENT_RMS_DB) {
            let rms = last.map_or("?".to_string(), |c| format!("{:.1}", c.rms_db));
            format!("silent ({rms} dB)")
        } else if last.is_some_and(|c| !c.held) {
            "silent 0.1 s into a held note: too short to tune".to_string()
        } else {
            let cents = last.map_or("?".to_string(), |c| format!("{:+.0}", c.cents));
            format!(
                "out of tune ({cents} cents, limit {})",
                rules::MAX_DETUNE_CENTS as i64
            )
        }
    }

    /// One source opened and checked as `SamplerBank.slot(for:)` does once it has a slot for it:
    /// opened, level-matched (a kit on the kit phrase), and turned away with Swift's wording
    /// ([`check_failure`]; `load failed: <error>` when it will not open). A tonal sound that came
    /// up silent is opened once more before it is turned away (Swift's retry, which an AUSampler
    /// quirk needed: a fresh unit is what any later load would have tried; rustysynth renders the
    /// same twice, so here it changes nothing but costs little, the font being shared).
    /// `opens` counts the openings.
    pub fn load_checked(
        source: &SamplerSource,
        sample_rate: u32,
        opens: &mut u64,
    ) -> Result<Sf2Sampler, String> {
        let mut open = || {
            *opens += 1;
            Sf2Sampler::open(source, sample_rate)
        };
        let mut inst = open().map_err(|e| format!("load failed: {e}"))?;
        let tuning = !source.is_local_only();
        let mut fits = source.is_kit() || inst.normalize(tuning);
        let silent = inst
            .last_check
            .is_some_and(|c| c.rms_db <= rules::SILENT_RMS_DB || !c.held);
        if !fits && silent {
            if let Ok(again) = open() {
                inst = again;
                fits = inst.normalize(tuning);
            }
        }
        if source.is_kit() {
            if !inst.normalize_kit() {
                return Err(check_failure(source, inst.last_check));
            }
        } else if !fits {
            return Err(check_failure(source, inst.last_check));
        }
        Ok(inst)
    }

    /// Open the first source of `sources` that loads and passes its load-time checks — what
    /// `SamplerBank.slot(for:)` does down a ranked list, less the slots. Sources this host cannot
    /// play (EXS, Apple's GM) or that are not on disk are skipped with the reason kept, in the
    /// words of [`SamplerBank::load_failure`](crate::host::deck::samplers::SamplerBank::load_failure).
    pub fn open_first_fit(
        sources: &[SamplerSource],
        sample_rate: u32,
    ) -> Result<(Sf2Sampler, Vec<String>), Vec<String>> {
        let mut why = Vec::new();
        let mut opens = 0;
        for s in sources {
            if !source_available(s) {
                why.push(format!("{s}: not on disk"));
                continue;
            }
            match load_checked(s, sample_rate, &mut opens) {
                Ok(inst) => return Ok((inst, why)),
                Err(e) => why.push(format!("{s}: {e}")),
            }
        }
        Err(why)
    }

    /// A tiny valid SoundFont for tests and smoke checks: one looped sine sample at middle C
    /// (key 60, tuned to within a tenth of a cent), and these presets, all playing it —
    /// `(name, bank, program)`. Bank 128 presets are percussion kits (the same sine on every
    /// key, which is enough to level-check one). No download, no licence: it is synthesised.
    pub fn synthetic_sf2(presets: &[(&str, u16, u16)]) -> Vec<u8> {
        synthetic_sf2_tuned(presets, 0)
    }

    /// [`synthetic_sf2`] with the sample played `fine_tune_cents` (±99) off pitch, to
    /// exercise the detune check.
    pub fn synthetic_sf2_tuned(presets: &[(&str, u16, u16)], fine_tune_cents: i16) -> Vec<u8> {
        const N: usize = 8428; // 50 cycles = 261.62 Hz at 44.1 kHz (0.08 cents flat of middle C)
        let mut pcm: Vec<i16> = (0..2 * N)
            .map(|i| {
                (12000.0 * (2.0 * std::f64::consts::PI * 50.0 * i as f64 / N as f64).sin()) as i16
            })
            .collect();
        pcm.resize(pcm.len() + 46, 0); // the guard zeros SF2 requires
        let le16 = |v: u16| v.to_le_bytes().to_vec();
        let le32 = |v: u32| v.to_le_bytes().to_vec();
        let name = |s: &str| {
            let mut n = [0u8; 20];
            n[..s.len().min(19)].copy_from_slice(&s.as_bytes()[..s.len().min(19)]);
            n.to_vec()
        };
        let chunk = |id: &[u8; 4], body: &[u8]| {
            let mut v = id.to_vec();
            v.extend(le32(body.len() as u32));
            v.extend_from_slice(body);
            if body.len() % 2 == 1 {
                v.push(0);
            }
            v
        };
        let list = |kind: &[u8; 4], parts: Vec<Vec<u8>>| {
            let mut body = kind.to_vec();
            for p in parts {
                body.extend(p);
            }
            chunk(b"LIST", &body)
        };
        let info = list(
            b"INFO",
            vec![
                chunk(b"ifil", &[2, 0, 1, 0]),
                chunk(b"isng", b"EMU8000\0"),
                chunk(b"INAM", b"djhost synthetic\0\0"),
            ],
        );
        let mut smpl = Vec::new();
        for s in &pcm {
            smpl.extend(s.to_le_bytes());
        }
        let sdta = list(b"sdta", vec![chunk(b"smpl", &smpl)]);

        let mut phdr = Vec::new();
        let mut pbag = Vec::new();
        let mut pgen = Vec::new();
        for (i, (nm, bank, program)) in presets.iter().enumerate() {
            phdr.extend(name(nm));
            phdr.extend(le16(*program));
            phdr.extend(le16(*bank));
            phdr.extend(le16(i as u16)); // bag index
            phdr.extend([0u8; 12]);
            pbag.extend(le16((i * 2) as u16));
            pbag.extend(le16(0));
            // pgen: key range (43) first when present, instrument (41) last
            pgen.extend(le16(43));
            pgen.extend([0u8, 127u8]);
            pgen.extend(le16(41));
            pgen.extend(le16(0));
        }
        phdr.extend(name("EOP"));
        phdr.extend(le16(0));
        phdr.extend(le16(0));
        phdr.extend(le16(presets.len() as u16));
        phdr.extend([0u8; 12]);
        pbag.extend(le16((presets.len() * 2) as u16));
        pbag.extend(le16(0));
        pgen.extend([0u8; 4]);
        let pmod = [0u8; 10].to_vec();

        let mut inst = name("Sine");
        inst.extend(le16(0));
        inst.extend(name("EOI"));
        inst.extend(le16(1));
        let mut ibag = Vec::new();
        ibag.extend(le16(0));
        ibag.extend(le16(0));
        ibag.extend(le16(if fine_tune_cents != 0 { 4 } else { 3 }));
        ibag.extend(le16(0));
        let imod = [0u8; 10].to_vec();
        let mut igen = Vec::new();
        igen.extend(le16(56)); // scaleTuning: 100 cents per key
        igen.extend(le16(100));
        if fine_tune_cents != 0 {
            igen.extend(le16(52)); // fineTune
            igen.extend(fine_tune_cents.to_le_bytes());
        }
        igen.extend(le16(54)); // sampleModes: loop
        igen.extend(le16(1));
        igen.extend(le16(53)); // sampleID
        igen.extend(le16(0));
        igen.extend([0u8; 4]);

        let mut shdr = name("sine");
        shdr.extend(le32(0));
        shdr.extend(le32((2 * N) as u32));
        shdr.extend(le32(8));
        shdr.extend(le32((8 + N) as u32));
        shdr.extend(le32(44100));
        shdr.push(60);
        shdr.push(0);
        shdr.extend(le16(0));
        shdr.extend(le16(1));
        shdr.extend(name("EOS"));
        shdr.extend([0u8; 26]);

        let pdta = list(
            b"pdta",
            vec![
                chunk(b"phdr", &phdr),
                chunk(b"pbag", &pbag),
                chunk(b"pmod", &pmod),
                chunk(b"pgen", &pgen),
                chunk(b"inst", &inst),
                chunk(b"ibag", &ibag),
                chunk(b"imod", &imod),
                chunk(b"igen", &igen),
                chunk(b"shdr", &shdr),
            ],
        );
        let mut body = b"sfbk".to_vec();
        body.extend(info);
        body.extend(sdta);
        body.extend(pdta);
        let mut out = b"RIFF".to_vec();
        out.extend(le32(body.len() as u32));
        out.extend(body);
        out
    }
}
