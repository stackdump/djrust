//! From the mixer's planar blocks to a device's interleaved buffer, with the headphone cue
//! routed the way `CueRouting.swift` does it (`ChannelRouterAU`: the master mix on one channel
//! pair of a multichannel interface, the cue on another, every other channel silent), or on a
//! second device through a ring.
//!
//! No device needed: this is what the cpal callback runs ([`crate::host::audio`]), and what the
//! tests run on plain slices.

use rtrb::{Consumer, Producer};

use super::engine::{Renderer, MAX_BLOCK};

/// Where the master and the headphone feed go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Routing {
    /// The master on the first two channels (mono: both summed to the one), no cue.
    Stereo,
    /// `ChannelRouterAU`: the master mix on channels `master_pair`, `master_pair + 1`, the cue
    /// on `cue_pair`, `cue_pair + 1` (default 0 and 2 on a 4-channel interface such as the
    /// Inpulse's built-in sound card); every other channel stays silent.
    Pairs { master_pair: usize, cue_pair: usize },
    /// The master on the first two channels of this stream; the cue is pushed to a ring that
    /// a second device's stream drains (`Interleaver::fill_from_ring`).
    CueDevice,
}

impl Routing {
    /// Does the routing carry a headphone feed?
    pub fn has_cue(&self) -> bool {
        !matches!(self, Routing::Stereo)
    }

    /// Does a device with `channels` outputs have room for it (`CueOutputError.notEnoughChannels`)?
    pub fn fits(&self, channels: usize) -> bool {
        match self {
            Routing::Stereo | Routing::CueDevice => channels >= 1,
            Routing::Pairs {
                master_pair,
                cue_pair,
            } => channels >= (*master_pair).max(*cue_pair) + 2,
        }
    }
}

/// Planar scratch for the callback (allocated once; the callback itself allocates nothing).
pub struct Interleaver {
    l: Vec<f32>,
    r: Vec<f32>,
    cl: Vec<f32>,
    cr: Vec<f32>,
}

impl Default for Interleaver {
    fn default() -> Self {
        Self::new()
    }
}

impl Interleaver {
    pub fn new() -> Interleaver {
        Interleaver {
            l: vec![0.0; MAX_BLOCK],
            r: vec![0.0; MAX_BLOCK],
            cl: vec![0.0; MAX_BLOCK],
            cr: vec![0.0; MAX_BLOCK],
        }
    }

    /// Render `out.len() / channels` frames and interleave them by `routing`. `cue_ring`
    /// receives the headphone feed (stereo interleaved) for [`Routing::CueDevice`]; a full
    /// ring drops the excess (the second device is late).
    pub fn fill(
        &mut self,
        renderer: &mut Renderer,
        routing: &Routing,
        channels: usize,
        out: &mut [f32],
        mut cue_ring: Option<&mut Producer<f32>>,
    ) {
        let channels = channels.max(1);
        let total = out.len() / channels;
        let mut at = 0;
        while at < total {
            let n = (total - at).min(MAX_BLOCK);
            let want_cue = routing.has_cue();
            let (l, r, cl, cr) = (
                &mut self.l[..n],
                &mut self.r[..n],
                &mut self.cl[..n],
                &mut self.cr[..n],
            );
            renderer.render_with_cue(l, r, want_cue.then_some((&mut *cl, &mut *cr)));
            let block = &mut out[at * channels..(at + n) * channels];
            for v in block.iter_mut() {
                *v = 0.0;
            }
            match routing {
                Routing::Stereo | Routing::CueDevice => {
                    for i in 0..n {
                        let f = &mut block[i * channels..(i + 1) * channels];
                        if channels == 1 {
                            f[0] = 0.5 * (l[i] + r[i]);
                        } else {
                            f[0] = l[i];
                            f[1] = r[i];
                        }
                    }
                }
                Routing::Pairs {
                    master_pair,
                    cue_pair,
                } => {
                    for i in 0..n {
                        let f = &mut block[i * channels..(i + 1) * channels];
                        if master_pair + 1 < channels {
                            f[*master_pair] = l[i];
                            f[*master_pair + 1] = r[i];
                        }
                        if cue_pair + 1 < channels {
                            f[*cue_pair] = cl[i];
                            f[*cue_pair + 1] = cr[i];
                        }
                    }
                }
            }
            if let (Routing::CueDevice, Some(ring)) = (routing, cue_ring.as_deref_mut()) {
                for i in 0..n {
                    if ring.push(cl[i]).is_err() || ring.push(cr[i]).is_err() {
                        break;
                    }
                }
            }
            at += n;
        }
    }

    /// The second device's callback: pop the cue frames the first one pushed (silence when it
    /// has not produced enough yet).
    pub fn fill_from_ring(ring: &mut Consumer<f32>, channels: usize, out: &mut [f32]) {
        let channels = channels.max(1);
        for f in out.chunks_mut(channels) {
            let (l, r) = (ring.pop().unwrap_or(0.0), ring.pop().unwrap_or(0.0));
            for v in f.iter_mut() {
                *v = 0.0;
            }
            if channels == 1 {
                f[0] = 0.5 * (l + r);
            } else {
                f[0] = l;
                f[1] = r;
            }
        }
    }
}
