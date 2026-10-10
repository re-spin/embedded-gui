//! Host render of the spike scenario: writes every Frame/Dirty mark as raw
//! X1RGB555 (frameNN.x1rgb555) and PNG-ready PPM into the given directory.
use std::{fs, path::PathBuf};

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "target/frames".into()));
    fs::create_dir_all(&dir).unwrap();
    let mut px = vec![0u16; (egui_bfin_spike::W * egui_bfin_spike::H) as usize];
    let mut n = 0;
    let mut slot = Box::new(std::mem::MaybeUninit::uninit());
    egui_bfin_spike::run(&mut px, &mut slot, &mut |m, f| {
        if matches!(m, egui_bfin_spike::Mark::Frame | egui_bfin_spike::Mark::Dirty) {
            let raw: Vec<u8> = f.iter().flat_map(|p| p.to_le_bytes()).collect();
            fs::write(dir.join(format!("frame{n:02}.x1rgb555")), &raw).unwrap();
            let mut ppm = format!("P6\n{} {}\n255\n", egui_bfin_spike::W, egui_bfin_spike::H).into_bytes();
            for p in f {
                ppm.extend([(p >> 10 & 31) as u8 * 8, (p >> 5 & 31) as u8 * 8, (p & 31) as u8 * 8]);
            }
            fs::write(dir.join(format!("frame{n:02}.ppm")), ppm).unwrap();
            n += 1;
        }
    })
    .unwrap();
}
