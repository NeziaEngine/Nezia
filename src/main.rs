use resia::engine::SoundEngine;

fn main() {
    let mut engine = SoundEngine::new().expect("failed to initialize sound engine");
    let mut volume = 0.2_f32;

    println!("Playing 440 Hz sine wave. Volume: {volume:.2}");
    println!("Commands:  u = volume up,  d = volume down,  q = quit");

    let stdin = std::io::stdin();
    let mut line = String::new();

    loop {
        line.clear();
        if stdin.read_line(&mut line).is_err() {
            break;
        }

        match line.trim() {
            "u" => {
                volume = (volume + 0.05).min(1.0);
                let _ = engine.set_volume(volume);
                println!("Volume: {volume:.2}");
            }
            "d" => {
                volume = (volume - 0.05).max(0.0);
                let _ = engine.set_volume(volume);
                println!("Volume: {volume:.2}");
            }
            "q" => break,
            _ => {
                println!("Commands:  u = volume up,  d = volume down,  q = quit");
            }
        }
    }
}
