//! Pick each genre's curated loop set (`djrust::loopsets::CURATED`): score track seeds
//! 1..=N with the musicality report (`loopsets::candidate_score`), and keep the best `GROUPS`
//! whose lead lines (and names) do not repeat one another's.
//!
//!     cargo run --release --example loopsets_curate [N=400] [--write]
//!
//! Prints the table; `--write` puts it between the `@curated-begin` / `@curated-end` markers of
//! `src/loopsets.rs`.

use std::collections::BTreeSet;

use djrust::loopsets::{candidate_score, cut, genres, played, Side, GROUPS};

/// What a track's A loop's bass and lead play (notes on steps), to tell two tracks apart.
fn lines(genre: &str, seed: u32) -> (Vec<(String, i64)>, Vec<(String, i64)>) {
    let p = cut(&played(genre, seed), Side::A, false);
    let of = |k: &str| {
        p.nets
            .get(k)
            .map(|n| {
                n.bindings
                    .iter()
                    .map(|(t, b)| (t.to_string(), b.note))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    (of("bass"), of("melody"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let n: u32 = args.iter().find_map(|a| a.parse().ok()).unwrap_or(400);
    let write = args.iter().any(|a| a == "--write");
    let mut rows = Vec::new();
    for g in genres() {
        let mut scored: Vec<(f64, u32)> = (1..=n).map(|s| (candidate_score(&g, s), s)).collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(a.1.cmp(&b.1)));
        let mut picked: Vec<u32> = Vec::new();
        let mut seen_bass = BTreeSet::new();
        let mut seen_lead = BTreeSet::new();
        let mut names = BTreeSet::new();
        for (_, s) in &scored {
            let (bass, lead) = lines(&g, *s);
            let name = djrust::gen::generate_track_name(&g, *s as f64);
            if (seen_bass.contains(&bass) && seen_lead.contains(&lead))
                || seen_lead.contains(&lead)
                || names.contains(&name)
            {
                continue;
            }
            seen_bass.insert(bass);
            seen_lead.insert(lead);
            names.insert(name);
            picked.push(*s);
            if picked.len() == GROUPS {
                break;
            }
        }
        eprintln!(
            "{g}: best {:.2}, picked {:?}",
            scored.first().map_or(0.0, |s| s.0),
            picked
        );
        if picked.len() == GROUPS {
            rows.push(format!(
                "    (\"{g}\", [{}]),",
                picked
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    let table = rows.join("\n");
    println!("{table}");
    if write {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/loopsets.rs");
        let src = std::fs::read_to_string(path).unwrap();
        let begin = src.find("// @curated-begin").unwrap();
        let begin = begin + src[begin..].find('\n').unwrap() + 1;
        let end = src[..].find("    // @curated-end").unwrap();
        let out = format!("{}{}\n{}", &src[..begin], table, &src[end..]);
        std::fs::write(path, out).unwrap();
        eprintln!("wrote {path}");
    }
}
