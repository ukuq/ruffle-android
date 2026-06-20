use ndk::audio::{AudioDirection, AudioFormat, AudioStream, AudioStreamBuilder, AudioStreamState};
use ruffle_core::backend::audio::{
    swf, AudioBackend, AudioMixer, DecodeError, RegisterError, SoundHandle, SoundInstanceHandle,
    SoundStreamInfo, SoundTransform,
};

use ruffle_core::impl_audio_mixer_backend;

pub struct AAudioAudioBackend {
    pub stream: Option<AudioStream>,
    pub mixer: AudioMixer,
    pub paused: bool,
}

type Error = Box<dyn std::error::Error>;

impl AAudioAudioBackend {
    pub fn new() -> Result<Self, Error> {
        let mixer = AudioMixer::new(2, 44100);

        let mut result = Self {
            stream: None,
            mixer,
            paused: true,
        };

        result.recreate_stream()?;

        Ok(result)
    }

    pub fn recreate_stream(&mut self) -> Result<(), Error> {
        let proxy = self.mixer.proxy();

        let stream = AudioStreamBuilder::new()?
            .direction(AudioDirection::Output)
            .format(AudioFormat::PCM_Float)
            .channel_count(2)
            .sample_rate(44100)
            .performance_mode(ndk::audio::AudioPerformanceMode::LowLatency)
            .data_callback(Box::new(move |_stream, data, len| {
                let sl = unsafe {
                    std::slice::from_raw_parts_mut::<f32>(data as *mut f32, len as usize * 2)
                };
                proxy.mix(sl);
                ndk::audio::AudioCallbackResult::Continue
            }))
            .open_stream()?;

        if !self.paused {
            stream.request_start()?;
        }

        self.stream = Some(stream);
        Ok(())
    }

    pub fn recreate_stream_if_needed(&mut self) {
        let should_recreate = self
            .stream
            .as_ref()
            .map(|stream| stream.state() == AudioStreamState::Disconnected)
            .unwrap_or(true);

        if should_recreate {
            if let Err(error) = self.recreate_stream() {
                log::warn!("Error recreating disconnected audio stream: {error}");
                self.paused = true;
            }
        }
    }

    pub fn resume_output(&mut self) {
        <Self as AudioBackend>::play(self);
    }

    pub fn pause_output(&mut self) {
        <Self as AudioBackend>::pause(self);
    }
}

impl AudioBackend for AAudioAudioBackend {
    impl_audio_mixer_backend!(mixer);

    fn play(&mut self) {
        self.paused = false;

        if self.stream.is_none() {
            if let Err(error) = self.recreate_stream() {
                log::warn!("Error recreating audio stream before resume: {error}");
                self.paused = true;
            }
            return;
        }

        if let Some(stream) = self.stream.as_mut() {
            if let Err(error) = stream.request_start() {
                log::warn!("Error trying to resume audio stream: {error}; recreating stream");
                self.stream = None;
                if let Err(error) = self.recreate_stream() {
                    log::warn!("Error recreating audio stream after resume failure: {error}");
                    self.paused = true;
                }
            }
        }
    }

    fn pause(&mut self) {
        self.paused = true;

        if let Some(stream) = self.stream.as_mut() {
            if let Err(error) = stream.request_pause() {
                log::warn!("Error trying to pause audio stream: {error}");
            }
        }
    }
}
