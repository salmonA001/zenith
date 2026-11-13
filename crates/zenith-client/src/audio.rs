//! Sound with no sound files: every effect is synthesised when it plays.
use zenith_content::Sound;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sfx {
    Break(Sound),
    Place(Sound),
    Step(Sound),
    Hit,
    Hurt,
    Death,
    Explosion,
    Fizz,
    Splash,
    Pickup,
    Eat,
    Click,
    Shoot,
    Open,
}

/// A tiny noise source: sound needs no quality, only speed.
struct Noise(u32);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// How a material sounds: how bright its noise is, how long it rings, and a tone under it.
fn material(s: Sound) -> (f32, f32, f32) {
    match s {
        Sound::Stone => (0.55, 0.09, 180.0),
        Sound::Dirt => (0.18, 0.1, 90.0),
        Sound::Grass => (0.35, 0.1, 0.0),
        Sound::Sand => (0.6, 0.12, 0.0),
        Sound::Wood => (0.3, 0.08, 240.0),
        Sound::Glass => (0.9, 0.16, 1400.0),
        Sound::Wool => (0.1, 0.09, 0.0),
        Sound::Snow => (0.45, 0.12, 0.0),
        Sound::Metal => (0.8, 0.2, 880.0),
    }
}

/// Renders an effect into samples at `rate`. Deterministic, and quick enough to do on the spot.
pub fn synth(sfx: Sfx, rate: f32, seed: u32) -> Vec<f32> {
    let mut noise = Noise(seed | 1);
    let mut lp = 0.0f32;
    let mut out;
    let mut render = |length: f32, f: &mut dyn FnMut(f32, f32, &mut Noise, &mut f32) -> f32| -> Vec<f32> {
        let n = (length * rate) as usize;
        let mut v = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f32 / rate;
            let p = i as f32 / n as f32;
            v.push(f(t, p, &mut noise, &mut lp));
        }
        v
    };
    match sfx {
        Sfx::Break(m) | Sfx::Place(m) | Sfx::Step(m) => {
            let (bright, ring, tone) = material(m);
            let (gain, length) = match sfx {
                Sfx::Break(_) => (0.8, ring * 2.2),
                Sfx::Place(_) => (0.6, ring * 1.2),
                _ => (0.35, ring * 0.8),
            };
            out = render(length, &mut |t, p, n, lp| {
                *lp += (n.next() - *lp) * bright;
                let body = if tone > 0.0 { (t * tone * std::f32::consts::TAU).sin() * 0.5 } else { 0.0 };
                (*lp + body) * (1.0 - p).powi(3) * gain
            });
        }
        Sfx::Hit => {
            out = render(0.12, &mut |t, p, n, _| {
                ((t * 220.0 * std::f32::consts::TAU).sin() * 0.6 + n.next() * 0.4) * (1.0 - p).powi(2) * 0.7
            });
        }
        Sfx::Hurt => {
            out = render(0.22, &mut |t, p, n, _| {
                let f = 280.0 - p * 120.0;
                ((t * f * std::f32::consts::TAU).sin() * 0.7 + n.next() * 0.2) * (1.0 - p) * 0.8
            });
        }
        Sfx::Death => {
            out = render(0.6, &mut |t, p, _, _| {
                let f = 220.0 - p * 150.0;
                (t * f * std::f32::consts::TAU).sin() * (1.0 - p).powi(2) * 0.8
            });
        }
        Sfx::Explosion => {
            out = render(1.4, &mut |_, p, n, lp| {
                let cutoff = 0.5 * (1.0 - p).powi(2) + 0.01;
                *lp += (n.next() - *lp) * cutoff;
                *lp * 3.0 * (1.0 - p).powi(2)
            });
        }
        Sfx::Fizz => {
            out = render(0.5, &mut |_, p, n, _| n.next() * (1.0 - p).powi(2) * 0.35);
        }
        Sfx::Splash => {
            out = render(0.45, &mut |_, p, n, lp| {
                *lp += (n.next() - *lp) * (0.5 - p * 0.3);
                *lp * (1.0 - p).powi(2) * 0.8
            });
        }
        Sfx::Pickup => {
            out = render(0.09, &mut |t, p, _, _| {
                (t * (900.0 + p * 700.0) * std::f32::consts::TAU).sin() * (1.0 - p) * 0.25
            });
        }
        Sfx::Eat => {
            out = render(0.25, &mut |t, p, n, lp| {
                *lp += (n.next() - *lp) * 0.3;
                let chew = ((t * 22.0).sin() * 0.5 + 0.5).powi(2);
                *lp * chew * (1.0 - p) * 0.8
            });
        }
        Sfx::Click => {
            out = render(0.04, &mut |t, p, _, _| (t * 1800.0 * std::f32::consts::TAU).sin() * (1.0 - p).powi(3) * 0.5);
        }
        Sfx::Shoot => {
            out = render(0.16, &mut |t, p, n, _| {
                ((t * (700.0 - p * 400.0) * std::f32::consts::TAU).sin() * 0.3 + n.next() * 0.25) * (1.0 - p).powi(2)
            });
        }
        Sfx::Open => {
            out = render(0.2, &mut |t, p, n, lp| {
                *lp += (n.next() - *lp) * 0.2;
                (*lp + (t * 130.0 * std::f32::consts::TAU).sin() * 0.3) * (1.0 - p) * 0.5
            });
        }
    }
    // Clicks at the ends of a sound are the only thing that would give away how little this is.
    let fade = (rate * 0.002) as usize;
    let len = out.len();
    for i in 0..fade.min(len) {
        let g = i as f32 / fade as f32;
        out[i] *= g;
        out[len - 1 - i] *= g;
    }
    out
}

