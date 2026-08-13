use std::time::Instant;
use rustfft::{FftPlanner, num_complex::Complex};
use realfft::RealFftPlanner;
fn main(){
 for n in [512usize,1024,2048] {
  let mut p=FftPlanner::<f64>::new();
  let f=p.plan_fft_forward(n); let i=p.plan_fft_inverse(n);
  let mut b=vec![Complex::new(0.0,0.0);n];
  let mut sc=vec![Complex::new(0.0,0.0); f.get_inplace_scratch_len().max(i.get_inplace_scratch_len())];
  let it=200000;
  let t=Instant::now();
  for _ in 0..it { f.process_with_scratch(&mut b,&mut sc); i.process_with_scratch(&mut b,&mut sc); }
  let cx=t.elapsed().as_secs_f64()/it as f64;

  let mut rp=RealFftPlanner::<f64>::new();
  let rf=rp.plan_fft_forward(n); let ri=rp.plan_fft_inverse(n);
  let mut tb=vec![0.0f64;n]; let mut sp=rf.make_output_vec();
  let mut s1=rf.make_scratch_vec(); let mut s2=ri.make_scratch_vec();
  let t=Instant::now();
  for _ in 0..it { rf.process_with_scratch(&mut tb,&mut sp,&mut s1).unwrap();
                   ri.process_with_scratch(&mut sp,&mut tb,&mut s2).unwrap(); }
  let re=t.elapsed().as_secs_f64()/it as f64;
  println!("N={n:<5} complex fwd+inv {:>7.2} us | real fwd+inv {:>7.2} us | realfft {:.2}x faster", cx*1e6, re*1e6, cx/re);
 }
}
