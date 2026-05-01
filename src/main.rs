use nezia::buffer_pool::BufferId;
use nezia::engine::SoundEngine;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut engine = SoundEngine::new().expect("failed to initialize sound engine");

    // コマンドライン引数で指定されたファイルをロードする。
    let mut buffers: Vec<BufferId> = Vec::new();
    for path in &args[1..] {
        match engine.load(path) {
            Ok(id) => {
                let idx = buffers.len();
                println!("Loaded: {path} (buffer #{idx})");
                buffers.push(id);
            }
            Err(e) => {
                eprintln!("Failed to load {path}: {e}");
            }
        }
    }

    if !buffers.is_empty() {
        let _ = engine.play(buffers[0], 1.0, 1.0);
        println!("Playing buffer #0. Volume: 1.00, Pitch: 1.00");
    }

    println!("Commands:");
    println!("  l <path>                — load audio file");
    println!("  u <index>               — unload buffer");
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
            "l" => {
                let Some(path) = parts.get(1) else {
                    println!("Usage: l <path>");
                    continue;
                };
                match engine.load(path) {
                    Ok(id) => {
                        let idx = buffers.len();
                        println!("Loaded: {path} (buffer #{idx})");
                        buffers.push(id);
                    }
                    Err(e) => {
                        eprintln!("Failed to load {path}: {e}");
                    }
                }
            }
            "u" => {
                let Some(idx) = parts.get(1).and_then(|s| s.parse::<usize>().ok()) else {
                    println!("Usage: u <index>");
                    continue;
                };
                let Some(&id) = buffers.get(idx) else {
                    println!("Invalid buffer index: {idx}");
                    continue;
                };
                if engine.unload(id) {
                    println!("Unloaded buffer #{idx}");
                } else {
                    println!("Buffer #{idx} is already unloaded.");
                }
            }
            "p" => {
                let idx: usize = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                let vol: f32 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(1.0);
                let pitch: f32 = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(1.0);
                let Some(&id) = buffers.get(idx) else {
                    println!("Invalid buffer index: {idx}");
                    continue;
                };
                if engine.play(id, vol, pitch) {
                    println!("Playing buffer #{idx}. Volume: {vol:.2}, Pitch: {pitch:.2}");
                } else {
                    println!("Failed to play buffer #{idx} (unloaded or queue full).");
                }
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
                println!("Unknown command. Use l/u/p/v/s/q.");
            }
        }
    }
}
