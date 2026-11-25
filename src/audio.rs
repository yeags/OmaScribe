use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effects {
    pub speed: f64,
    pub gains: [f32; 5],
    pub vocal_cut: bool,
}
impl Default for Effects {
    fn default() -> Self {
        Self {
            speed: 1.0,
            gains: [0.; 5],
            vocal_cut: false,
        }
    }
}
pub const BANDS: [u32; 5] = [80, 250, 1000, 4000, 10000];
impl Effects {
    pub fn filters(&self) -> String {
        let mut filters = vec!["aformat=channel_layouts=stereo".to_owned()];
        if self.vocal_cut {
            filters.push("pan=stereo|c0=0.5*c0-0.5*c1|c1=0.5*c1-0.5*c0".into());
        }
        for (frequency, gain) in BANDS.iter().zip(self.gains) {
            filters.push(format!("equalizer=f={frequency}:t=o:w=1:g={gain}"));
        }
        let mut speed = self.speed.clamp(0.25, 1.5);
        while speed < 0.5 {
            filters.push("atempo=0.5".into());
            speed /= 0.5;
        }
        filters.push(format!("atempo={speed}"));
        filters.push("alimiter=limit=0.95:level=false:latency=true".into());
        filters.join(",")
    }
}

pub fn decode(path: &Path, rate: u32, effects: &Effects) -> Result<Vec<f32>, String> {
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-af",
            &effects.filters(),
            "-ar",
            &rate.to_string(),
            "-ac",
            "2",
            "-f",
            "f32le",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!("Cannot start FFmpeg: {e}. Install ffmpeg and ensure it is on PATH.")
        })?;
    let stderr = child.stderr.take().unwrap();
    // Drain diagnostics concurrently so a malformed file cannot fill the pipe and deadlock.
    let errors = std::thread::spawn(move || {
        let mut message = String::new();
        let _ = stderr.take(65536).read_to_string(&mut message);
        message
    });
    let mut bytes = Vec::new();
    let limit = 512 * 1024 * 1024;
    let result = child
        .stdout
        .take()
        .unwrap()
        .take(limit + 1)
        .read_to_end(&mut bytes);
    if result.is_err() || bytes.len() as u64 > limit {
        let _ = child.kill();
        let _ = child.wait();
        let _ = errors.join();
        return Err(
            "Audio exceeds the 512 MiB processing limit. Use a shorter file or a faster tempo."
                .into(),
        );
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let message = errors.join().unwrap_or_default();
    if !status.success() {
        return Err(format!(
            "FFmpeg could not read the audio: {}",
            message.trim()
        ));
    }
    let samples: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    if samples.len() < 2 {
        return Err("The file contains no audio samples.".into());
    }
    Ok(samples)
}

#[derive(Default)]
pub struct Transport {
    pub samples: Arc<Vec<f32>>,
    pub frame: usize,
    pub playing: bool,
    pub rate: u32,
    pub speed: f64,
    pub duration: f64,
    pub selection: Option<(f64, f64)>,
    pub looping: bool,
    pub volume: f32,
}
impl Transport {
    pub fn set_audio(&mut self, samples: Arc<Vec<f32>>, speed: f64, position: f64) {
        self.samples = samples;
        self.speed = speed;
        self.seek(position);
    }
    pub fn position(&self) -> f64 {
        self.frame as f64 / self.rate.max(1) as f64 * self.speed
    }
    pub fn seek(&mut self, seconds: f64) {
        self.frame =
            ((seconds.clamp(0., self.duration) / self.speed.max(0.25)) * self.rate as f64) as usize;
        self.frame = self.frame.min(self.samples.len() / 2);
    }
    fn next(&mut self) -> [f32; 2] {
        if !self.playing {
            return [0.; 2];
        }
        let end = if self.looping {
            self.selection.map(|s| s.1).unwrap_or(self.duration)
        } else {
            self.duration
        };
        if self.frame >= self.samples.len() / 2 || self.position() >= end {
            if self.looping {
                self.seek(self.selection.map(|s| s.0).unwrap_or(0.));
            } else {
                self.playing = false;
                return [0.; 2];
            }
        }
        let index = self.frame * 2;
        if index + 1 >= self.samples.len() {
            self.playing = false;
            return [0.; 2];
        }
        self.frame += 1;
        [
            (self.samples[index] * self.volume).clamp(-1., 1.),
            (self.samples[index + 1] * self.volume).clamp(-1., 1.),
        ]
    }
}

