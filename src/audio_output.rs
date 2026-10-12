//! Plays [`AudioCommand`]s with kira: through the system audio device in a
//! window, or through an offline mixer that scenarios render tick by tick.

use std::path::Path;
use std::time::Duration;

use crate::runtime::{AudioCommand, BusEffect};

/// Sample rate of the offline mix.
pub const MIX_RATE: u32 = 48_000;

/// Files larger than this stream from disk in a window instead of loading
/// whole: tapes, ambience and radio of a few minutes. The offline mixer
/// always loads whole, so test mixes do not depend on disk speed.
pub const STREAM_OVER_BYTES: u64 = 1 << 20;

/// How far an occluded sound falls: its volume times `1 - OCCLUDED_DROP`.
pub const OCCLUDED_DROP: f32 = 0.7;

/// Low-pass cutoff of a fully occluded sound, in Hz.
pub const OCCLUDED_CUTOFF_HZ: f32 = 800.0;

/// One sound that has not ended, as scenarios see it under `audio:/playing`.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct PlayingSound {
    pub id: u64,
    pub clip: String,
    /// Linear gain it plays at: the volume asked for (or the target of its
    /// last fade), times distance and occlusion.
    pub volume: f32,
    pub pan: f32,
    /// `[left, right]` gain from `volume` and `pan` together, before bus
    /// and master volume: see [`pan_gains`].
    pub gain: [f32; 2],
    pub bus: String,
    pub looped: bool,
    /// Fixed tick it starts on.
    pub tick: u64,
    /// Speed and pitch, 1 as recorded.
    pub rate: f32,
    /// Seconds into the clip.
    pub position: f64,
    /// Seconds of real time until it ends at the current rate; `None` when
    /// looped.
    pub remaining: Option<f64>,
    pub paused: bool,
    pub priority: u8,
    /// Where it is in the world, for positioned sounds.
    pub world_position: Option<[f32; 3]>,
    /// 0 clear to 1 behind a wall.
    pub occlusion: f32,
    /// Read from disk while it plays instead of loaded whole.
    pub streamed: bool,
}

/// `[left, right]` gain of a sound at `pan`, kira's constant-power law:
/// both sides 1 at pan 0, and at pan 1 the right side √2 (+3 dB) and the
/// left silent. A hard-panned sound is louder on its side than at centre.
#[must_use]
pub fn pan_gains(pan: f32) -> [f32; 2] {
    let right = (pan.clamp(-1.0, 1.0) + 1.0) * 0.5;
    [(1.0 - right).sqrt(), right.sqrt()]
        .map(|side| side * std::f32::consts::SQRT_2)
}

/// One bus as scenarios see it under `audio:/buses/<name>`; the main
/// output is `""`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct BusReport {
    /// Sounds playing or scheduled on the bus.
    pub voices: usize,
    pub limit: usize,
    /// New sounds refused because the bus was full.
    pub dropped: u64,
    /// Playing sounds stopped to make room for higher-ranked ones.
    pub stolen: u64,
    /// Effects that change the sound, in processing order.
    pub effects: Vec<BusEffect>,
    pub muted: bool,
    pub solo: bool,
    /// `[left, right]` RMS of the bus's output since the last report,
    /// after its effects and volume. On `""` it is the whole mix before
    /// the master volume.
    pub level: [f32; 2],
    /// `[left, right]` largest sample magnitude since the last report.
    pub peak: [f32; 2],
}

/// Sums a bus's output between two reports.
#[cfg(feature = "audio")]
#[derive(Default)]
struct MeterTotals {
    squares: [f64; 2],
    peak: [f32; 2],
    frames: u64,
}

#[cfg(feature = "audio")]
impl MeterTotals {
    /// Level and peak so far, then starts over.
    fn take(&mut self) -> ([f32; 2], [f32; 2]) {
        let totals = std::mem::take(self);
        let frames = totals.frames.max(1) as f64;
        (
            totals.squares.map(|sum| (sum / frames).sqrt() as f32),
            totals.peak,
        )
    }
}

/// A kira effect that leaves the sound alone and meters it.
#[cfg(feature = "audio")]
struct Meter(std::sync::Arc<std::sync::Mutex<MeterTotals>>);

#[cfg(feature = "audio")]
impl kira::effect::Effect for Meter {
    fn process(
        &mut self,
        input: &mut [kira::Frame],
        _dt: f64,
        _info: &kira::info::Info,
    ) {
        let Ok(mut totals) = self.0.lock() else {
            return;
        };
        for frame in input.iter() {
            for (side, sample) in
                [frame.left, frame.right].into_iter().enumerate()
            {
                totals.squares[side] += f64::from(sample * sample);
                totals.peak[side] = totals.peak[side].max(sample.abs());
            }
        }
        totals.frames += input.len() as u64;
    }
}

#[cfg(feature = "audio")]
impl kira::effect::EffectBuilder for Meter {
    type Handle = ();

