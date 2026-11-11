//! The vocal pads (docs/MPK.md, "Vocals"): a folder of files, 16 to a page, played one-shot on
//! the beat with a choke group, stretched when the tempo is known; and the "vocals" preset that
//! puts them on the MPK's pads, added to a saved set that never had it.

use std::path::PathBuf;

use djrust::control::controller::Action;
use djrust::control::map::MapSet;
use djrust::control::profiles;
use djrust::control::surface::DJActionKind;
use djrust::host::controllers::ControllerSession;
use djrust::host::deck::loop_pads::Quant;
use djrust::host::deck::vox::{scan_dir, PAGE};
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};
use djrust::host::stores::maps::MapStore;
use djrust::host::stores::temp_path;

const SR: f64 = 48_000.0;
const PORT: &str = "MPK Mini Mk II";

fn rig() -> (Engine, Renderer) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, r) = engine(c);
    e.set_crossfader(0.0);
    e.set_loop_volume(1.0);
    (e, r)
}

/// A folder of `n` stub files (the scan reads names, never the audio), plus things it must skip.
fn folder(names: &[&str]) -> PathBuf {
    let d = temp_path("vox-pads");
    std::fs::create_dir_all(d.join("sub/deeper")).unwrap();
    for n in names {
        let p = d.join(n);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"RIFF").unwrap();
    }
    std::fs::write(d.join("notes.txt"), "x").unwrap();
    std::fs::write(d.join(".hidden.wav"), "x").unwrap();
    d
}

fn many(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("phrase_{i:03}.wav")).collect()
}

