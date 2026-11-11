//! Live output through cpal (CoreAudio on macOS, ALSA on Linux, WASAPI on Windows): the
//! stand-in for `AVAudioEngine`'s output node, `AudioDevices.swift` and `CueRouting.swift`.
//! The stream callback runs [`Interleaver::fill`] over the mixer's [`Renderer`], which is the
//! same code every offline test drives, so all but "open a device" is covered without one.
//!
//! The headphone cue follows `CueRouting.swift`: channel pairs of one multichannel device
//! ([`Routing::Pairs`], e.g. the Inpulse's sound card: master 1/2, cue 3/4), or, where the
//! headphones are a second device, a second stream fed through a ring ([`Routing::CueDevice`]).
//! Different clocks drift; the ring just drops or pads. Apple-only differences: none of
//! `AudioObjectSetPropertyData`'s device switching (a new `Output` is opened instead), and no
//! automatic follow of the system default device.
//!
//! Every stream reports into a [`StreamHealth`] (`crate::host::device_watch`): its data callback
//! beats, its error callback leaves a fatal error there. The host watches it and rebuilds the
//! stream when the device goes (a Bluetooth headset dropping out, a USB interface pulled).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample, Stream, StreamConfig};
use rtrb::RingBuffer;

use std::sync::Arc;

use crate::host::deck::output::{Interleaver, Routing};
use crate::host::deck::Renderer;
use crate::host::device_watch::{Severity, StreamHealth};
use crate::host::HostError;

/// How a backend error is taken: the device gone or the stream invalidated is fatal (the host
/// rebuilds); an xrun, or the system default rerouting a default-following stream, is not.
pub fn severity(kind: cpal::ErrorKind) -> Severity {
    use cpal::ErrorKind as K;
    match kind {
        K::Xrun | K::DeviceChanged => Severity::Notice,
        _ => Severity::Fatal,
    }
}

/// The error callback every stream gets: say it, and leave it in `health`.
fn report(health: &StreamHealth, what: &'static str, e: &cpal::Error) {
    match severity(e.kind()) {
        Severity::Fatal => {
            eprintln!("djhost {what}: {e} ({:?}): the stream is done", e.kind());
            health.fault(&format!("{e}"));
        }
        Severity::Notice => health.notice(),
    }
}

fn be(e: impl std::fmt::Display) -> HostError {
    HostError::Backend(e.to_string())
}

fn name_of(d: &cpal::Device) -> String {
    d.description()
        .map(|x| x.name().to_string())
        .unwrap_or_else(|_| "(unnamed)".into())
}

/// A renderer the output stream borrows instead of owning, so the app can move it to another
/// device while it runs (the control socket's `audio.output`): drop the old [`Running`], start
/// a new one on the same slot. The callback only `try_lock`s: while the slot is held (a
/// switch) or empty it plays silence, never waits.
pub type SharedRenderer = std::sync::Arc<std::sync::Mutex<Option<Renderer>>>;

/// An input strip's ring writer, kept apart from any one capture stream so the strip can be
/// fed by another device later (the control socket's `audio.input`). As with
/// [`SharedRenderer`], the callback only `try_lock`s and drops the buffer when it can't.
pub type SharedFeed = std::sync::Arc<std::sync::Mutex<Option<rtrb::Producer<f32>>>>;

/// Where an output stream's samples come from.
enum Source {
    Owned(Renderer),
    Shared(SharedRenderer),
}

/// Names of the output devices the platform reports.
pub fn output_names() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|it| it.map(|d| name_of(&d)).collect())
        .unwrap_or_default()
}

/// The platform's default output device, if any.
pub fn default_output_name() -> Option<String> {
    cpal::default_host()
        .default_output_device()
        .map(|d| name_of(&d))
}

/// An output device and the config it will run at. Open it first: the mixer is built at the
/// device's sample rate.
pub struct Output {
    device: cpal::Device,
    pub name: String,
    config: StreamConfig,
    format: cpal::SampleFormat,
}

