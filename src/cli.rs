use crate::session::Session;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub fn run() -> Option<i32> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let command = args.first()?.to_str()?;
    match command {
        "--help" | "-h" => {
            println!(
                "OmaScribe — music transcription workbench\n\nomascribe [AUDIO | SESSION.omascribe]\nomascribe check FILE [--json]\nomascribe skill install\n\nSpace: play/pause   M: measure   B: beat   ?: shortcuts"
            );
            Some(0)
        }
        "--version" | "-V" => {
            println!("OmaScribe {}", env!("CARGO_PKG_VERSION"));
            Some(0)
        }
        "check" => {
            let json = args.iter().any(|a| a == "--json");
            let result = args
                .get(1)
                .filter(|a| *a != "--json")
                .ok_or("Usage: omascribe check FILE [--json]".into())
                .and_then(|path| check(Path::new(path)));
            match result {
                Ok(report) => {
                    if json {
                        println!("{report}");
                    } else {
                        println!(
                            "Audio is readable: {} seconds, {} Hz, {} channels",
                            report["duration_seconds"], report["sample_rate"], report["channels"]
                        );
                    }
                    Some(0)
                }
                Err(error) => {
                    if json {
                        eprintln!("{}", serde_json::json!({"ok":false,"error":error}));
                    } else {
                        eprintln!("{error}");
                    }
                    Some(1)
                }
            }
        }
        "skill" => {
            let result = if args.get(1).is_some_and(|a| a == "install") {
                install_skill()
            } else {
                Err("Usage: omascribe skill install".into())
            };
            match result {
                Ok(path) => {
                    println!("Installed {}", path.display());
                    Some(0)
                }
                Err(e) => {
                    eprintln!("{e}");
                    Some(1)
                }
            }
        }
        _ => None,
    }
}
fn check(path: &Path) -> Result<serde_json::Value, String> {
    let session = if path.extension().is_some_and(|x| x == "omascribe") {
        Some(Session::read(path)?)
    } else {
        None
    };
    let audio = session.as_ref().map(|s| s.audio.as_path()).unwrap_or(path);
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=sample_rate,channels:format=duration",
            "-of",
            "json",
        ])
        .arg(audio)
        .output()
        .map_err(|e| format!("Cannot start ffprobe: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let probe: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let stream = probe["streams"]
        .as_array()
        .and_then(|s| s.first())
        .ok_or("File has no audio stream")?;
    Ok(
        serde_json::json!({"ok":true,"audio":audio,"duration_seconds":probe["format"]["duration"].as_str().and_then(|d| d.parse::<f64>().ok()),"sample_rate":stream["sample_rate"],"channels":stream["channels"],"markers":session.as_ref().map(|s| s.markers.len()).unwrap_or(0)}),
    )
}
fn install_skill() -> Result<PathBuf, String> {
    let root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))
        .ok_or("Cannot determine skill directory")?;
    let dir = root.join("skills/omascribe");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("SKILL.md");
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| format!("Will not replace an existing skill: {e}"))?;
    file.write_all(include_bytes!("../AGENT_SKILL.md"))
        .map_err(|e| e.to_string())?;
    Ok(path)
}
