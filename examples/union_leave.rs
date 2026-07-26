//! Headless union-room departure probe (local debugging tool, not for
//! commit).
//!
//! Phase 1: run a bn6_probe-style key script to walk three Emerald
//! machines into the union room together. Phase 2: capture every side —
//! exactly the boot-state + adapter-state pair a gbaroll merge/handoff
//! carries — then rebuild two links the way a departure does:
//!
//! - the LEAVER alone (gbaroll's unplug-continue solo resume);
//! - the two SURVIVORS (the lobby's re-merge).
//!
//! Both continuations then run unattended, dumping frames, so the two
//! sides' in-game views of the departure can be compared.
//!
//! usage: union_leave <script> <outdir> <rom> <sav0> <sav1> <sav2> <leaver>

use mgba_rollback::{BootSide, Link, LinkOptions, Peripheral, SideOptions};

fn gba_ptr(link: &mut Link, i: usize) -> *mut mgba_sys::GBA {
    link.core_mut(i).gba_mut().as_raw()
}

fn pc(link: &mut Link, i: usize) -> u32 {
    unsafe { (*(*gba_ptr(link, i)).cpu).__bindgen_anon_1.regs.__bindgen_anon_1.gprs[15] as u32 }
}

fn siocnt(link: &mut Link, i: usize) -> u16 {
    unsafe { (*gba_ptr(link, i)).sio.siocnt }
}

fn dump_frame(link: &mut Link, i: usize, dir: &str, tag: &str) {
    let Some(buf) = link.video_buffer(i) else { return };
    let mut out = Vec::with_capacity(240 * 160 * 3 + 20);
    out.extend_from_slice(b"P6\n240 160\n255\n");
    for px in buf.chunks_exact(2) {
        let v = u16::from_le_bytes([px[0], px[1]]);
        out.push(((v & 0x1F) << 3) as u8);
        out.push((((v >> 5) & 0x1F) << 3) as u8);
        out.push((((v >> 10) & 0x1F) << 3) as u8);
    }
    std::fs::write(format!("{dir}/{tag}-p{i}.ppm"), out).unwrap();
}

fn blob_field(link: &mut Link, i: usize, off: usize) -> u32 {
    let snap = link.save().unwrap();
    let b = snap.driver_blob(i);
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn telemetry(link: &mut Link) -> String {
    let n = link.num_players();
    let (mut pcs, mut cnts, mut afs) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..n {
        pcs.push(format!("{:08X}", pc(link, i)));
        cnts.push(format!("{:04X}", siocnt(link, i)));
        afs.push(format!("{:05X}", blob_field(link, i, 0xC0)));
    }
    format!(
        "pc=[{}] siocnt=[{}] aflags=[{}]",
        pcs.join(","),
        cnts.join(","),
        afs.join(",")
    )
}

fn capture(link: &mut Link, i: usize, rom: &[u8]) -> BootSide {
    BootSide {
        rom: rom.to_vec(),
        save: link.export_save(i),
        state: link.capture_boot_state(i).unwrap(),
        adapter: link.capture_adapter_state(i),
    }
}

/// Run a continuation link unattended, dumping frames periodically.
fn run_continuation(link: &mut Link, outdir: &str, tag: &str, ticks: u32) {
    let n = link.num_players();
    let keys = vec![0u32; n];
    for t in 0..ticks {
        if let Err(e) = link.try_tick(&keys) {
            println!("!! {tag}: link error at tick {t}: {e}");
            for i in 0..n {
                dump_frame(link, i, outdir, &format!("{tag}-err{t}"));
            }
            return;
        }
        if t % 600 == 599 {
            let tel = telemetry(link);
            println!("{tag} t{}: {tel}", t + 1);
            for i in 0..n {
                dump_frame(link, i, outdir, &format!("{tag}-t{:05}", t + 1));
            }
        }
    }
}

fn main() {
    mgba::log::install_default_logger();
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 8 {
        panic!("usage: union_leave <script> <outdir> <rom> <sav0> <sav1> <sav2> <leaver>");
    }
    let (script_path, outdir) = (&args[1], &args[2]);
    std::fs::create_dir_all(outdir).unwrap();
    let rom = std::fs::read(&args[3]).unwrap();
    let leaver: usize = args[7].parse().unwrap();

    let sides: Vec<SideOptions> = args[4..7]
        .iter()
        .map(|sav| SideOptions {
            rom: rom.clone(),
            save: Some(std::fs::read(sav).unwrap()),
        })
        .collect();
    let n_players = sides.len();
    let rtc = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_752_000_000);
    let mut link = Link::with_options(LinkOptions {
        sides,
        rtc: Some(rtc),
        peripheral: Peripheral::Wireless,
    })
    .unwrap();

    // Phase 1: the scripted walk into the union room.
    let script = std::fs::read_to_string(script_path).unwrap();
    let mut tick_no = 0u32;
    for (ln, line) in script.lines().enumerate() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.split_whitespace();
        let n: u32 = it.next().unwrap().parse().unwrap();
        let mut keys = vec![0u32; n_players];
        let mut prev = 0u32;
        for k in keys.iter_mut() {
            match it.next() {
                Some("=") => *k = prev,
                Some(v) => *k = v.parse().unwrap(),
                None => *k = 0,
            }
            prev = *k;
        }
        for _ in 0..n {
            if let Err(e) = link.try_tick(&keys) {
                println!("!! tick {tick_no}: link error: {e}");
                return;
            }
            tick_no += 1;
            if tick_no % 1200 == 0 {
                let t = telemetry(&mut link);
                println!("t{tick_no}: {t}");
            }
        }
        if ln % 8 == 0 || ln >= 40 {
            let step = format!("s{:02}-t{tick_no}", ln);
            for i in 0..n_players {
                dump_frame(&mut link, i, outdir, &step);
            }
        }
    }

    // Settle, then record what everyone sees before the departure.
    let keys = vec![0u32; n_players];
    for _ in 0..600 {
        link.try_tick(&keys).unwrap();
        tick_no += 1;
    }
    println!("script + settle done at tick {tick_no}: {}", telemetry(&mut link));
    for i in 0..n_players {
        dump_frame(&mut link, i, outdir, "pre");
    }

    // Phase 2: capture every side, exactly as a merge/handoff would.
    let captures: Vec<BootSide> = (0..n_players)
        .map(|i| capture(&mut link, i, &rom))
        .collect();
    drop(link);

    // The leaver's solo continuation (gbaroll's unplug-continue).
    let mut solo_side = None;
    let mut survivor_sides = Vec::new();
    for (i, side) in captures.into_iter().enumerate() {
        if i == leaver {
            solo_side = Some(side);
        } else {
            survivor_sides.push(side);
        }
    }

    println!("== leaver p{leaver} continues solo ==");
    let mut solo = Link::from_states(vec![solo_side.unwrap()], Some(rtc), Peripheral::Wireless)
        .unwrap();
    println!("solo seat: {}", solo.player_id(0));
    run_continuation(&mut solo, outdir, "solo", 7200);
    dump_frame(&mut solo, 0, outdir, "solo-final");
    drop(solo);

    println!("== survivors re-merge ==");
    let mut surv = Link::from_states(survivor_sides, Some(rtc), Peripheral::Wireless).unwrap();
    println!(
        "survivor seats: {}",
        (0..surv.num_players())
            .map(|i| surv.player_id(i).to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    run_continuation(&mut surv, outdir, "surv", 7200);
    for i in 0..surv.num_players() {
        dump_frame(&mut surv, i, outdir, "surv-final");
    }
    println!("done");
}