impl Output {
    /// The default output device, or the first whose name contains `name` (case-insensitive).
    /// `sample_rate`: ask for this rate when the device supports it, else its default.
    pub fn open(name: Option<&str>, sample_rate: Option<u32>) -> Result<Output, HostError> {
        let host = cpal::default_host();
        let device = match name {
            None => host
                .default_output_device()
                .ok_or_else(|| be("no default output device"))?,
            Some(want) => {
                let want = want.to_lowercase();
                host.output_devices()
                    .map_err(be)?
                    .find(|d| name_of(d).to_lowercase().contains(&want))
                    .ok_or_else(|| be(format!("no output device named like {want:?}")))?
            }
        };
        let mut supported = device.default_output_config().map_err(be)?;
        if let Some(rate) = sample_rate {
            if let Ok(mut ranges) = device.supported_output_configs() {
                if let Some(s) = ranges.find_map(|r| {
                    (r.channels() == supported.channels()
                        && r.sample_format() == supported.sample_format())
                    .then(|| r.try_with_sample_rate(rate))
                    .flatten()
                }) {
                    supported = s;
                }
            }
        }
        Ok(Output {
            name: name_of(&device),
            config: supported.config(),
            format: supported.sample_format(),
            device,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.config.channels as usize
    }

    /// Run `renderer` on this device. `cue_device` (with [`Routing::CueDevice`]) is the second
    /// device's stream; its rate must equal this one's.
    pub fn start(
        self,
        renderer: Renderer,
        routing: Routing,
        cue_device: Option<Output>,
    ) -> Result<Running, HostError> {
        self.start_source(Source::Owned(renderer), routing, cue_device)
    }

    /// [`Output::start`] on a [`SharedRenderer`]: the stream renders whatever the slot holds.
    pub fn start_shared(
        self,
        slot: SharedRenderer,
        routing: Routing,
        cue_device: Option<Output>,
    ) -> Result<Running, HostError> {
        self.start_source(Source::Shared(slot), routing, cue_device)
    }

    fn start_source(
        self,
        renderer: Source,
        routing: Routing,
        cue_device: Option<Output>,
    ) -> Result<Running, HostError> {
        if !routing.fits(self.channels()) {
            return Err(be(format!(
                "{} has {} output channels: not enough for {routing:?}",
                self.name,
                self.channels()
            )));
        }
        let health = Arc::new(StreamHealth::new());
        let mut cue_stream = None;
        let ring = match (&routing, cue_device) {
            (Routing::CueDevice, Some(second)) => {
                if second.sample_rate() != self.sample_rate() {
                    return Err(be(format!(
                        "{} runs at {} Hz, {} at {} Hz",
                        second.name,
                        second.sample_rate(),
                        self.name,
                        self.sample_rate()
                    )));
                }
                let (tx, rx) = RingBuffer::<f32>::new(self.sample_rate() as usize / 2 * 2);
                cue_stream = Some(second.build_cue(rx, health.clone())?);
                Some(tx)
            }
            (Routing::CueDevice, None) => {
                return Err(be("Routing::CueDevice needs a second device"))
            }
            _ => None,
        };
        let stream = match self.format {
            cpal::SampleFormat::F32 => self.build::<f32>(renderer, routing, ring, health.clone())?,
            cpal::SampleFormat::I16 => self.build::<i16>(renderer, routing, ring, health.clone())?,
            other => {
                return Err(be(format!(
                    "{} wants {other:?} samples; f32 and i16 are supported",
                    self.name
                )))
            }
        };
        stream.play().map_err(be)?;
        if let Some(c) = &cue_stream {
            c.play().map_err(be)?;
        }
        Ok(Running {
            health,
            _stream: stream,
            _cue: cue_stream,
        })
    }

    fn build<T>(
        &self,
        mut renderer: Source,
        routing: Routing,
        mut ring: Option<rtrb::Producer<f32>>,
        health: Arc<StreamHealth>,
    ) -> Result<Stream, HostError>
    where
        T: SizedSample + FromSample<f32> + Send + 'static,
    {
        let channels = self.channels();
        let mut inter = Interleaver::new();
        let mut scratch: Vec<f32> = vec![0.0; 16_384 * channels];
        let beat = health.clone();
        let rate = self.config.sample_rate as f64;
        let mut last: Option<std::time::Instant> = None;
        self.device
            .build_output_stream(
                self.config,
                move |data: &mut [T], _| {
                    beat.beat();
                    let now = std::time::Instant::now();
                    let gap = last.map(|t| now.saturating_duration_since(t).as_secs_f64());
                    last = Some(now);
                    beat.callback(data.len() / channels.max(1), gap, rate);
                    let mut silent = false;
                    let mut at = 0;
                    while at < data.len() {
                        let n = (data.len() - at).min(scratch.len());
                        match &mut renderer {
                            Source::Owned(r) => {
                                inter.fill(r, &routing, channels, &mut scratch[..n], ring.as_mut())
                            }
                            Source::Shared(slot) => match slot.try_lock() {
                                Ok(mut g) if g.is_some() => inter.fill(
                                    g.as_mut().expect("checked"),
                                    &routing,
                                    channels,
                                    &mut scratch[..n],
                                    ring.as_mut(),
                                ),
                                _ => {
                                    silent = true;
                                    scratch[..n].fill(0.0)
                                }
                            },
                        }
                        for (o, s) in data[at..at + n].iter_mut().zip(&scratch[..n]) {
                            *o = T::from_sample(*s);
                        }
                        at += n;
                    }
                    if silent {
                        beat.silent_fill();
                    }
                },
                move |e| report(&health, "audio", &e),
                None,
            )
            .map_err(be)
    }

    fn build_cue(
        self,
        mut rx: rtrb::Consumer<f32>,
        health: Arc<StreamHealth>,
    ) -> Result<Stream, HostError> {
        let channels = self.channels();
        match self.format {
            cpal::SampleFormat::F32 => self
                .device
                .build_output_stream(
                    self.config,
                    move |data: &mut [f32], _| Interleaver::fill_from_ring(&mut rx, channels, data),
                    move |e| report(&health, "cue audio", &e),
                    None,
                )
                .map_err(be),
            other => Err(be(format!(
                "{} wants {other:?} samples; the cue device needs f32",
                self.name
            ))),
        }
    }
}

/// The streams, alive while this is.
pub struct Running {
    /// What the callbacks report: the heartbeat (the master stream's) and the first fatal
    /// error of either stream.
    pub health: Arc<StreamHealth>,
    _stream: Stream,
    _cue: Option<Stream>,
}

/// An audio input device (cpal: CoreAudio, ALSA / PipeWire), feeding an input strip.
pub struct Input {
    device: cpal::Device,
    pub name: String,
    config: StreamConfig,
    format: cpal::SampleFormat,
}

/// Names of the input devices the platform reports.
pub fn input_names() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|it| it.map(|d| name_of(&d)).collect())
        .unwrap_or_default()
}

