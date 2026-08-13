use ebur128::Mode; use std::time::Instant;
fn run(mode: Mode) -> (f64,f64,f64,f64) {
    let ch=2usize; let frames=1024usize;
    let mut m = ebur128::EbuR128::new(ch as u32, 48000, mode).unwrap();
    let mut s=12345u64;
    for blk in 0..1200 {
        // vary level every ~2s so LRA is non-zero
        let amp = 0.05 + 0.9*(((blk/90) % 5) as f64/4.0);
        let buf: Vec<f64> = (0..frames*ch).map(|_| { s=s.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((s>>11) as f64/(1u64<<53) as f64 - 0.5)*2.0*amp }).collect();
        m.add_frames_f64(&buf).unwrap();
    }
    (m.loudness_global().unwrap(), m.loudness_shortterm().unwrap(), m.loudness_momentary().unwrap(), m.loudness_range().unwrap())
}
fn perf(mode: Mode)->f64{
    let frames=4096usize; let ch=2;
    let buf:Vec<f64>=(0..frames*ch).map(|i|((i as f64)*0.013).sin()*0.5).collect();
    let mut m=ebur128::EbuR128::new(ch as u32,48000,mode).unwrap();
    for _ in 0..300 { m.add_frames_f64(&buf).unwrap(); }
    let it=8000; let t=Instant::now(); for _ in 0..it{m.add_frames_f64(&buf).unwrap();}
    t.elapsed().as_secs_f64()/it as f64*1e9/frames as f64
}
fn main(){
    let a=run(Mode::all());
    let b=run(Mode::I|Mode::LRA|Mode::HISTOGRAM);
    println!("all()           I={:.12} S={:.12} M={:.12} LRA={:.12}",a.0,a.1,a.2,a.3);
    println!("I|LRA|HISTOGRAM I={:.12} S={:.12} M={:.12} LRA={:.12}",b.0,b.1,b.2,b.3);
    println!("BIT-EQUAL (varying-level, LRA nonzero): {}", a==b);
    println!();
    println!("perf all()            : {:.2} ns/sample", perf(Mode::all()));
    println!("perf I|LRA|HISTOGRAM  : {:.2} ns/sample", perf(Mode::I|Mode::LRA|Mode::HISTOGRAM));
}