pub struct Engine {
    pub state: Arc<Mutex<Transport>>,
    pub rate: u32,
    _stream: cpal::Stream,
}
impl Engine {
    pub fn new(errors: std::sync::mpsc::Sender<String>) -> Result<Self, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("No default audio output device is available.")?;
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        let rate = config.sample_rate().0;
        let state = Arc::new(Mutex::new(Transport {
            rate,
            speed: 1.,
            volume: 0.75,
            ..Default::default()
        }));
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                stream::<f32>(&device, &config.into(), state.clone(), errors)
            }
            cpal::SampleFormat::I16 => {
                stream::<i16>(&device, &config.into(), state.clone(), errors)
            }
            cpal::SampleFormat::U16 => {
                stream::<u16>(&device, &config.into(), state.clone(), errors)
            }
            other => return Err(format!("Unsupported audio output format: {other:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Self {
            state,
            rate,
            _stream: stream,
        })
    }
}
fn stream<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    state: Arc<Mutex<Transport>>,
    errors: std::sync::mpsc::Sender<String>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let channels = config.channels as usize;
    device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            if let Ok(mut transport) = state.try_lock() {
                for frame in data.chunks_mut(channels) {
                    let sample = transport.next();
                    for (channel, value) in frame.iter_mut().enumerate() {
                        *value = T::from_sample(if channels == 1 {
                            (sample[0] + sample[1]) * 0.5
                        } else {
                            sample[channel % 2]
                        });
                    }
                }
            } else {
                data.fill(T::from_sample(0.));
            }
        },
        move |error| {
            let _ = errors.send(format!("Audio output error: {error}"));
        },
        None,
    )
}

pub fn peaks(samples: &[f32]) -> Vec<f32> {
    samples
        .chunks(512)
        .map(|chunk| chunk.iter().fold(0f32, |peak, s| peak.max(s.abs())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn switching_playback_buffers_keeps_song_position_and_loop() {
        let original = Arc::new(vec![0.25; 200]);
        let modified = Arc::new(vec![0.75; 400]);
        let mut t = Transport {
            rate: 10,
            duration: 10.,
            speed: 0.5,
            frame: 100,
            samples: modified.clone(),
            playing: true,
            volume: 1.,
            looping: true,
            selection: Some((4., 6.)),
        };
        t.set_audio(original, 1., t.position());
        assert_eq!(t.position(), 5.);
        assert_eq!(t.next(), [0.25; 2]);
        t.set_audio(modified, 0.5, t.position());
        assert_eq!(t.position(), 5.1);
        assert_eq!(t.next(), [0.75; 2]);
        assert!(t.looping && t.playing);
        assert_eq!(t.selection, Some((4., 6.)));
    }
    #[test]
    fn looping_and_source_clock() {
        let mut t = Transport {
            samples: Arc::new(vec![0.25; 40]),
            rate: 10,
            speed: 0.5,
            duration: 1.,
            volume: 1.,
            playing: true,
            looping: true,
            selection: Some((0.2, 0.4)),
            ..Default::default()
        };
        t.seek(0.35);
        t.next();
        assert!((t.position() - 0.4).abs() < 0.001);
        t.next();
        assert!((t.position() - 0.25).abs() < 0.001);
        t.looping = false;
        t.seek(1.);
        assert_eq!(t.next(), [0.; 2]);
        assert!(!t.playing);
    }
    #[test]
    fn ffmpeg_preserves_pitch_and_reduces_center() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        assert!(
            Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=1",
                    "-ac",
                    "2"
                ])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        for speed in [0.25, 0.5, 0.75, 1., 1.5] {
            let data = decode(
                &path,
                44100,
                &Effects {
                    speed,
                    ..Default::default()
                },
            )
            .unwrap();
            let duration = data.len() as f64 / 88200.;
            assert!(
                (duration - 1. / speed).abs() < 0.15,
                "speed {speed}: duration {duration}"
            );
            let mono: Vec<f32> = data.as_chunks::<2>().0.iter().map(|x| x[0]).collect();
            let crossings = mono.windows(2).filter(|s| s[0] <= 0. && s[1] > 0.).count();
            assert!((crossings as f64 / duration - 440.).abs() < 12.);
        }
        let cut = decode(
            &path,
            44100,
            &Effects {
                vocal_cut: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(cut.iter().all(|x| x.abs() < 0.0001));
    }
}