    fn build(self) -> (Box<dyn kira::effect::Effect>, ()) {
        (Box::new(self), ())
    }
}

#[cfg(feature = "audio")]
enum Handle {
    Static(kira::sound::static_sound::StaticSoundHandle),
    Stream(
        kira::sound::streaming::StreamingSoundHandle<
            kira::sound::FromFileError,
        >,
    ),
}

#[cfg(feature = "audio")]
macro_rules! each_handle {
    ($handle:expr, $inner:ident => $body:expr) => {
        match $handle {
            Handle::Static($inner) => $body,
            Handle::Stream($inner) => $body,
        }
    };
}

#[cfg(feature = "audio")]
struct Voice {
    handle: Handle,
    info: PlayingSound,
    /// Volume asked for, before distance and occlusion.
    volume: f32,
    /// Rate asked for; `info.rate` adds doppler.
    rate: f32,
    falloff: crate::runtime::Falloff,
    doppler: f32,
    /// Clip length in seconds.
    duration: f64,
    reverse: bool,
    /// Own track with a low-pass filter, for occluded sounds.
    muffle:
        Option<(kira::track::TrackHandle, kira::effect::filter::FilterHandle)>,
}

#[cfg(feature = "audio")]
struct Bus {
    /// `None` is the main track.
    track: Option<kira::track::TrackHandle>,
    distortion: kira::effect::distortion::DistortionHandle,
    reverb: kira::effect::reverb::ReverbHandle,
    filter: kira::effect::filter::FilterHandle,
    /// Low shelf, bell and high shelf.
    eq: [kira::effect::eq_filter::EqFilterHandle; 3],
    compressor: kira::effect::compressor::CompressorHandle,
    /// The bus volume, before the meter so the level includes it. The
    /// main output's volume stays on its track.
    volume: kira::effect::volume_control::VolumeControlHandle,
    meter: std::sync::Arc<std::sync::Mutex<MeterTotals>>,
    /// Active effects by kind: distortion, reverb, low-pass, EQ,
    /// compressor.
    effects: [Option<BusEffect>; 5],
    /// Linear volume asked for; `volume` holds 0 instead while silenced.
    gain: f32,
    muted: bool,
    solo: bool,
    /// Muted, or another bus is soloed.
    silent: bool,
    limit: usize,
    dropped: u64,
    stolen: u64,
}

/// Adds a bus's effects to a track builder, all off: tape saturation,
/// then the room, then a wall's low-pass, then the EQ and a compressor,
/// then the bus volume and a meter.
#[cfg(feature = "audio")]
macro_rules! bus_effects {
    ($builder:expr) => {{
        use kira::effect::eq_filter::{EqFilterBuilder, EqFilterKind};
        use kira::effect::{compressor, distortion, filter, reverb};
        let distortion = $builder.add_effect(
            distortion::DistortionBuilder::new()
                .kind(distortion::DistortionKind::SoftClip)
                .mix(0.0),
        );
        let reverb = $builder.add_effect(reverb::ReverbBuilder::new().mix(0.0));
        let filter = $builder
            .add_effect(filter::FilterBuilder::new().cutoff(20_000.0).mix(0.0));
        let eq = [
            (EqFilterKind::LowShelf, EQ_LOW_HZ),
            (EqFilterKind::Bell, EQ_MID_HZ),
            (EqFilterKind::HighShelf, EQ_HIGH_HZ),
        ]
        .map(|(kind, hz)| {
            $builder.add_effect(EqFilterBuilder::new(
                kind,
                hz,
                kira::Decibels(0.0),
                0.7,
            ))
        });
        let compressor =
            $builder.add_effect(compressor::CompressorBuilder::new());
        let volume = $builder.add_effect(
            kira::effect::volume_control::VolumeControlBuilder::default(),
        );
        let meter = std::sync::Arc::<std::sync::Mutex<MeterTotals>>::default();
        $builder.add_effect(Meter(meter.clone()));
        (distortion, reverb, filter, eq, compressor, volume, meter)
    }};
}

#[cfg(feature = "audio")]
pub struct AudioOutput<B: kira::backend::Backend = kira::DefaultBackend> {
    manager: kira::AudioManager<B>,
    clips: std::collections::HashMap<
        std::path::PathBuf,
        kira::sound::static_sound::StaticSoundData,
    >,
    voices: std::collections::BTreeMap<u64, Voice>,
    buses: std::collections::BTreeMap<String, Bus>,
    /// Ear position and unit right vector.
    listener: ([f32; 3], [f32; 3]),
    /// Files over this many bytes stream; `None` loads every file whole.
    stream_over: Option<u64>,
}

#[cfg(feature = "audio")]
impl AudioOutput {
    /// `None` when there is no audio device; the game runs silent.
    pub fn open() -> Option<Self> {
        AudioOutput::with_backend(Default::default(), Some(STREAM_OVER_BYTES))
            .inspect_err(|error| {
                eprintln!("audio: no output device, running silent: {error}");
            })
            .ok()
    }
}

