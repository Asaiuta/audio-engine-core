#[path = "support/automix_accuracy/mod.rs"]
pub mod accuracy;
pub mod support;

fn main() -> Result<(), String> {
    let options = accuracy::Options::parse(std::env::args().skip(1))?;
    if options.help {
        println!("audio_automix_accuracy [--quick] [--corpus-manifest PATH] [--corpus-root DIR] [--require-corpus ID ...] [--enforce] [--out PATH]\nOffline only. --quick runs synthetic audio and visibly skips external metrics.");
        return Ok(());
    }
    let mut report = accuracy::run(&options)?;
    let passed = report
        .metrics
        .iter()
        .filter(|metric| metric.classification == "gate" && metric.passed == Some(true))
        .count();
    let failed = report
        .metrics
        .iter()
        .filter(|metric| metric.classification == "gate" && metric.passed == Some(false))
        .count();
    let skipped = report
        .metrics
        .iter()
        .filter(|metric| metric.classification == "skipped")
        .count();
    println!("AutoMix accuracy: {passed} gates passed, {failed} failed, {skipped} skipped; {} input errors", report.input_errors.len());
    report.write_then_enforce(&options)
}