impl Input {
    /// The input device whose name contains `name` (case-insensitive), at `sample_rate` (the
    /// output's: the strip does not resample). Fails if the device has no config at that rate.
    pub fn open(name: &str, sample_rate: u32) -> Result<Input, HostError> {
        let host = cpal::default_host();
        let want = name.to_lowercase();
        let device = host
            .input_devices()
            .map_err(be)?
            .find(|d| name_of(d).to_lowercase().contains(&want))
            .ok_or_else(|| be(format!("no input device named like {name:?}")))?;
        let def = device.default_input_config().map_err(be)?;
        let supported = device
            .supported_input_configs()
            .map_err(be)?
            .filter(|r| r.sample_format() == def.sample_format())
            .find_map(|r| r.try_with_sample_rate(sample_rate))
            .ok_or_else(|| {
                be(format!(
                    "{} cannot capture at {sample_rate} Hz (it runs at {}); open the output at a rate both share",
                    name_of(&device),
                    def.sample_rate()
                ))
            })?;
        Ok(Input {
            name: name_of(&device),
            config: supported.config(),
            format: supported.sample_format(),
            device,
        })
    }

    pub fn channels(&self) -> usize {
        self.config.channels as usize
    }

    /// Start capturing into `feed`, one mono sample per frame, the channel `channel` selects
    /// (0 L, 1 R, 2 L+R at −3 dB; read every callback, so it can change while running). The
    /// callback only selects and pushes; a full ring drops samples.
    pub fn start(
        self,
        feed: rtrb::Producer<f32>,
        channel: std::sync::Arc<std::sync::atomic::AtomicU8>,
    ) -> Result<Capture, HostError> {
        self.start_feed(Feed::Owned(feed), channel)
    }