/// A kira backend with no device: [`AudioOutput::render`] pulls samples.
#[cfg(feature = "audio")]
pub struct OfflineBackend(Option<kira::backend::Renderer>);

#[cfg(feature = "audio")]
impl kira::backend::Backend for OfflineBackend {
    type Settings = ();
    type Error = std::convert::Infallible;

    fn setup(
        _: (),
        _internal_buffer_size: usize,
    ) -> Result<(Self, u32), Self::Error> {
        Ok((Self(None), MIX_RATE))
    }

    fn start(
        &mut self,
        renderer: kira::backend::Renderer,
    ) -> Result<(), Self::Error> {
        self.0 = Some(renderer);
        Ok(())
    }
}

/// The mixer scenarios render into.
#[cfg(feature = "audio")]
pub type OfflineMixer = AudioOutput<OfflineBackend>;
#[cfg(not(feature = "audio"))]
pub type OfflineMixer = AudioOutput;

#[cfg(feature = "audio")]
impl AudioOutput<OfflineBackend> {
    pub fn offline() -> Option<Self> {
        Self::with_backend((), None).ok()
    }

    /// An offline mixer that streams files over `bytes`, as a window does.
    #[cfg(test)]
    pub fn offline_streaming(bytes: u64) -> Option<Self> {
        Self::with_backend((), Some(bytes)).ok()
    }

    /// Mixes the next `frames` stereo frames, as `[left, right, ...]`.
    pub fn render(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        if let Some(renderer) = &mut self.manager.backend_mut().0 {
            renderer.on_start_processing();
            renderer.process(&mut out, 2);
        }
        out
    }
}

