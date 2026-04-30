use nezia::engine::SoundEngine;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <audio_file> [audio_file...]", args[0]);
        eprintln!("Supported formats: MP3, WAV, FLAC, OGG Vorbis");
        std::process::exit(1);
    }

    let mut engine = SoundEngine::new().expect("failed to initialize sound engine");

    // コマンドライン引数で指定されたファイルをロードする。
    let mut buffer_indices = Vec::new();
    for path in &args[1..] {
        match engine.load(path) {
            Ok(index) => {
                println!("Loaded: {path} (buffer index: {index})");
                buffer_indices.push(index);
            }
            Err(e) => {
                eprintln!("Failed to load {path}: {e}");
            }
        }
    }

    if buffer_indices.is_empty() {
        eprintln!("No audio files loaded.");
        std::process::exit(1);
    }

    // 最初のファイルを再生する。
    let _ = engine.play(buffer_indices[0], 1.0, 1.0);
    println!("Playing buffer 0. Volume: 1.00, Pitch: 1.00");

    println!("Commands:");
    println!("  p <index> [vol] [pitch] — play buffer (e.g. 'p 0 0.8 1.5')");
    println!("  v <volume>              — set master volume");
    println!("  s                       — stop all");
    println!("  q                       — quit");

    let stdin = std::io::stdin();
    let mut line = String::new();

    loop {
        line.clear();
        if stdin.read_line(&mut line).is_err() {
            break;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        match parts[0] {
            "p" => {
                let idx: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                let vol: f32 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(1.0);
                let pitch: f32 = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(1.0);
                let _ = engine.play(idx, vol, pitch);
                println!("Playing buffer {idx}. Volume: {vol:.2}, Pitch: {pitch:.2}");
            }
            "v" => {
                let vol: f32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(1.0);
                let _ = engine.set_volume(vol);
                println!("Master volume: {vol:.2}");
            }
            "s" => {
                let _ = engine.stop_all();
                println!("Stopped all voices.");
            }
            "q" => break,
            _ => {
                println!("Unknown command. Use p/v/s/q.");
            }
        }
    }
}
