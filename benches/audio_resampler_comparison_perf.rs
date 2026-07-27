#![allow(dead_code)]

mod resampler_comparison_support;
mod support;

fn main() -> Result<(), String> {
    resampler_comparison_support::run(std::env::args().skip(1).collect())
}
