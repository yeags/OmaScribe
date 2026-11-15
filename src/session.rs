use crate::audio::Effects;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Kind {
    Measure,
    Beat,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub time: f64,
    pub kind: Kind,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Session {
    pub audio: PathBuf,
    pub markers: Vec<Marker>,
    pub selection: Option<(f64, f64)>,
    pub effects: Effects,
    pub looping: bool,
}
impl Session {
    pub fn read(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let mut session: Self =
            serde_json::from_slice(&bytes).map_err(|e| format!("Invalid session: {e}"))?;
        if !session.effects.speed.is_finite()
            || !(0.25..=1.5).contains(&session.effects.speed)
            || session
                .effects
                .gains
                .iter()
                .any(|g| !g.is_finite() || g.abs() > 12.)
            || session
                .markers
                .iter()
                .any(|m| !m.time.is_finite() || m.time < 0.)
            || session
                .selection
                .is_some_and(|(a, b)| !a.is_finite() || !b.is_finite() || a < 0. || b <= a)
        {
            return Err("Session contains invalid times or effects.".into());
        }
        session.markers.sort_by(|a, b| a.time.total_cmp(&b.time));
        if session.audio.is_relative() {
            session.audio = path.parent().unwrap_or(Path::new(".")).join(&session.audio);
        }
        Ok(session)
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let parent = path.parent().unwrap_or(Path::new("."));
        let temp = parent.join(format!(".omascribe-{}.tmp", std::process::id()));
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        let result = (|| {
            file.write_all(&serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            std::fs::rename(&temp, path).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.omascribe");
        let s = Session {
            audio: "song.wav".into(),
            markers: vec![Marker {
                time: 1.25,
                kind: Kind::Measure,
            }],
            ..Default::default()
        };
        s.save(&path).unwrap();
        let read = Session::read(&path).unwrap();
        assert_eq!(read.markers, s.markers);
        assert_eq!(read.audio, dir.path().join("song.wav"));
        std::fs::write(&path, r#"{"audio":"a","markers":[],"selection":[5,2],"effects":{"speed":1,"gains":[0,0,0,0,0],"vocal_cut":false},"looping":false}"#).unwrap();
        assert!(Session::read(&path).is_err());
    }
}
