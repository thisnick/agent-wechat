//! Mix two synthetic tracks without sockets, devices or a native codec.
use wechat_calls::{
    audio::{AudioFormat, AudioFrame},
    mixer::PcmMixer,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let format = AudioFormat::new(16000, 1, 20)?;
    let mut mixer = PcmMixer::new(format, 2, 3, 0)?;
    for (track, level) in [(7, 1000), (8, 2000)] {
        mixer.add_track(track, 1)?;
        mixer.push(
            track,
            AudioFrame {
                generation: 1,
                sequence: 0,
                timestamp: 0,
                samples: vec![level; format.samples_per_frame()],
            },
        )?;
    }
    let mixed = mixer.render(0)?;
    assert!(mixed.samples.iter().all(|sample| *sample == 3000));
    println!(
        "tracks={} samples={} first_sample={}",
        mixed.contributing_tracks.len(),
        mixed.samples.len(),
        mixed.samples[0]
    );
    Ok(())
}
