use std::time::Instant;

// ---- Minimal in-house EBU R128 (BS.1770-4) prototype ----
#[derive(Clone, Copy, Default)]
struct Biquad { b0:f64,b1:f64,b2:f64,a1:f64,a2:f64 }
#[derive(Clone, Copy, Default)]
struct St { x1:f64,x2:f64,y1:f64,y2:f64 }
impl Biquad {
    #[inline] fn run(&self, s:&mut St, x:f64)->f64{
        let y = self.b0*x + self.b1*s.x1 + self.b2*s.x2 - self.a1*s.y1 - self.a2*s.y2;
        s.x2=s.x1; s.x1=x; s.y2=s.y1; s.y1=y; y
    }
}
// BS.1770 K-weighting coefficients (48 kHz reference, per spec tables)
fn kweight(rate:f64)->(Biquad,Biquad){
    // stage 1: high shelf
    let f0=1681.974450955533; let g=3.999843853973347; let q=0.7071752369554196;
    let k=(std::f64::consts::PI*f0/rate).tan();
    let vh=10f64.powf(g/20.0); let vb=vh.powf(0.4996667741545416);
    let a0=1.0+k/q+k*k;
    let hs=Biquad{
        b0:(vh+vb*k/q+k*k)/a0, b1:2.0*(k*k-vh)/a0, b2:(vh-vb*k/q+k*k)/a0,
        a1:2.0*(k*k-1.0)/a0, a2:(1.0-k/q+k*k)/a0 };
    // stage 2: high pass
    let f0=38.13547087602444; let q=0.5003270373238773;
    let k=(std::f64::consts::PI*f0/rate).tan();
    let hp=Biquad{ b0:1.0, b1:-2.0, b2:1.0,
        a1:2.0*(k*k-1.0)/(1.0+k/q+k*k), a2:(1.0-k/q+k*k)/(1.0+k/q+k*k) };
    (hs,hp)
}
struct Meter{
    hs:Biquad, hp:Biquad, st:Vec<(St,St)>, weights:Vec<f64>,
    ch:usize, block_frames:usize, pos:usize, acc:Vec<f64>,
    // 100ms sub-block energies, ring for 400ms(M)/3000ms(S) sliding sums
    sub:Vec<f64>, sub_head:usize, sub_count:usize,
    hist:Vec<u32>, // gating histogram over momentary blocks
}
const HN:usize=1000; const HMIN:f64=-70.0; const HSTEP:f64=0.1;
impl Meter{
    fn new(ch:usize, rate:u32)->Self{
        let (hs,hp)=kweight(rate as f64);
        let w=(0..ch).map(|i| if i>=3 && i<5 {1.41} else {1.0}).collect();
        Self{hs,hp,st:vec![(St::default(),St::default());ch],weights:w,ch,
            block_frames:(rate as usize+5)/10, pos:0, acc:vec![0.0;ch],
            sub:vec![0.0;64], sub_head:0, sub_count:0, hist:vec![0;HN]}
    }
    #[inline]
    fn add(&mut self, samples:&[f64]){
        let ch=self.ch;
        for frame in samples.chunks_exact(ch){
            for c in 0..ch{
                let s=&mut self.st[c];
                let y=self.hp.run(&mut s.1, self.hs.run(&mut s.0, frame[c]));
                self.acc[c]+=y*y;
            }
            self.pos+=1;
            if self.pos==self.block_frames{ self.close_block(); }
        }
    }
    fn close_block(&mut self){
        let n=self.block_frames as f64;
        let mut e=0.0;
        for c in 0..self.ch{ e+=self.weights[c]*self.acc[c]/n; self.acc[c]=0.0; }
        self.pos=0;
        self.sub[self.sub_head]=e; self.sub_head=(self.sub_head+1)%self.sub.len();
        self.sub_count+=1;
        // momentary gating block = mean of last 4 sub-blocks, every sub-block (75% overlap)
        if self.sub_count>=4 {
            let mut m=0.0; for k in 1..=4 { m+=self.sub[(self.sub_head+self.sub.len()-k)%self.sub.len()]; }
            let m=m/4.0;
            let l=-0.691+10.0*m.max(1e-30).log10();
            if l>=HMIN { let i=(((l-HMIN)/HSTEP) as usize).min(HN-1); self.hist[i]+=1; }
        }
    }
    fn integrated(&self)->f64{
        // absolute gate -70 LUFS already applied by histogram floor
        let mut n=0u64; let mut sum=0.0;
        for (i,&c) in self.hist.iter().enumerate(){ if c>0 {
            let l=HMIN+HSTEP*i as f64; sum+=c as f64*10f64.powf((l+0.691)/10.0); n+=c as u64; } }
        if n==0 {return f64::NEG_INFINITY;}
        let rel=-0.691+10.0*(sum/n as f64).log10()-10.0;
        let mut n2=0u64; let mut s2=0.0;
        for (i,&c) in self.hist.iter().enumerate(){ if c>0 {
            let l=HMIN+HSTEP*i as f64;
            if l>=rel { s2+=c as f64*10f64.powf((l+0.691)/10.0); n2+=c as u64; } } }
        if n2==0 {return f64::NEG_INFINITY;}
        -0.691+10.0*(s2/n2 as f64).log10()
    }
    fn momentary(&self)->f64{
        if self.sub_count<4 {return f64::NEG_INFINITY;}
        let mut m=0.0; for k in 1..=4 { m+=self.sub[(self.sub_head+self.sub.len()-k)%self.sub.len()]; }
        -0.691+10.0*(m/4.0).max(1e-30).log10()
    }
}

