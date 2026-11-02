//! Manual generated-audio check for a disposable sound server, never host defaults.
use std::time::{Duration, Instant};
use wechat_audio::{AudioConfig, AudioError, AudioSession};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.len() != 3 || arguments[0] != "--isolated-fixture" {
        return Err("usage: isolated_loopback --isolated-fixture <capture-source> <playback-sink>; use only an isolated null-sink monitor".into());
    }
    if arguments[1].is_empty() || arguments[2].is_empty() {
        return Err("both fixture device names are required; defaults are prohibited".into());
    }
    let mut session = AudioSession::open(AudioConfig {
        input_device: Some(arguments[1].clone()),
        output_device: Some(arguments[2].clone()),
        sample_rate: 16_000,
        playback_buffer_ms: 200,
    })?;
    let warmup = Instant::now();
    loop {
        match session.recv_timeout(Duration::from_millis(100)) {
            Ok(samples) if !samples.is_empty() => break,
            Ok(_) | Err(AudioError::Timeout) if warmup.elapsed() < Duration::from_secs(3) => {}
            Ok(_) | Err(AudioError::Timeout) => return Err("fixture capture did not start".into()),
            Err(error) => return Err(error.into()),
        }
    }
    eprintln!(
        "capture_started elapsed_ms={}",
        warmup.elapsed().as_millis()
    );
    let playback = session.playback();
    let token = playback.token()?;
    let start = Instant::now();
    let mut next = start;
    let mut sample_index = 0usize;
    let mut count = 0usize;
    let mut energy = 0.0f64;
    while start.elapsed() < Duration::from_secs(2) {
        if Instant::now() >= next {
            let mut tone = [0.0f32; 480];
            for sample in &mut tone {
                // Generated 1kHz at a quiet amplitude, pulsed every quarter-second.
                let pulse = (sample_index / 12_000).is_multiple_of(2);
                *sample = if pulse {
                    (std::f32::consts::TAU * 1_000.0 * sample_index as f32 / 48_000.0).sin() * 0.1
                } else {
                    0.0
                };
                sample_index += 1;
            }
            if let Err(error) = playback.enqueue(token, &tone, 48_000) {
                eprintln!("enqueue_failed elapsed_ms={} generated_samples={sample_index} captured_samples={count} energy={energy:.6}", start.elapsed().as_millis());
                return Err(error.into());
            }
            next += Duration::from_millis(10);
        }
        match session.recv_timeout(Duration::from_millis(2)) {
            Ok(samples) => {
                count += samples.len();
                energy += samples.iter().map(|&s| (s as f64).powi(2)).sum::<f64>();
            }
            Err(AudioError::Timeout) => {}
            Err(error) => return Err(error.into()),
        }
    }
    let status = session.aec_status();
    playback.clear()?;
    session.stop()?;
    let rms = (energy / count.max(1) as f64).sqrt();
    println!("captured_samples={count} generated_samples={sample_index} rms={rms:.6} aec={status:?} stopped=true");
    if count < 16_000 || rms < 0.005 || !rms.is_finite() {
        return Err("isolated loopback did not return enough generated audio".into());
    }
    Ok(())
}
