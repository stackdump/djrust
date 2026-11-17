//! Loop pairs (`djrust::looppair`): in every genre, curated and generated, a group's A and B
//! share a frame (key, tempo, length, parts, rings), the groove in unison, and a lead that
//! answers without clashing. See the module doc for the rule.

use djrust::looppair::{self, complement, report, Band};
use djrust::loopsets::{cut, genres, played, LoopSet, Side, GROUPS};
use djrust::musicality::track_variety::transpose_track;

/// Every set under test: each genre's curated set, and a generated one (a key and a tempo of
/// its own).
fn sets() -> Vec<LoopSet> {
    let mut v = Vec::new();
    for g in genres() {
        v.push(LoopSet::curated(&g).unwrap_or_else(|| panic!("{g} has no curated set")));
        v.push(LoopSet::generate(&g, 4411).unwrap());
    }
    v
}

#[test]
fn a_pair_shares_the_frame_in_every_genre() {
    for s in sets() {
        for grp in &s.groups {
            let (a, b) = s.pair(grp.number, true).unwrap();
            let r = report(&a, &b);
            let at = format!("{} group {}", s.spec(), grp.number);
            assert!(
                r.frame_ok() && r.same_parts,
                "{at}: {}",
                looppair::summary(&r)
            );
            assert_eq!((a.tempo, b.tempo), (s.bpm, s.bpm), "{at}");
            assert_eq!(a.root_note, b.root_note, "{at}");
            assert_eq!(a.bars, b.bars, "{at}");
            // the loops the picker lists say the same key and tempo
            let infos = s.loops();
            let (ia, ib) = (
                &infos[(grp.number as usize - 1) * 2],
                &infos[(grp.number as usize - 1) * 2 + 1],
            );
            assert_eq!((ia.bpm, &ia.key), (ib.bpm, &ib.key), "{at}");
        }
    }
}

#[test]
fn a_pair_never_clashes_in_any_band() {
    for s in sets() {
        for grp in &s.groups {
            for drums in [true, false] {
                let r = s.pair_report(grp.number, drums).unwrap();
                let at = format!(
                    "{} group {} drums={drums}: {}",
                    s.spec(),
                    grp.number,
                    looppair::summary(&r)
                );
                for band in [Band::Low, Band::Mid, Band::High] {
                    assert_eq!(r.band(band).clash, 0, "{} band clashes, {at}", band.name());
                }
                assert_eq!(r.harmony_ratio(), 1.0, "{at}");
            }
        }
    }
}

#[test]
fn the_groove_is_in_unison_and_the_lead_answers() {
    for s in sets() {
        for grp in &s.groups {
            let r = s.pair_report(grp.number, true).unwrap();
            let at = format!(
                "{} group {}: {}",
                s.spec(),
                grp.number,
                looppair::summary(&r)
            );
            // the bass and the chords are the same notes in both: they stack, never mask
            for part in ["bass", "harmony"] {
                let p = &r.parts[part];
                assert!(
                    p.b > 0 && p.unison == p.b && p.fill + p.octave + p.clash == 0,
                    "{part}: {at}"
                );
            }
            // B is not A again: the lead says something else, in A's gaps or an octave up
            assert!(r.lead_answers(), "{at}");
            // the low band: B's bass is A's, B's kicks are A's or in A's kick gaps (a part that sits low, as in a transposed set, may be an octave): no note of
            // B's starts against one of A's
            let low = r.band(Band::Low);
            assert_eq!(low.unison + low.fill + low.octave, low.b, "{at}");
        }
    }
}

#[test]
fn a_loop_takes_the_leads_gaps_or_an_octave_but_never_a_rival_note() {
    // melody notes B keeps are in A's gaps, on A's notes, or an octave from them
    let s = LoopSet::curated("techno").unwrap();
    let r = s.pair_report(1, true).unwrap();
    let m = &r.parts["melody"];
    assert_eq!(m.clash, 0);
    assert!(
        m.unison + m.fill + m.octave == m.b && m.b > 0,
        "{}",
        looppair::summary(&r)
    );
}

#[test]
fn a_is_as_it_was_and_b_is_deterministic() {
    for g in ["techno", "house", "dnb"] {
        let s = LoopSet::curated(g).unwrap();
        for grp in &s.groups {
            // A is the cut as before: same model CID
            let mut was = cut(&played(g, grp.seed), Side::A, true);
            transpose_track(&mut was, s.transpose);
            was.tempo = s.bpm;
            was.name = format!("{} {}A {}", s.name(), grp.number, grp.name);
            assert_eq!(
                s.project(&format!("{}A", grp.number), true)
                    .unwrap()
                    .model_cid(),
                was.model_cid(),
                "{g} {}A changed",
                grp.number
            );
            // B twice is B
            let id = format!("{}B", grp.number);
            assert_eq!(
                s.project(&id, true).unwrap().model_cid(),
                LoopSet::curated(g)
                    .unwrap()
                    .project(&id, true)
                    .unwrap()
                    .model_cid()
            );
        }
    }
}

#[test]
fn pairing_is_idempotent() {
    let s = LoopSet::curated("edm").unwrap();
    for grp in &s.groups {
        let p = played("edm", grp.seed);
        let a = cut(&p, Side::A, true);
        let once = complement(&a, cut(&p, Side::B, true));
        let twice = complement(&a, once.clone());
        assert_eq!(once.model_cid(), twice.model_cid(), "group {}", grp.number);
    }
}

#[test]
fn the_cut_alone_did_not_pair() {
    // what the pairing fixes: B straight from the track clashes with A in most genres, and
    // in some its lead is A's notes again
    let (mut clashing, mut same_lead, mut total) = (0usize, 0usize, 0usize);
    for g in genres() {
        for seed in LoopSet::curated(&g).unwrap().groups.iter().map(|g| g.seed) {
            let p = played(&g, seed);
            let r = report(&cut(&p, Side::A, true), &cut(&p, Side::B, true));
            total += 1;
            clashing +=
                (r.band(Band::Low).clash + r.band(Band::Mid).clash + r.band(Band::High).clash > 0)
                    as usize;
            same_lead += !r.lead_answers() as usize;
        }
    }
    assert_eq!(total, genres().len() * GROUPS);
    assert!(clashing * 2 > total, "{clashing} of {total} clash");
    assert!(same_lead > 0);
}

#[test]
fn the_document_names_the_pairing() {
    let s = LoopSet::curated("lofi").unwrap();
    assert_eq!(
        s.to_json().get("pairing").and_then(|v| v.as_i64()),
        Some(looppair::VERSION as i64)
    );
}