#[cfg(feature = "audio")]
mod device {
    use super::{Sfx, synth};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::mpsc::{self, Sender};

    pub struct Output {
        tx: Sender<(Sfx, f32)>,
    }

    impl Output {
        pub fn start() -> Option<Self> {
            let (tx, rx) = mpsc::channel::<(Sfx, f32)>();
            // The stream lives on a thread of its own: on some platforms it cannot be sent around.
            std::thread::Builder::new()
                .name("audio".into())
                .spawn(move || {
                    let host = cpal::default_host();
                    let Some(device) = host.default_output_device() else { return };
                    let Ok(config) = device.default_output_config() else { return };
                    if config.sample_format() != cpal::SampleFormat::F32 {
                        return;
                    }
                    let rate = config.sample_rate() as f32;
                    let channels = config.channels() as usize;
                    let stream_config: cpal::StreamConfig = config.into();
                    let (voices_tx, voices_rx) = mpsc::channel::<Vec<f32>>();
                    let mut voices: Vec<(Vec<f32>, usize)> = Vec::new();
                    let stream = device.build_output_stream(
                        stream_config,
                        move |data: &mut [f32], _| {
                            while let Ok(v) = voices_rx.try_recv() {
                                voices.push((v, 0));
                            }
                            for frame in data.chunks_mut(channels) {
                                let mut sum = 0.0;
                                for (samples, at) in &mut voices {
                                    if let Some(s) = samples.get(*at) {
                                        sum += *s;
                                    }
                                    *at += 1;
                                }
                                let sum = sum.clamp(-1.0, 1.0);
                                frame.fill(sum);
                            }
                            voices.retain(|(samples, at)| *at < samples.len());
                        },
                        |e| eprintln!("zenith: audio stream: {e}"),
                        None,
                    );
                    let Ok(stream) = stream else { return };
                    if stream.play().is_err() {
                        return;
                    }
                    let mut seed = 0x1234_5678u32;
                    while let Ok((sfx, volume)) = rx.recv() {
                        seed = seed.wrapping_mul(0x9E37_79B1).wrapping_add(1);
                        let mut samples = synth(sfx, rate, seed);
                        for s in &mut samples {
                            *s *= volume;
                        }
                        let _ = voices_tx.send(samples);
                    }
                    drop(stream);
                })
                .ok()?;
            Some(Self { tx })
        }

        pub fn play(&self, sfx: Sfx, volume: f32) {
            let _ = self.tx.send((sfx, volume));
        }
    }
}

/// The game's sound output. Silent if there is no device, the `audio` feature is off, or
/// `ZENITH_NO_AUDIO` is set, and the game plays the same either way.
pub struct Audio {
    #[cfg(feature = "audio")]
    output: Option<device::Output>,
    pub volume: f32,
}

impl Audio {
    pub fn start() -> Self {
        #[cfg(feature = "audio")]
        {
            let output = if std::env::var_os("ZENITH_NO_AUDIO").is_some() { None } else { device::Output::start() };
            Self { output, volume: 0.7 }
        }
        #[cfg(not(feature = "audio"))]
        {
            Self { volume: 0.7 }
        }
    }

    pub fn play(&self, sfx: Sfx, volume: f32) {
        let v = volume * self.volume;
        if v <= 0.001 {
            return;
        }
        #[cfg(feature = "audio")]
        if let Some(o) = &self.output {
            o.play(sfx, v);
        }
        #[cfg(not(feature = "audio"))]
        let _ = sfx;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sound_stays_in_range_and_is_not_silent() {
        let materials = [
            Sound::Stone,
            Sound::Dirt,
            Sound::Grass,
            Sound::Sand,
            Sound::Wood,
            Sound::Glass,
            Sound::Wool,
            Sound::Snow,
            Sound::Metal,
        ];
        let mut all: Vec<Sfx> = vec![
            Sfx::Hit,
            Sfx::Hurt,
            Sfx::Death,
            Sfx::Explosion,
            Sfx::Fizz,
            Sfx::Splash,
            Sfx::Pickup,
            Sfx::Eat,
            Sfx::Click,
            Sfx::Shoot,
            Sfx::Open,
        ];
        for m in materials {
            all.extend([Sfx::Break(m), Sfx::Place(m), Sfx::Step(m)]);
        }
        for sfx in all {
            let s = synth(sfx, 44_100.0, 7);
            assert!(!s.is_empty(), "{sfx:?}");
            let peak = s.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
            assert!(peak > 0.005, "{sfx:?} is silent");
            assert!(peak <= 3.0, "{sfx:?} peaks at {peak}");
            assert!(s.iter().all(|v| v.is_finite()));
            assert!(s[0].abs() < 0.01, "{sfx:?} starts with a click");
        }
    }

    #[test]
    fn a_sound_is_repeatable_from_its_seed() {
        assert_eq!(synth(Sfx::Fizz, 8000.0, 3), synth(Sfx::Fizz, 8000.0, 3));
        assert_ne!(synth(Sfx::Fizz, 8000.0, 3), synth(Sfx::Fizz, 8000.0, 4));
    }
}