#[cfg(feature = "audio")]
impl<B: kira::backend::Backend> AudioOutput<B>
where
    B::Settings: Default,
{
    fn with_backend(
        backend_settings: B::Settings,
        stream_over: Option<u64>,
    ) -> Result<Self, B::Error> {
        let mut main = kira::track::MainTrackBuilder::new()
            .sound_capacity(2 * crate::runtime::MAX_VOICE_LIMIT);
        let (distortion, reverb, filter, eq, compressor, volume, meter) =
            bus_effects!(main);
        let manager = kira::AudioManager::new(kira::AudioManagerSettings {
            backend_settings,
            main_track_builder: main,
            capacities: kira::Capacities {
                // Buses plus one track per occluded sound.
                sub_track_capacity: 1024,
                ..Default::default()
            },
            ..Default::default()
        })?;
        let main = Bus {
            track: None,
            distortion,
            reverb,
            filter,
            eq,
            compressor,
            volume,
            meter,
            effects: [None; 5],
            gain: 1.0,
            muted: false,
            solo: false,
            silent: false,
            limit: crate::runtime::DEFAULT_VOICE_LIMIT,
            dropped: 0,
            stolen: 0,
        };
        Ok(Self {
            manager,
            clips: Default::default(),
            voices: Default::default(),
            buses: [(String::new(), main)].into_iter().collect(),
            listener: ([0.0; 3], [1.0, 0.0, 0.0]),
            stream_over,
        })
    }

    /// Runs one command. `delay` gives how long from now a sound for a
    /// fixed tick should start.
    pub fn run(
        &mut self,
        assets: &Path,
        command: AudioCommand,
        delay: impl Fn(u64) -> Duration,
    ) {
        match command {
            AudioCommand::Play {
                id,
                clip,
                tick,
                sound,
            } => self.play(assets, id.0, clip, tick, sound, delay(tick)),
            AudioCommand::Stop(id) => {
                if let Some(mut voice) = self.voices.remove(&id.0) {
                    each_handle!(&mut voice.handle, h => h.stop(kira::Tween::default()));
                }
            }
            AudioCommand::StopAll => {
                for (_, mut voice) in std::mem::take(&mut self.voices) {
                    each_handle!(&mut voice.handle, h => h.stop(kira::Tween::default()));
                }
            }
            AudioCommand::SetMasterVolume(volume) => {
                self.manager
                    .main_track()
                    .set_volume(decibels(volume), kira::Tween::default());
            }
            AudioCommand::SetVolume { id, volume, fade } => {
                if let Some(voice) = self.voices.get_mut(&id.0) {
                    voice.volume = volume;
                }
                self.place(id.0, tween(fade));
            }
            AudioCommand::SetRate { id, rate, fade } => {
                if let Some(voice) = self.voices.get_mut(&id.0) {
                    voice.rate = rate.max(1e-3);
                    voice.set_rate(tween(fade));
                }
            }
            AudioCommand::SetDoppler { id, pitch } => {
                if let Some(voice) = self.voices.get_mut(&id.0) {
                    voice.doppler = pitch;
                    voice.set_rate(tween(0.05));
                }
            }
            AudioCommand::SetPosition { id, position } => {
                if let Some(voice) = self.voices.get_mut(&id.0) {
                    voice.info.world_position = Some(position);
                }
                self.place(id.0, tween(0.02));
            }
            AudioCommand::SetOcclusion { id, amount } => {
                if let Some(voice) = self.voices.get_mut(&id.0) {
                    let amount = amount.clamp(0.0, 1.0);
                    voice.info.occlusion = amount;
                    if let Some((_, filter)) = &mut voice.muffle {
                        filter
                            .set_cutoff(f64::from(cutoff(amount)), tween(0.1));
                    }
                }
                self.place(id.0, tween(0.1));
            }
            AudioCommand::SetListener { position, right } => {
                self.listener = (position, right);
                let ids: Vec<u64> = self.voices.keys().copied().collect();
                for id in ids {
                    self.place(id, tween(0.02));
                }
            }
            AudioCommand::Pause(id) => {
                if let Some(voice) = self.voices.get_mut(&id.0) {
                    each_handle!(&mut voice.handle, h => h.pause(tween(0.0)));
                    voice.info.paused = true;
                }
            }
            AudioCommand::Resume(id) => {
                if let Some(voice) = self.voices.get_mut(&id.0) {
                    each_handle!(&mut voice.handle, h => h.resume(tween(0.0)));
                    voice.info.paused = false;
                }
            }
            AudioCommand::Seek { id, seconds } => {
                if let Some(voice) = self.voices.get_mut(&id.0) {
                    let seconds = f64::from(seconds.max(0.0));
                    each_handle!(&mut voice.handle, h => h.seek_to(seconds));
                }
            }
            AudioCommand::SetBusVolume { bus, volume, fade } => {
                if bus.is_empty() {
                    self.manager
                        .main_track()
                        .set_volume(decibels(volume), tween(fade));
                } else if let Some(bus) = self.bus(&bus) {
                    bus.gain = volume;
                    if !bus.silent {
                        bus.volume.set_volume(decibels(volume), tween(fade));
                    }
                }
            }
            AudioCommand::SetBusMute { bus, muted } => {
                if let Some(bus) = self.bus(&bus) {
                    bus.muted = muted;
                }
                self.silence_buses();
            }
            AudioCommand::SetBusSolo { bus, solo } => {
                if let Some(bus) = self.bus(&bus) {
                    bus.solo = solo;
                }
                self.silence_buses();
            }
            AudioCommand::SetBusEffect { bus, effect, fade } => {
                if let Some(bus) = self.bus(&bus) {
                    bus.set_effect(effect, tween(fade));
                }
            }
            AudioCommand::SetBusVoiceLimit { bus, limit } => {
                if let Some(bus) = self.bus(&bus) {
                    bus.limit = limit.clamp(1, crate::runtime::MAX_VOICE_LIMIT);
                }
            }
        }
    }

    fn play(
        &mut self,
        assets: &Path,
        id: u64,
        clip: String,
        tick: u64,
        sound: crate::runtime::Sound,
        delay: Duration,
    ) {
        let path = assets.join(&clip);
        let streamed = !sound.reverse
            && self.stream_over.is_some_and(|limit| {
                crate::assets::file_len(&path).is_some_and(|len| len > limit)
            });
        if !streamed && !self.clips.contains_key(&path) {
            let loaded = match crate::sfx::clip(&clip) {
                Some(samples) => Ok(builtin_clip(&samples)),
                None => crate::assets::read_file(&path)
                    .map_err(|error| error.to_string())
                    .and_then(|bytes| {
                        kira::sound::static_sound::StaticSoundData::from_cursor(
                            std::io::Cursor::new(bytes),
                        )
                        .map_err(|error| error.to_string())
                    }),
            };
            match loaded {
                Ok(data) => {
                    self.clips.insert(path.clone(), data);
                }
                Err(error) => {
                    eprintln!("audio: cannot play `{clip}`: {error}");
                    return;
                }
            }
        }
        let rate = sound.rate.max(1e-3);
        let mut info = PlayingSound {
            id,
            clip,
            volume: sound.volume,
            pan: sound.pan,
            gain: [0.0; 2],
            bus: sound.bus,
            looped: sound.looped,
            tick,
            rate,
            position: 0.0,
            remaining: None,
            paused: false,
            priority: sound.priority,
            world_position: sound.position.or(sound.follow.map(|_| [0.0; 3])),
            occlusion: 0.0,
            streamed,
        };
        (info.pan, info.volume) =
            self.heard(&info, sound.volume, sound.falloff);
        if !self.make_room(&info) {
            return;
        }
        let start = kira::StartTime::Delayed(delay);
        let mut muffle = None;
        if sound.occlude {
            let mut builder = kira::track::TrackBuilder::new();
            let filter = builder.add_effect(
                kira::effect::filter::FilterBuilder::new().cutoff(20_000.0),
            );
            let track = match &mut self.buses.get_mut(&info.bus).unwrap().track
            {
                Some(parent) => parent.add_sub_track(builder),
                None => self.manager.add_sub_track(builder),
            };
            match track {
                Ok(track) => muffle = Some((track, filter)),
                Err(error) => {
                    eprintln!("audio: cannot occlude `{}`: {error}", info.clip);
                }
            }
        }
        let bus = self.buses.get_mut(&info.bus).unwrap();
        let (handle, duration) = if streamed {
            use kira::sound::streaming::StreamingSoundData;
            // A packed clip streams its decoding from the encoded bytes in
            // memory; a loose one from the file.
            let data = if path.is_file() {
                StreamingSoundData::from_file(&path).map_err(|e| e.to_string())
            } else {
                crate::assets::read_file(&path)
                    .map_err(|error| error.to_string())
                    .and_then(|bytes| {
                        StreamingSoundData::from_cursor(std::io::Cursor::new(
                            bytes,
                        ))
                        .map_err(|error| error.to_string())
                    })
            };
            let data = match data {
                Ok(data) => data,
                Err(error) => {
                    eprintln!("audio: cannot play `{}`: {error}", info.clip);
                    return;
                }
            };
            let duration = data.duration().as_secs_f64();
            let mut data = data
                .volume(decibels(info.volume))
                .panning(info.pan)
                .playback_rate(f64::from(rate))
                .start_time(start);
            if info.looped {
                data = data.loop_region(..);
            }
            let played = match (&mut muffle, &mut bus.track) {
                (Some((track, _)), _) | (None, Some(track)) => {
                    track.play(data).map_err(|error| error.to_string())
                }
                (None, None) => {
                    self.manager.play(data).map_err(|error| error.to_string())
                }
            };
            (played.map(Handle::Stream), duration)
        } else {
            let data = &self.clips[&path];
            let duration = data.duration().as_secs_f64();
            let mut data = data
                .volume(decibels(info.volume))
                .panning(info.pan)
                .playback_rate(f64::from(rate))
                .reverse(sound.reverse)
                .start_time(start);
            if info.looped {
                data = data.loop_region(..);
            }
            let played = match (&mut muffle, &mut bus.track) {
                (Some((track, _)), _) | (None, Some(track)) => {
                    track.play(data).map_err(|error| error.to_string())
                }
                (None, None) => {
                    self.manager.play(data).map_err(|error| error.to_string())
                }
            };
            (played.map(Handle::Static), duration)
        };
        match handle {
            Ok(handle) => {
                info.remaining =
                    (!info.looped).then_some(duration / f64::from(rate));
                self.voices.insert(
                    id,
                    Voice {
                        handle,
                        info,
                        volume: sound.volume,
                        rate,
                        falloff: sound.falloff,
                        doppler: 1.0,
                        duration,
                        reverse: sound.reverse,
                        muffle,
                    },
                );
            }
            Err(error) => {
                eprintln!("audio: cannot play `{}`: {error}", info.clip)
            }
        }
    }

    /// Pan and volume a sound plays at: its own pan, or its side and
    /// distance from the listener, lowered by occlusion.
    fn heard(
        &self,
        info: &PlayingSound,
        volume: f32,
        falloff: crate::runtime::Falloff,
    ) -> (f32, f32) {
        let (pan, gain) = match info.world_position {
            Some(position) => crate::runtime::spatialize(
                self.listener.0,
                self.listener.1,
                position,
                falloff,
            ),
            None => (info.pan, 1.0),
        };
        (pan, volume * gain * (1.0 - OCCLUDED_DROP * info.occlusion))
    }

    /// Moves a voice to its current pan and volume.
    fn place(&mut self, id: u64, tween: kira::Tween) {
        let Some(voice) = self.voices.get(&id) else {
            return;
        };
        let (pan, volume) =
            self.heard(&voice.info, voice.volume, voice.falloff);
        let voice = self.voices.get_mut(&id).unwrap();
        voice.info.pan = pan;
        voice.info.volume = volume;
        each_handle!(&mut voice.handle, h => {
            h.set_panning(pan, tween);
            h.set_volume(decibels(volume), tween);
        });
    }

    /// Whether a new sound may play on its bus, stopping the lowest-ranked
    /// playing one when the bus is full and the new one outranks it.
    /// Ranks are priority, then volume.
    fn make_room(&mut self, new: &PlayingSound) -> bool {
        if self.bus(&new.bus).is_none() {
            return false;
        }
        let limit = self.buses[&new.bus].limit;
        let rank = |info: &PlayingSound| (info.priority, info.volume);
        let mut count = 0;
        let mut lowest: Option<(u64, (u8, f32))> = None;
        for (id, voice) in &self.voices {
            if voice.info.bus != new.bus || voice.stopped() {
                continue;
            }
            count += 1;
            let ranked = rank(&voice.info);
            if lowest.is_none_or(|(_, low)| {
                ranked.0 < low.0 || (ranked.0 == low.0 && ranked.1 < low.1)
            }) {
                lowest = Some((*id, ranked));
            }
        }
        let bus = self.buses.get_mut(&new.bus).unwrap();
        if count < limit {
            return true;
        }
        let ranked = rank(new);
        match lowest {
            Some((victim, low))
                if ranked.0 > low.0
                    || (ranked.0 == low.0 && ranked.1 > low.1) =>
            {
                bus.stolen += 1;
                if let Some(mut voice) = self.voices.remove(&victim) {
                    each_handle!(&mut voice.handle, h => h.stop(kira::Tween::default()));
                }
                true
            }
            _ => {
                bus.dropped += 1;
                false
            }
        }
    }

    /// Sounds that have not ended, in the order they were asked for.
    pub fn playing(&mut self) -> Vec<PlayingSound> {
        self.voices.retain(|_, voice| !voice.stopped());
        self.voices
            .values()
            .map(|voice| {
                let position = each_handle!(&voice.handle, h => h.position());
                let left = if voice.reverse {
                    position
                } else {
                    voice.duration - position
                };
                PlayingSound {
                    position,
                    gain: pan_gains(voice.info.pan)
                        .map(|side| side * voice.info.volume),
                    remaining: (!voice.info.looped)
                        .then(|| left.max(0.0) / f64::from(voice.info.rate)),
                    ..voice.info.clone()
                }
            })
            .collect()
    }

    /// Whether sound `id` is playing or scheduled.
    pub fn has_sound(&self, id: u64) -> bool {
        self.voices.get(&id).is_some_and(|voice| !voice.stopped())
    }

    /// Every bus by name, the main output as `""`. Levels and peaks cover
    /// the audio rendered since the previous call.
    pub fn buses(&mut self) -> std::collections::BTreeMap<String, BusReport> {
        self.voices.retain(|_, voice| !voice.stopped());
        self.buses
            .iter()
            .map(|(name, bus)| {
                let (level, peak) = bus
                    .meter
                    .lock()
                    .map(|mut totals| totals.take())
                    .unwrap_or_default();
                let report = BusReport {
                    voices: self
                        .voices
                        .values()
                        .filter(|voice| voice.info.bus == *name)
                        .count(),
                    limit: bus.limit,
                    dropped: bus.dropped,
                    stolen: bus.stolen,
                    effects: bus.effects.iter().flatten().copied().collect(),
                    muted: bus.muted,
                    solo: bus.solo,
                    level,
                    peak,
                };
                (name.clone(), report)
            })
            .collect()
    }

    /// Applies mute and solo to every bus whose silence changed.
    fn silence_buses(&mut self) {
        let soloing = self
            .buses
            .iter()
            .any(|(name, bus)| !name.is_empty() && bus.solo);
        for (name, bus) in &mut self.buses {
            let silent =
                bus.muted || (soloing && !bus.solo && !name.is_empty());
            if silent != bus.silent {
                bus.silent = silent;
                let gain = if silent { 0.0 } else { bus.gain };
                bus.volume.set_volume(decibels(gain), tween(0.0));
            }
        }
    }

    fn bus(&mut self, name: &str) -> Option<&mut Bus> {
        if !self.buses.contains_key(name) {
            let mut builder = kira::track::TrackBuilder::new()
                .sound_capacity(2 * crate::runtime::MAX_VOICE_LIMIT);
            let (distortion, reverb, filter, eq, compressor, volume, meter) =
                bus_effects!(builder);
            match self.manager.add_sub_track(builder) {
                Ok(track) => {
                    self.buses.insert(
                        name.to_owned(),
                        Bus {
                            track: Some(track),
                            distortion,
                            reverb,
                            filter,
                            eq,
                            compressor,
                            volume,
                            meter,
                            effects: [None; 5],
                            gain: 1.0,
                            muted: false,
                            solo: false,
                            silent: false,
                            limit: crate::runtime::DEFAULT_VOICE_LIMIT,
                            dropped: 0,
                            stolen: 0,
                        },
                    );
                }
                Err(error) => {
                    eprintln!("audio: cannot add bus `{name}`: {error}");
                    return None;
                }
            }
            self.silence_buses();
        }
        self.buses.get_mut(name)
    }
}

