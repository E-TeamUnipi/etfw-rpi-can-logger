//! canring: inspect and export a CAN ring without the web interface.
//!
//! Works on the logger itself or on a laptop with the SD card plugged in
//! (or a `dd` image of the ring partition).
//!
//!   canring <device|image> info
//!   canring <device|image> list [--json]
//!   canring <device|image> export <session|last> [--format candump|asc] [-o FILE] [--from S] [--to S]
//!   canring <device|image> dump <session|last>      # non-frame records, for debugging
//!   canring <image> create <size MiB>                # make an empty ring file for testing

use canlog_core::export::{self, Format, Options};
use canlog_core::ring::{random_u32, Ring, RingDevice};
use canlog_core::timefmt;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::process::exit;

fn usage() -> ! {
    eprintln!(
        "usage:\n  canring <dev> info\n  canring <dev> list [--json]\n  canring <dev> export <id|last> [--format candump|asc] [-o FILE] [--from SEC] [--to SEC] [--iface NAME]\n  canring <dev> dump <id|last>\n  canring <file> create <MiB>"
    );
    exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        usage();
    }
    let path = Path::new(&args[0]);
    let cmd = args[1].as_str();
    if let Err(e) = run(path, cmd, &args[2..]) {
        eprintln!("canring: {e}");
        exit(1);
    }
}

fn run(path: &Path, cmd: &str, rest: &[String]) -> io::Result<()> {
    if cmd == "create" {
        let mib: u64 = rest.first().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage());
        let f = std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(path)?;
        f.set_len(mib << 20)?;
        drop(f);
        let mut d = RingDevice::open(path, true)?;
        d.format(random_u32(), canlog_core::realtime_ns() / 1_000_000)?;
        println!("created {} ({} data blocks)", path.display(), d.total_blocks - 1);
        return Ok(());
    }
    let dev = RingDevice::open(path, false)?;
    let Some(sb) = dev.sb.clone() else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "no ring superblock (not a ring, or never used)"));
    };
    let mut ring = Ring::new(dev);
    match cmd {
        "info" => {
            let head = ring.find_head();
            println!("ring id       {:08x}", sb.ring_id);
            println!("size          {} MiB ({} data blocks)", (sb.total_blocks * 4096) >> 20, sb.data_blocks());
            println!("created       {}", timefmt::iso(sb.created_utc_ms));
            match head {
                Some(h) => {
                    println!("head seq      {} (session {})", h.seq, h.session);
                    println!("wrapped       {}", h.seq >= sb.data_blocks());
                }
                None => println!("head          (empty)"),
            }
        }
        "list" => {
            let ss = ring.sessions();
            if rest.iter().any(|a| a == "--json") {
                println!("{}", serde_json::to_string_pretty(&ss).unwrap());
                return Ok(());
            }
            println!("{:>6}  {:<24} {:>10} {:>9}  name", "id", "start (UTC)", "duration", "size");
            for s in &ss {
                let start = s.start_utc_ms.map(timefmt::iso).unwrap_or_else(|| "(no time sync)".into());
                let flag = if s.complete { "" } else { " [start overwritten]" };
                println!("{:>6}  {:<24} {:>9.1}s {:>7}KiB  {}{}", s.id, start, s.duration_s, s.blocks * 4, s.name, flag);
            }
        }
        "export" | "dump" => {
            let which = rest.first().unwrap_or_else(|| usage());
            let ss = ring.sessions();
            let s = if which == "last" {
                ss.last().cloned()
            } else {
                let id: u32 = which.parse().unwrap_or_else(|_| usage());
                ss.iter().find(|s| s.id == id).cloned()
            }
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such session"))?;
            if cmd == "dump" {
                let stdout = io::stdout();
                return export::dump_records(&mut ring, &s, &mut stdout.lock());
            }
            let mut fmt = Format::Candump;
            let mut outp: Option<String> = None;
            let mut opt = Options::default();
            let mut i = 1;
            while i < rest.len() {
                let v = rest.get(i + 1).cloned().unwrap_or_default();
                match rest[i].as_str() {
                    "--format" | "-f" => fmt = Format::parse(&v).unwrap_or_else(|| usage()),
                    "-o" => outp = Some(v),
                    "--from" => opt.from_s = v.parse().ok(),
                    "--to" => opt.to_s = v.parse().ok(),
                    "--iface" => opt.ifaces.push(v),
                    _ => usage(),
                }
                i += 2;
            }
            let n = match outp.as_deref() {
                Some("auto") => {
                    let name = export::file_name(&s, fmt);
                    let n = export::export(&mut ring, &s, fmt, &opt, &mut BufWriter::new(std::fs::File::create(&name)?))?;
                    eprintln!("wrote {name}");
                    n
                }
                Some(p) => export::export(&mut ring, &s, fmt, &opt, &mut BufWriter::new(std::fs::File::create(p)?))?,
                None => {
                    let stdout = io::stdout();
                    let mut lock = stdout.lock();
                    let n = export::export(&mut ring, &s, fmt, &opt, &mut lock)?;
                    lock.flush()?;
                    n
                }
            };
            eprintln!("{n} frames");
        }
        _ => usage(),
    }
    Ok(())
}
