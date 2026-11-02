//! Offline generated audio check: no sockets, devices or account access.

use std::time::Duration;
use wechat_calls::{
    audio::{AudioDecoder, AudioEncoder, AudioFormat},
    opus::{OpusDecoder, OpusEncoder},
    tone::PulseTone,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let format = AudioFormat::new(16000, 1, 20)?;
    let mut source = PulseTone::new(
        format,
        880.0,
        0.15,
        Duration::from_millis(500),
        Duration::from_millis(250),
    )?;
    let mut encoder = OpusEncoder::new(format, 20000)?;
    let mut decoder = OpusDecoder::new(format)?;
    let mut packet_bytes = 0;
    let mut decoded_samples = 0;
    for _ in 0..50 {
        let pcm = source.next_frame()?;
        let packet = encoder.encode(&pcm)?;
        packet_bytes += packet.len();
        decoded_samples += decoder.decode(&packet)?.len();
    }
    println!("frames=50 packet_bytes={packet_bytes} decoded_samples={decoded_samples}");
    Ok(())
}
