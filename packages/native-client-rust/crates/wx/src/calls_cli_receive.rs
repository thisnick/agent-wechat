//! Version-three binary receive tracks and timed local playback, backed by `wechat-call-client`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use wechat_call_client::audio::ReceiveAudio;
    use wechat_calls::audio::AudioFormat;
    use wechat_core::{
        call_types::{CallAudioFormat, CallTrack, CallTrackDirection},
        calls::pcm_envelope::{encode_pcm, MediaHeader},
    };

    fn track(id: u32, generation: u32) -> CallTrack {
        CallTrack::Audio {
            id,
            generation,
            direction: CallTrackDirection::Receive,
            format: CallAudioFormat::default(),
            participant_id: Some(format!("person-{id}")),
        }
    }

    fn frame(id: u32, generation: u32, sequence: u64, offset: u64, sample: i16) -> Vec<u8> {
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        encode_pcm(
            MediaHeader {
                track_id: id,
                generation,
                sequence,
                timestamp_micros: offset * 1_000_000 / 16000,
            },
            format,
            &[sample; 320],
        )
        .unwrap()
    }

    #[test]
    fn simultaneous_sources_mix_once_with_delay_and_separate_levels() {
        let mut rx = ReceiveAudio::new(
            AudioFormat::new(16000, 1, 20).unwrap(),
            vec![track(7, 1), track(8, 1)],
        )
        .unwrap();
        let mut levels = levels::CallLevels::for_test();
        rx.binary_frame(
            &frame(7, 1, 0, 3200, 1000),
            Duration::ZERO,
            |track_id, participant_id, samples| {
                levels.participant_audio(track_id, participant_id, samples);
            },
        )
        .unwrap();
        rx.binary_frame(
            &frame(8, 1, 0, 3200, 2000),
            Duration::from_millis(5),
            |track_id, participant_id, samples| {
                levels.participant_audio(track_id, participant_id, samples);
            },
        )
        .unwrap();
        assert!(rx.render(Duration::from_millis(59)).unwrap().is_none());
        assert_eq!(
            rx.render(Duration::from_millis(60)).unwrap().unwrap(),
            vec![3000; 320]
        );
        assert!(rx.render(Duration::from_millis(61)).unwrap().is_none());
        assert_eq!(
            rx.render(Duration::from_millis(100)).unwrap().unwrap(),
            vec![0; 320]
        );
        levels.refresh();
        assert!(levels.participant_level("person-8") > levels.participant_level("person-7"));
    }
}