#[test]
fn the_folder_is_read_sorted_with_what_the_names_say() {
    let d = folder(&[
        "b_Hook_128bpm_Am.wav",
        "A_first.WAV",
        "sub/c_ad lib.flac",
        "sub/deeper/d_Fmaj_90 BPM.aif",
    ]);
    let f = scan_dir(&d);
    let names: Vec<&str> = f.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        ["A_first", "b_Hook_128bpm_Am", "sub/c_ad lib", "sub/deeper/d_Fmaj_90 BPM"],
        "sorted without case; notes.txt and .hidden.wav left out; the extension dropped"
    );
    assert_eq!(f[1].bpm, Some(128.0));
    assert_eq!(f[1].key, Some((9, true)));
    assert_eq!(f[3].bpm, Some(90.0));
    assert_eq!(f[3].key, Some((5, false)));
    assert_eq!((f[0].bpm, f[0].key), (None, None));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn pages_are_sixteen_files_and_wrap() {
    let names = many(40);
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let d = folder(&refs);
    let (mut e, _r) = rig();
    assert_eq!(e.set_vox_dir(Some(d.clone())), 40);
    assert_eq!(e.vox_pages(), 3);
    // bank A is the first eight of the page, bank B the next eight
    assert_eq!(e.vox_file(0).unwrap().name, "phrase_000");
    assert_eq!(e.vox_file(7).unwrap().name, "phrase_007");
    assert_eq!(e.vox_file(8).unwrap().name, "phrase_008");
    assert_eq!(e.vox_file(15).unwrap().name, "phrase_015");
    assert_eq!(e.vox_page_step("next"), 1);
    assert_eq!(e.vox_file(0).unwrap().name, "phrase_016");
    assert_eq!(e.vox_page_step("next"), 2);
    assert_eq!(e.vox_file(7).unwrap().name, "phrase_039");
    assert!(e.vox_file(8).is_none(), "the last page is short");
    assert_eq!(e.vox_page_step("next"), 0, "wraps");
    assert_eq!(e.vox_page_step("prev"), 2);
    assert_eq!(e.vox_page_step("first"), 0);
    let j = e.vox_json();
    assert_eq!(j.get("files").and_then(|v| v.as_f64()), Some(40.0));
    assert_eq!(j.get("pages").and_then(|v| v.as_f64()), Some(3.0));
    assert_eq!(j.get("pads").and_then(|v| v.as_array()).map(|a| a.len()), Some(PAGE));
    // files added: a rescan keeps the page
    std::fs::write(d.join("zzz.wav"), b"RIFF").unwrap();
    e.vox_page_step("next");
    assert_eq!(e.vox_scan(), 41);
    assert_eq!(e.vox.page, 1);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_pad_gets_the_folders_defaults_and_its_own_arguments_win() {
    let d = folder(&["a_plain.wav", "b_Hook_128bpm.wav"]);
    let (mut e, _r) = rig();
    e.set_vox_dir(Some(d.clone()));
    // defaults: one-shot, the next beat, the "vox" choke group, no tempo for a plain name
    let l = e.vox_launch(0, None, None, None, 1.0).unwrap();
    assert!(l.once && l.launch);
    assert_eq!((l.quant, l.choke.as_deref(), l.bpm), (Quant::Beat, Some("vox"), None));
    assert!(l.path.ends_with("a_plain.wav"));
    // a tempo in the name stretches it
    assert_eq!(e.vox_launch(1, None, None, None, 1.0).unwrap().bpm, Some(128.0));
    // the pad's own bpm / quant / choke / gain win over the name and the folder
    let l = e.vox_launch(1, Some("bar"), Some("lead"), Some(100.0), 0.5).unwrap();
    assert_eq!((l.quant, l.choke.as_deref(), l.bpm, l.gain), (Quant::Bar, Some("lead"), Some(100.0), 0.5));
    // a folder at one tempo
    e.vox.bpm = Some(124.0);
    assert_eq!(e.vox_launch(0, None, None, None, 1.0).unwrap().bpm, Some(124.0));
    assert_eq!(e.vox_launch(1, None, None, None, 1.0).unwrap().bpm, Some(128.0), "the name first");
    // stretch off: every file at its own speed
    e.vox.stretch = false;
    assert_eq!(e.vox_launch(1, None, None, None, 1.0).unwrap().bpm, None);
    // an empty choke is no choke
    e.vox.choke = String::new();
    assert_eq!(e.vox_launch(0, None, None, None, 1.0).unwrap().choke, None);
    // a pad with no file says why
    let err = e.vox_launch(9, None, None, None, 1.0).unwrap_err();
    assert!(err.contains("vox pad 10 has no file") && err.contains("2 files"), "{err}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn with_no_folder_the_pad_says_which_setting_to_make() {
    let (mut e, _r) = rig();
    let err = e.vox_launch(0, None, None, None, 1.0).unwrap_err();
    assert!(err.contains("set vox.dir"), "{err}");
}

fn target(sample: &str) -> Action {
    Action::Target {
        id: "oneShot".into(),
        deck: None,
        slot: None,
        index: 0,
        value: 1.0,
        pressed: true,
        args: [("sample".to_string(), sample.to_string())].into(),
    }
}

#[test]
fn a_press_with_no_file_is_counted_not_lost() {
    use djrust::control::controller::SurfaceHost;
    let (mut e, _r) = rig();
    SurfaceHost::perform(&mut e, target("vox:4"));
    assert!(e.unhandled.iter().any(|u| u.contains("vox pad 5 has no file")), "{:?}", e.unhandled);
    SurfaceHost::perform(&mut e, target("vox:banana"));
    assert!(e.unhandled.iter().any(|u| u.contains("vox:N takes a pad number")), "{:?}", e.unhandled);
    SurfaceHost::perform(&mut e, target("vox:99"));
    assert!(e.unhandled.iter().any(|u| u.contains("vox pad 100 has no file")));
}

#[test]
fn the_vocals_preset_puts_sixteen_vocals_on_the_pads() {
    let mut s = ControllerSession::new();
    assert!(s.attach(PORT));
    let mut set = MapSet::parse(profiles::MPK_PERFORMANCE_MAP_JSON).unwrap();
    assert!(set.select("vocals"));
    s.router.apply_map(&set.device, Some(set.current()));
    let one_shot = |a: &[Action]| -> Option<String> {
        a.iter().find_map(|a| match a {
            Action::Target { id, args, .. } if id == "oneShot" => args.get("sample").cloned(),
            _ => None,
        })
    };
    // NOTE mode: bank A pad 1 is note 36, bank B pad 8 is note 51
    let a = s.route(PORT, &[0x99, 36, 100], 0.0).unwrap().actions;
    assert_eq!(one_shot(&a).as_deref(), Some("vox:0"));
    let a = s.route(PORT, &[0x99, 44, 100], 0.0).unwrap().actions;
    assert_eq!(one_shot(&a).as_deref(), Some("vox:8"));
    let a = s.route(PORT, &[0x99, 51, 100], 0.0).unwrap().actions;
    assert_eq!(one_shot(&a).as_deref(), Some("vox:15"));
    // PROG mode: the eight program changes play bank A's eight, so the preset works there too
    let a = s.route(PORT, &[0xC0, 3, 0], 0.0).unwrap().actions;
    assert_eq!(one_shot(&a).as_deref(), Some("vox:3"));
    // the options ride on the binding
    let b = set.current().get("padNotes", Some(0), None).unwrap();
    assert_eq!(b.action, DJActionKind::OneShot);
    assert_eq!(b.args["quant"], "beat");
    assert_eq!(b.args["choke"], "vox");
}

#[test]
fn a_saved_set_gets_the_vocals_preset_once_and_the_keys_binding_follows() {
    let dir = temp_path("vox-maps");
    let store = MapStore::new(&dir);
    // a set saved before the vocals preset: the three then shipped, with the old keys binding
    let shipped = MapSet::parse(profiles::MPK_PERFORMANCE_MAP_JSON).unwrap();
    let mut old = shipped.clone();
    old.presets.retain(|p| p.name != "vocals");
    old.offered.clear();
    let keys = shipped.presets[0].get("keys", None, None).unwrap().clone();
    assert_eq!(keys.action, DJActionKind::Keys);
    let mut transpose = keys.clone();
    transpose.action = DJActionKind::Transpose;
    transpose.deck = Some("L".into());
    transpose.args = [("quant".into(), "bar".into()), ("mode".into(), "keep-tempo".into())].into();
    old.presets[0].set(transpose);
    store.save(&old).unwrap();

    let set = store.load_or_default(PORT);
    let names: Vec<&str> = set.presets.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["performance", "vocoder", "vocals", "decks"], "before the empty one");
    assert_eq!(set.presets[0].get("keys", None, None).unwrap().action, DJActionKind::Keys);
    assert!(set.offered.contains(&"vocals".to_string()));

    // saved again, a preset the user deleted stays deleted
    let mut mine = set.clone();
    mine.presets.retain(|p| p.name != "vocals");
    store.save(&mine).unwrap();
    let again = store.load_or_default(PORT);
    assert!(!again.presets.iter().any(|p| p.name == "vocals"), "offered once, not forced");

    // a keys binding the user changed is theirs
    let mut changed = old.clone();
    let mut b = changed.presets[0].get("keys", None, None).unwrap().clone();
    b.args.insert("mode".into(), "tape".into());
    changed.presets[0].set(b);
    store.save(&changed).unwrap();
    let kept = store.load_or_default(PORT);
    assert_eq!(kept.presets[0].get("keys", None, None).unwrap().action, DJActionKind::Transpose);

    // offered round-trips and an old file without it parses
    assert_eq!(MapSet::parse(&set.to_json().js_stringified()).unwrap(), set);
    let legacy = MapSet::parse(&old.to_json().js_stringified()).unwrap();
    assert!(legacy.offered.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "decode")]
mod playing {
    use super::*;
    use djrust::control::controller::SurfaceHost;
    use djrust::looplib::wav_encode;
    use djrust::host::deck::loops::LoopState;

    fn wav(dir: &std::path::Path, name: &str, secs: f64) {
        let n = (secs * SR) as usize;
        let s: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * 300.0 * i as f64 / SR).sin() as f32 * 0.4)
            .collect();
        std::fs::write(dir.join(name), wav_encode(SR as u32, &s, Some(&s))).unwrap();
    }

    fn run(e: &mut Engine, r: &mut Renderer, blocks: usize) {
        let (mut l, mut rr) = (vec![0f32; 256], vec![0f32; 256]);
        for _ in 0..blocks {
            r.render_block(&mut l, &mut rr);
            e.service_loops();
            std::thread::sleep(std::time::Duration::from_micros(300));
        }
    }

    fn sounding(e: &Engine, name: &str) -> Option<bool> {
        e.loops
            .row
            .clips
            .iter()
            .find(|c| e.clip_extra(&c.id).from.ends_with(name))
            .map(|c| e.loop_state(&c.id) != LoopState::Off)
    }

    #[test]
    fn a_phrase_cuts_the_one_before_and_paging_clears_the_row() {
        let d = temp_path("vox-play");
        std::fs::create_dir_all(&d).unwrap();
        // 17 files: the 17th is on page 2
        for i in 0..17 {
            wav(&d, &format!("p_{i:02}.wav"), if i < 2 { 3.0 } else { 0.2 });
        }
        let (mut e, mut r) = rig();
        e.set_vox_dir(Some(d.clone()));
        // the quantise is now so the test does not wait for a beat
        e.vox.quant = Quant::Now;
        SurfaceHost::perform(&mut e, target("vox:0"));
        run(&mut e, &mut r, 60);
        assert_eq!(sounding(&e, "p_00.wav"), Some(true));
        SurfaceHost::perform(&mut e, target("vox:1"));
        run(&mut e, &mut r, 60);
        assert_eq!(sounding(&e, "p_01.wav"), Some(true));
        assert_eq!(sounding(&e, "p_00.wav"), Some(false), "one voice: the second phrase cut the first");
        // the clip is a one-shot with the vocal choke group
        let c = e.loops.row.clips.iter().find(|c| e.clip_extra(&c.id).from.ends_with("p_01.wav")).unwrap();
        let x = e.clip_extra(&c.id);
        assert!(x.once);
        assert_eq!(x.choke.as_deref(), Some("vox"));
        assert_eq!(x.root, 0, "a phrase is not a loop to transpose");
        // page 2: the idle vocals of page 1 come off the row (the sounding one is kept)
        run(&mut e, &mut r, 1200);
        let before = e.loops.row.clips.len();
        assert!(before >= 2);
        SurfaceHost::perform(
            &mut e,
            Action::Target {
                id: "voxPage".into(),
                deck: None,
                slot: None,
                index: 0,
                value: 1.0,
                pressed: true,
                args: [("op".to_string(), "next".to_string())].into(),
            },
        );
        assert_eq!(e.vox.page, 1);
        assert!(e.loops.row.clips.len() < before, "{} then {}", before, e.loops.row.clips.len());
        SurfaceHost::perform(&mut e, target("vox:0"));
        run(&mut e, &mut r, 60);
        assert!(e.loops.row.clips.iter().any(|c| e.clip_extra(&c.id).from.ends_with("p_16.wav")));
        let _ = std::fs::remove_dir_all(&d);
    }
}
