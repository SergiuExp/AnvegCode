//! Write the "Claude finished" chime to a WAV file, to listen to it:
//! `cargo run -p anveg_core --example chime -- chime.wav`

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "chime.wav".to_owned());
    match std::fs::write(&path, anveg_core::chime::finished_wav()) {
        Ok(()) => println!("wrote {path}"),
        Err(error) => {
            eprintln!("could not write {path}: {error}");
            std::process::exit(1);
        }
    }
}