#[cfg(feature = "audio")]
impl Voice {
    fn set_rate(&mut self, tween: kira::Tween) {
        let rate = self.rate * self.doppler;
        each_handle!(&mut self.handle, h => h.set_playback_rate(f64::from(rate), tween));
        self.info.rate = rate;
    }

    fn stopped(&self) -> bool {
        each_handle!(&self.handle, h => h.state())
            == kira::sound::PlaybackState::Stopped
    }
}

#[cfg(feature = "audio")]
impl Bus {
    fn set_effect(&mut self, effect: BusEffect, tween: kira::Tween) {
        let on = if effect.active() { 1.0 } else { 0.0 };
        let slot = match effect {
            BusEffect::Distortion { drive, mix } => {
                self.distortion
                    .set_drive(kira::Decibels(drive.clamp(0.0, 48.0)), tween);
                self.distortion.set_mix(mix.clamp(0.0, 1.0), tween);
                0
            }
            BusEffect::Reverb { room, damping, mix } => {
                self.reverb
                    .set_feedback(f64::from(room.clamp(0.0, 0.99)), tween);
                self.reverb
                    .set_damping(f64::from(damping.clamp(0.0, 1.0)), tween);
                self.reverb.set_mix(mix.clamp(0.0, 1.0), tween);
                1
            }
            BusEffect::LowPass { cutoff_hz } => {
                self.filter.set_cutoff(
                    f64::from(cutoff_hz.clamp(20.0, 20_000.0)),
                    tween,
                );
                self.filter.set_mix(on, tween);
                2
            }
            BusEffect::Eq {
                low_db,
                mid_db,
                high_db,
            } => {
                for (band, gain) in
                    self.eq.iter_mut().zip([low_db, mid_db, high_db])
                {
                    band.set_gain(
                        kira::Decibels(gain.clamp(-24.0, 24.0)),
                        tween,
                    );
                }
                3
            }
            BusEffect::Compressor {
                threshold_db,
                ratio,
                makeup_db,
            } => {
                self.compressor
                    .set_threshold(f64::from(threshold_db.min(0.0)), tween);
                self.compressor.set_ratio(f64::from(ratio.max(1.0)), tween);
                self.compressor.set_makeup_gain(
                    kira::Decibels(makeup_db.clamp(0.0, 24.0)),
                    tween,
                );
                4
            }
        };
        self.effects[slot] = effect.active().then_some(effect);
    }
}

