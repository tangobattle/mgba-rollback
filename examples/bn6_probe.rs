//! Headless BN6 wireless probe (local debugging tool, not for commit).
//!
//! Reads a script of `TICKS KEYS0 KEYS1` lines (keys as decimal mgba
//! bitmasks: A=1 B=2 SEL=4 START=8 R=16 L=32 U=64 D=128 Rb=256 Lb=512),
//! runs a two-player wireless link, dumps each core's frame as a PPM at
//! every script step boundary, and prints adapter/CPU telemetry so a
//! freeze names the spot where both sides are stuck.

use mgba_rollback::{Link, LinkOptions, Peripheral, SideOptions};

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

/// Tick marker for correlating the C-side WL_TRACE stderr stream with
/// the probe's stdout telemetry: each tick prints as `WT tick N` between
/// the wireless trace lines.
static TICK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// One "pc=[..] siocnt=[..] aflags=[..]" telemetry line for all sides.
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

fn main() {
    mgba::log::install_default_logger();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 || args.len() % 2 == 0 {
        panic!("usage: bn6_probe <script> <outdir> <rom> <save|-> [<rom> <save|-> ...]");
    }
    let (script_path, outdir) = (&args[1], &args[2]);
    std::fs::create_dir_all(outdir).unwrap();

    let sides: Vec<SideOptions> = args[3..]
        .chunks(2)
        .map(|pair| SideOptions {
            rom: std::fs::read(&pair[0]).unwrap(),
            save: (pair[1] != "-").then(|| std::fs::read(&pair[1]).unwrap()),
        })
        .collect();
    let n_players = sides.len();
    let mut link = Link::with_options(LinkOptions {
        sides,
        rtc: Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_752_000_000)),
        peripheral: Peripheral::Wireless,
    })
    .unwrap();

    let script = std::fs::read_to_string(script_path).unwrap();
    let mut tick_no = 0u32;
    for (ln, line) in script.lines().enumerate() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.split_whitespace();
        let n: u32 = it.next().unwrap().parse().unwrap();
        // One key column per player; missing columns idle. `=` repeats
        // the previous column (handy when every side walks in step).
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
                for i in 0..n_players {
                    dump_frame(&mut link, i, outdir, &format!("err-{tick_no}"));
                }
                return;
            }
            tick_no += 1;
            TICK.store(tick_no, std::sync::atomic::Ordering::Relaxed);
            if std::env::var_os("WL_TRACE").is_some() {
                eprintln!("WT tick {tick_no}");
            }
            if tick_no % 300 == 0 {
                let t = telemetry(&mut link);
                println!("t{tick_no}: {t}");
            }
        }
        let step = format!("s{:02}-t{tick_no}", ln);
        for i in 0..n_players {
            dump_frame(&mut link, i, outdir, &step);
        }
        println!("-- step {ln} done at tick {tick_no} (keys {keys:?})");
    }
    // Post-script freeze watch: run on, sampling for stuck screens.
    println!("script done; watching for 3600 ticks");
    let keys = vec![0u32; n_players];
    let mut static_screens = 0u32;
    let mut prev_crc = 0u32;
    for _ in 0..3600 {
        if let Err(e) = link.try_tick(&keys) {
            println!("!! watch: link error: {e}");
            break;
        }
        tick_no += 1;
        TICK.store(tick_no, std::sync::atomic::Ordering::Relaxed);
        if std::env::var_os("WL_TRACE").is_some() {
            eprintln!("WT tick {tick_no}");
        }
        if tick_no % 60 == 0 {
            let crc = (0..n_players)
                .map(|i| link.video_buffer(i).map(crc32fast::hash).unwrap_or(0))
                .fold(0, |a, b| a ^ b);
            if crc == prev_crc {
                static_screens += 1;
            } else {
                static_screens = 0;
            }
            prev_crc = crc;
            if static_screens >= 5 {
                let t = telemetry(&mut link);
                println!("!! screens static for {static_screens}s: {t}");
            }
        }
    }
    for i in 0..n_players {
        dump_frame(&mut link, i, outdir, "final");
    }
    println!("done at tick {tick_no}");
}