fn main(){
    let rate=48000u32; let ch=2usize; let frames=4096usize;
    let buf:Vec<f64>=(0..frames*ch).map(|i| ((i as f64)*0.013).sin()*0.5).collect();

    // correctness: compare against ebur128 over 30 s
    let mut mine=Meter::new(ch,rate);
    let mut theirs=ebur128::EbuR128::new(ch as u32, rate, ebur128::Mode::all()).unwrap();
    for _ in 0..(rate as usize*30/frames){ mine.add(&buf); theirs.add_frames_f64(&buf).unwrap(); }
    println!("integrated  mine={:.4}  ebur128={:.4}  delta={:.4} LU",
        mine.integrated(), theirs.loudness_global().unwrap(),
        mine.integrated()-theirs.loudness_global().unwrap());
    println!("momentary   mine={:.4}  ebur128={:.4}  delta={:.4} LU",
        mine.momentary(), theirs.loudness_momentary().unwrap(),
        mine.momentary()-theirs.loudness_momentary().unwrap());

    // perf
    let iters=8000;
    let mut m2=Meter::new(ch,rate);
    let t=Instant::now(); for _ in 0..iters{ m2.add(&buf); }
    let a=t.elapsed().as_secs_f64()/iters as f64;
    let mut t2=ebur128::EbuR128::new(ch as u32,rate,ebur128::Mode::all()).unwrap();
    let t=Instant::now(); for _ in 0..iters{ t2.add_frames_f64(&buf).unwrap(); }
    let b=t.elapsed().as_secs_f64()/iters as f64;
    println!("\ningest in-house : {:.2} ns/sample", a*1e9/frames as f64);
    println!("ingest ebur128  : {:.2} ns/sample", b*1e9/frames as f64);
    println!("speedup         : {:.2}x", b/a);
    // getter cost
    let t=Instant::now(); let mut acc=0.0; for _ in 0..iters{ acc+=m2.integrated()+m2.momentary(); }
    println!("in-house getters: {:.3} us/call (ebur128 momentary alone was ~19 us)", t.elapsed().as_secs_f64()/iters as f64*1e6);
    std::hint::black_box(acc);
}