/// Corner frequencies of the bus EQ's bands.
#[cfg(feature = "audio")]
const EQ_LOW_HZ: f64 = 200.0;
#[cfg(feature = "audio")]
const EQ_MID_HZ: f64 = 1_000.0;
#[cfg(feature = "audio")]
const EQ_HIGH_HZ: f64 = 5_000.0;

/// Low-pass cutoff for an occlusion amount, falling evenly in octaves.
#[cfg(feature = "audio")]
fn cutoff(amount: f32) -> f32 {
    20_000.0 * (OCCLUDED_CUTOFF_HZ / 20_000.0).powf(amount)
}

#[cfg(feature = "audio")]
fn tween(fade: f32) -> kira::Tween {
    kira::Tween {
        duration: Duration::from_secs_f32(fade.max(0.0)),
        ..Default::default()
    }
}

/// Linear gain as kira decibels; zero or less is silence.
#[cfg(feature = "audio")]
fn decibels(gain: f32) -> kira::Decibels {
    if gain <= 0.0 {
        kira::Decibels::SILENCE
    } else {
        kira::Decibels(20.0 * gain.log10())
    }
}

#[cfg(not(feature = "audio"))]
pub struct AudioOutput;

#[cfg(not(feature = "audio"))]
impl AudioOutput {
    pub fn open() -> Option<Self> {
        None
    }

