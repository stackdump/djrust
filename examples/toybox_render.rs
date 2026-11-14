//! Write the synthesized Toy Box bank, and optionally a WAV of every sample to audition it.
//!
//! ```text
//! cargo run --release --example toybox_render -- <out/toybox.sf2> [wav-folder]
//! ```
//!
//! Each WAV is the sample as the bank stores it (mono, written as stereo 16-bit at the bank's
//! rate); a looped sample is played through its loop for about two seconds, then faded.

use std::time::Instant;

use djrust::sound::toybox;
use djrust::wav::wav_bytes;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(out) = args.first() else {
        eprintln!("usage: toybox_render <out/toybox.sf2> [wav-folder]");
        std::process::exit(2);
    };
    let t = Instant::now();
    let bytes = toybox::build();
    let took = t.elapsed();
    std::fs::write(out, &bytes).expect("write the bank");
    println!(
        "{out}: {} bytes, {} programs + kit, version {}, built in {:.0} ms",
        bytes.len(),
        toybox::programs().len(),
        toybox::VERSION,
        took.as_secs_f64() * 1000.0
    );
    let Some(dir) = args.get(1) else { return };
    std::fs::create_dir_all(dir).expect("make the wav folder");
    let rate = toybox::SAMPLE_RATE as usize;
    for s in toybox::samples() {
        let mut pcm = s.pcm.clone();
        if let Some(l) = &s.loop_ {
            pcm.truncate(l.end);
            while pcm.len() < 2 * rate {
                pcm.extend_from_slice(&s.pcm[l.clone()]);
            }
            let fade = rate / 5;
            let n = pcm.len();
            for i in 0..fade {
                pcm[n - fade + i] *= 1.0 - (i + 1) as f32 / fade as f32;
            }
        }
        let name = format!("{dir}/{:03}-{}.wav", s.program, s.name.replace(' ', "-"));
        std::fs::write(&name, wav_bytes(&pcm, &pcm, rate as f64, false)).expect("write a wav");
        println!("{name}: {:.2} s", pcm.len() as f64 / rate as f64);
    }
}