    /// [`Input::start`] into a [`SharedFeed`]: stop this capture and start another on the same
    /// feed to move the strip to another device.
    pub fn start_shared(
        self,
        feed: SharedFeed,
        channel: std::sync::Arc<std::sync::atomic::AtomicU8>,
    ) -> Result<Capture, HostError> {
        self.start_feed(Feed::Shared(feed), channel)
    }

    fn start_feed(
        self,
        feed: Feed,
        channel: std::sync::Arc<std::sync::atomic::AtomicU8>,
    ) -> Result<Capture, HostError> {
        let health = Arc::new(StreamHealth::new());
        let stream = match self.format {
            cpal::SampleFormat::F32 => self.build::<f32>(feed, channel, health.clone())?,
            cpal::SampleFormat::I16 => self.build::<i16>(feed, channel, health.clone())?,
            other => {
                return Err(be(format!(
                    "{} delivers {other:?} samples; f32 and i16 are supported",
                    self.name
                )))
            }
        };
        stream.play().map_err(be)?;
        Ok(Capture {
            name: self.name,
            health,
            _stream: stream,
        })
    }

    fn build<T>(
        &self,
        mut feed: Feed,
        channel: std::sync::Arc<std::sync::atomic::AtomicU8>,
        health: Arc<StreamHealth>,
    ) -> Result<Stream, HostError>
    where
        T: SizedSample + Send + 'static,
        f32: FromSample<T>,
    {
        let chans = self.channels().max(1);
        let beat = health.clone();
        self.device
            .build_input_stream(
                self.config,
                move |data: &[T], _| {
                    beat.beat();
                    let sel = djhost_channel(channel.load(std::sync::atomic::Ordering::Relaxed));
                    let mut guard;
                    let feed: &mut rtrb::Producer<f32> = match &mut feed {
                        Feed::Owned(f) => f,
                        Feed::Shared(slot) => {
                            guard = match slot.try_lock() {
                                Ok(g) => g,
                                Err(_) => return, // being moved: drop this buffer
                            };
                            match guard.as_mut() {
                                Some(f) => f,
                                None => return,
                            }
                        }
                    };
                    for frame in data.chunks_exact(chans) {
                        let l = <f32 as FromSample<T>>::from_sample_(frame[0]);
                        let r = if chans > 1 {
                            <f32 as FromSample<T>>::from_sample_(frame[1])
                        } else {
                            l
                        };
                        if feed.push(sel.select(l, r)).is_err() {
                            break; // the strip is not reading: drop the rest of this buffer
                        }
                    }
                },
                move |e| report(&health, "input", &e),
                None,
            )
            .map_err(be)
    }
}

/// Where a capture stream's samples go.
enum Feed {
    Owned(rtrb::Producer<f32>),
    Shared(SharedFeed),
}

fn djhost_channel(code: u8) -> crate::input::InputChannel {
    crate::host::deck::input::channel_of(code)
}

/// A running capture stream; dropping it stops the capture.
pub struct Capture {
    pub name: String,
    /// The heartbeat and the first fatal error.
    pub health: Arc<StreamHealth>,
    _stream: Stream,
}

/// Drop a stream off the calling thread: stopping one whose device just vanished can block in
/// the backend for a while (CoreAudio waiting on a Bluetooth link), and the caller is the UI.
pub fn retire<T: Send + 'static>(stream: T) {
    let _ = std::thread::Builder::new()
        .name("djhost-retire-stream".into())
        .spawn(move || drop(stream));
}