    pub fn offline() -> Option<Self> {
        None
    }

    pub fn render(&mut self, frames: usize) -> Vec<f32> {
        vec![0.0; frames * 2]
    }

    pub fn run(
        &mut self,
        _assets: &Path,
        _command: AudioCommand,
        _delay: impl Fn(u64) -> Duration,
    ) {
    }

    pub fn playing(&mut self) -> Vec<PlayingSound> {
        Vec::new()
    }

    pub fn has_sound(&self, _id: u64) -> bool {
        false
    }

    pub fn buses(&mut self) -> std::collections::BTreeMap<String, BusReport> {
        Default::default()
    }
}

/// Writes interleaved stereo samples as a 16-bit WAV file.
pub fn write_wav(path: &Path, samples: &[f32]) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&2u16.to_le_bytes()); // channels
    bytes.extend_from_slice(&MIX_RATE.to_le_bytes());
    bytes.extend_from_slice(&(MIX_RATE * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

/// Reads a WAV file [`write_wav`] wrote back into samples.
pub fn read_wav(path: &Path) -> std::io::Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[36..40] != b"data" {
        return Err(std::io::Error::other("not a WAV file from write_wav"));
    }
    Ok(bytes[44..]
        .chunks_exact(2)
        .map(|pair| {
            f32::from(i16::from_le_bytes([pair[0], pair[1]]))
                / f32::from(i16::MAX)
        })
        .collect())
}

#[cfg(feature = "audio")]
/// A built-in `sfx:` clip as sound data: interleaved stereo at [`MIX_RATE`].
fn builtin_clip(samples: &[f32]) -> kira::sound::static_sound::StaticSoundData {
    kira::sound::static_sound::StaticSoundData {
        sample_rate: MIX_RATE,
        frames: samples
            .chunks_exact(2)
            .map(|pair| kira::Frame::new(pair[0], pair[1]))
            .collect(),
        settings: Default::default(),
        slice: None,
    }
}

#[cfg(all(test, feature = "audio"))]
mod tests {
    use super::*;
    use crate::runtime::{AudioQueue, Sound};

    /// Plays one second of a rising ramp from a fresh WAV with `sound`,
    /// then mixes `frames`.
    fn mix(mixer: &mut OfflineMixer, sound: Sound, frames: usize) -> Vec<f32> {
        let directory = std::env::temp_dir()
            .join(format!("rusting-mix-{}", uuid::Uuid::new_v4()));
        let ramp: Vec<f32> = (0..48_000)
            .flat_map(|i| {
                let s = i as f32 / 48_000.0 * 0.5;
                [s, s]
            })
            .collect();
        write_wav(&directory.join("ramp.wav"), &ramp).unwrap();
        let mut queue = AudioQueue::default();
        queue.play("ramp.wav", &sound, 0);
        for command in queue.drain() {
            mixer.run(&directory, command, |_| Duration::ZERO);
        }
        let out = mixer.render(frames);
        std::fs::remove_dir_all(&directory).unwrap();
        out
    }

    #[test]
    fn builtin_sfx_clips_play_with_no_file() {
        let mut mixer = OfflineMixer::offline().unwrap();
        let mut queue = AudioQueue::default();
        queue.play("sfx:coin 7", &Sound::default(), 0);
        let empty = std::env::temp_dir().join("rusting-no-assets");
        for command in queue.drain() {
            mixer.run(&empty, command, |_| Duration::ZERO);
        }
        let out = mixer.render(4800);
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05, "silent: {peak}");
        assert_eq!(mixer.playing()[0].clip, "sfx:coin 7");
    }

    #[test]
    fn large_files_stream_and_still_mix() {
        let mut mixer = OfflineMixer::offline_streaming(1024).unwrap();
        // A streamed sound starts once its decoder thread fills a buffer.
        let mut heard = false;
        for _ in 0..200 {
            let block = mix_more(&mut mixer);
            if block.iter().any(|s| s.abs() > 1e-3) {
                heard = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(heard);
        assert!(mixer.playing()[0].streamed);

        fn mix_more(mixer: &mut OfflineMixer) -> Vec<f32> {
            if mixer.playing().is_empty() {
                mix(mixer, Sound::default(), 480)
            } else {
                mixer.render(480)
            }
        }
    }

    #[test]
    fn a_packed_clip_plays_whole_and_streamed() {
        let directory = std::env::temp_dir()
            .join(format!("rusting-mix-pack-{}", uuid::Uuid::new_v4()));
        let assets = directory.join("assets");
        let ramp: Vec<f32> = (0..48_000).flat_map(|_| [0.5, 0.5]).collect();
        write_wav(&assets.join("ramp.wav"), &ramp).unwrap();
        crate::assets::pack_folder(&assets, &directory.join("assets.rpak"))
            .unwrap();
        std::fs::remove_dir_all(&assets).unwrap();
        for mut mixer in [
            OfflineMixer::offline().unwrap(),
            OfflineMixer::offline_streaming(1024).unwrap(),
        ] {
            let mut queue = AudioQueue::default();
            queue.play("ramp.wav", &Sound::default(), 0);
            for command in queue.drain() {
                mixer.run(&assets, command, |_| Duration::ZERO);
            }
            // A streamed sound starts once its decoder thread fills a buffer.
            let heard = (0..200).any(|_| {
                let block = mixer.render(480);
                std::thread::sleep(Duration::from_millis(5));
                block.iter().any(|s| s.abs() > 0.1)
            });
            assert!(heard, "packed clip is silent");
        }
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn pan_gains_match_the_mix() {
        let [left, right] = pan_gains(0.0);
        assert!((left - 1.0).abs() < 1e-6 && (right - 1.0).abs() < 1e-6);
        let [left, right] = pan_gains(1.0);
        assert!(left.abs() < 1e-6);
        assert!((right - std::f32::consts::SQRT_2).abs() < 1e-6);
        let pan = |pan: f32| {
            mix(
                &mut OfflineMixer::offline().unwrap(),
                Sound {
                    pan,
                    ..Sound::default()
                },
                480,
            )
        };
        let (centre, hard) = (pan(0.0), pan(1.0));
        let at = centre.len() - 1;
        assert!(hard[at - 1].abs() < 1e-4, "left is silent");
        let ratio = hard[at] / centre[at];
        assert!((ratio - pan_gains(1.0)[1]).abs() < 0.01, "{ratio}");
    }

    #[test]
    fn reversed_sounds_start_from_the_end() {
        let forward =
            mix(&mut OfflineMixer::offline().unwrap(), Sound::default(), 480);
        let backward = mix(
            &mut OfflineMixer::offline().unwrap(),
            Sound {
                reverse: true,
                ..Sound::default()
            },
            480,
        );
        let tail = |block: &[f32]| block[block.len() - 2];
        // The ramp rises forward and starts near its top reversed.
        assert!(tail(&forward) < 0.01, "{}", tail(&forward));
        assert!(tail(&backward) > 0.4, "{}", tail(&backward));
    }
}
