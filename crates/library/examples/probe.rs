//! Prints what the scanner sees in a file. Useful when a track does not appear
//! in the library and you need to know whether the tag reader refused it.
//!
//! ```text
//! cargo run -p library --example probe -- "D:\music\track.mp3"
//! ```

fn main() {
    let mut any = false;
    for argument in std::env::args().skip(1) {
        any = true;
        let path = std::path::PathBuf::from(&argument);
        println!("{}", path.display());
        println!("  audio by extension: {}", library::is_audio_file(&path));

        match library::metadata::probe(&path) {
            Ok(probed) => {
                println!("  codec:      {:?}", probed.codec);
                println!("  duration:   {:.1}s", probed.properties.duration);
                println!("  bitrate:    {:?}", probed.properties.bitrate);
                println!("  sample rate:{:?}", probed.properties.sample_rate);
                println!("  channels:   {:?}", probed.properties.channels);
                println!("  title:      {:?}", probed.tags.title);
                println!("  artist:     {:?}", probed.tags.artist);
                println!("  album:      {:?}", probed.tags.album);
                println!("  replaygain: {:?}", probed.tags.replay_gain);
                println!(
                    "  artwork:    {}",
                    probed
                        .picture
                        .map(|picture| {
                            // The MIME the tag claims and the bytes it actually
                            // holds: a cover the image crate refuses is nearly
                            // always one where those two disagree.
                            let head: Vec<String> = picture
                                .data
                                .iter()
                                .take(16)
                                .map(|byte| format!("{byte:02x}"))
                                .collect();
                            format!(
                                "{} bytes, mime {:?}, first bytes {}",
                                picture.data.len(),
                                picture.mime,
                                head.join(" ")
                            )
                        })
                        .unwrap_or_else(|| "none".to_owned())
                );
            }
            Err(error) => println!("  REFUSED: {error:#}"),
        }
    }

    if !any {
        eprintln!("usage: probe <file>...");
    }
}
