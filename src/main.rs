use ddnet_rust::datafile::Datafile;
use ddnet_rust::map_visual::VisualMap;
use std::path::Path;
use ddnet_rust::map::GameMap;
use ddnet_rust::graphics::{Framebuffer, Camera};
use ddnet_rust::physics::Vec2;
use ddnet_rust::netobjects::CharacterCoreNet;
use ddnet_rust::layers::PhysicsLayers;
use ddnet_rust::physics::{Character, Input, JumpEvent, Tuning};
use ddnet_rust::protocol::{decode_int, write_int};
use ddnet_rust::transport::probe_server;
use ddnet_rust::session::{GameAction, GameSession06, Stage};
use ddnet_rust::huffman::{reference_inputs, Huffman};
use ddnet_rust::audio::{AudioMixer, Sample, VoiceOptions, MAX_BLOCK_FRAMES};
use ddnet_rust::sound_assets::decode_wavpack;
use ddnet_rust::server_browser::{ClientSettings, fetch_master, parse_master, matches_filter};

const CODEC_VECTORS: &[i32] = &[
    i32::MIN, -65536, -65, -64, -2, -1, 0, 1, 63, 64, 255, 65535, i32::MAX,
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => print_help(),
        [arg] if arg == "-h" || arg == "--help" => print_help(),
        [arg] if arg == "--version" => println!("DDNet-Rust 0.1.0 (upstream 20.2.1)"),
        [arg] if arg == "--self-test" => self_test(),
        [arg] if arg == "--codec-vectors" => codec_vectors(),
        [arg] if arg == "--huffman-vectors" => huffman_vectors(),
        [arg, path] if arg == "--inspect-map" => inspect_map(path),
        [arg, path] if arg == "--inspect-game-map" => inspect_game_map(path),
        [arg, path, output] if arg == "--render-debug-map" => render_debug_map(path, output, false),
        [arg, path, output] if arg == "--render-debug-png" => render_debug_map(path, output, true),
        [arg, path, assets, output] if arg == "--render-textured-png" => render_textured_map(path, assets, output),
        [arg, path] if arg == "--inspect-layers" => inspect_layers(path),
        [arg, addr] if arg == "--probe-server" => probe(addr),
        [arg, addr] if arg == "--connect-headless" => connect_headless(addr, None),
        [arg, addr, out] if arg == "--capture-headless" => connect_headless(addr, Some(out)),
        [arg, map] if arg == "--play-offline" => play_offline(map),
        [arg, map, output] if arg == "--play-offline-smoke" => play_offline_smoke(map, output),
        [arg, host] if arg == "--play-online" => play_online(host),
        [arg] if arg == "--servers" => servers_list(None, None),
        [arg, query] if arg == "--servers" => servers_list(None, Some(query)),
        [arg, file] if arg == "--servers-file" => servers_list(Some(file), None),
        [arg, file, query] if arg == "--servers-file" => servers_list(Some(file), Some(query)),
        [arg] if arg == "--settings" => print_client_settings(),
        [arg, name] if arg == "--set-name" => set_client_name(name),
        [arg] if arg == "--browse-servers" => browse_servers(),
        [arg, output] if arg == "--audio-smoke" => audio_smoke(output),
        [arg, file, output] if arg == "--audio-wv-preview" => audio_wv_preview(file, output),
        _ => {
            eprintln!("Unknown arguments. Run ddnet-rust --help.");
            std::process::exit(2);
        }
    }
}

fn print_help() {
    println!("DDNet-Rust (work in progress; not yet a playable client)");
    println!("  --self-test         Run isolated physics/protocol smoke tests");
    println!("  --codec-vectors     Emit integer vectors for C++ differential testing");
    println!("  --huffman-vectors   Compare compressed output against upstream C++");
    println!("  --inspect-map PATH  Parse and validate a v3/v4 DDNet map's metadata");
    println!("  --render-debug-map MAP OUT.ppm  Export collision map to PPM");
    println!("  --render-debug-png MAP OUT.png  Export collision map to PNG");
    println!("  --render-textured-png MAP DATA_DIR OUT.png  Render PNG/embedded visual tilemap layers");
    println!("  --probe-server HOST[:PORT]  Probe a live DDNet 0.6 UDP handshake");
    println!("  --connect-headless HOST[:PORT]  Test DDNet 0.6 login and map download");
    println!("  --capture-headless HOST[:PORT] OUT.png  Capture first decoded snapshot");
    println!("  --play-offline MAP   Offline window: cargo run --features window -- --play-offline MAP");
    println!("  --play-offline-smoke MAP PNG   Run scripted window test under Xvfb");
    println!("  --play-online HOST   Experimental 0.6 UDP snapshot window (feature: window)");
    println!("  --audio-smoke OUT.wav   Export a synthetic PCM mixer test");
    println!("  --audio-wv-preview SOUND.wv OUT.wav   Decode a real DDNet sound to stereo WAV");
    println!("  --servers [FILTER]  Fetch official master 0.6 servers (name/map/mode)");
    println!("  --servers-file JSON [FILTER]  Offline master JSON fixture");
    println!("  --browse-servers    Native window browser (feature: window)");
    println!("  --set-name NICK     Save nickname for 0.6 login (max 15 bytes)");
    println!("  --settings          Show saved nickname/filter/last server");
    println!("  --version           Print project and upstream versions");
}

fn self_test() {
    let mut character = Character::default();
    let event = character.tick_without_collision(
        Input { direction: 1, jump: true },
        true,
        &Tuning::default(),
    );
    assert_eq!(event, JumpEvent::Ground);
    assert_eq!(character.velocity.x, 2.0);
    assert_eq!(character.velocity.y, -13.2);

    let mut bytes = Vec::new();
    write_int(i32::MIN, &mut bytes);
    assert_eq!(decode_int(&bytes), Ok((i32::MIN, bytes.len())));
    println!("PASS: isolated movement + integer codec smoke tests");
}

fn codec_vectors() {
    for &value in CODEC_VECTORS {
        let mut bytes = Vec::new();
        write_int(value, &mut bytes);
        print!("{value} ");
        for byte in bytes {
            print!("{byte:02x}");
        }
        println!();
    }
}

fn inspect_map(path: &str) {
    let bytes = std::fs::read(path).unwrap_or_else(|error| {
        eprintln!("Unable to open {path}: {error}");
        std::process::exit(1);
    });
    let map = Datafile::parse(&bytes).unwrap_or_else(|error| {
        eprintln!("Invalid DDNet datafile {path}: {error:?}");
        std::process::exit(1);
    });
    println!(
        "{path}: DATA v{}, {} item types, {} items, {} raw blocks",
        map.version,
        map.item_types.len(),
        map.items.len(),
        map.raw_blocks.len()
    );
}

fn inspect_game_map(path: &str) {
    let bytes = std::fs::read(path).unwrap_or_else(|error| {
        eprintln!("Unable to open {path}: {error}");
        std::process::exit(1);
    });
    let file = Datafile::parse(&bytes).unwrap_or_else(|error| {
        eprintln!("Invalid DDNet datafile {path}: {error:?}");
        std::process::exit(1);
    });
    let game = GameMap::from_datafile(&file, 16_777_216).unwrap_or_else(|error| {
        eprintln!("Invalid game collision layer {path}: {error:?}");
        std::process::exit(1);
    });
    let solids = game
        .tiles
        .iter()
        .filter(|tile| matches!(tile.index, 1 | 3))
        .count();
    println!(
        "{path}: collision layer {}x{}, {} solid/nohook tiles",
        game.width, game.height, solids
    );
}

fn inspect_layers(path: &str) {
    let bytes = std::fs::read(path).unwrap_or_else(|error| {
        eprintln!("Unable to open {path}: {error}");
        std::process::exit(1);
    });
    let file = Datafile::parse(&bytes).unwrap_or_else(|error| {
        eprintln!("Invalid map datafile {path}: {error:?}");
        std::process::exit(1);
    });
    let layers = PhysicsLayers::from_datafile(&file, 16_777_216).unwrap_or_else(|error| {
        eprintln!("Invalid physics layers {path}: {error:?}");
        std::process::exit(1);
    });
    println!(
        "{path}: {}x{} front={} tele={} speedup={} switch={} tune={}",
        layers.game.width, layers.game.height,
        layers.front.is_some(), layers.tele.is_some(),
        layers.speedup.is_some(), layers.switches.is_some(), layers.tune.is_some(),
    );
}

fn probe(endpoint: &str) {
    let address = if endpoint.contains(':') {
        endpoint.to_owned()
    } else {
        format!("{endpoint}:8303")
    };
    match probe_server(address.as_str(), 5) {
        Ok(peer) => println!(
            "DDNet 0.6 transport handshake succeeded with {peer}; gameplay is NOT yet implemented"
        ),
        Err(error) => {
            eprintln!("DDNet 0.6 handshake failed: {error}");
            std::process::exit(1);
        }
    }
}

fn huffman_vectors() {
    let codec = Huffman::new();
    for (index, input) in reference_inputs().into_iter().enumerate() {
        let output = codec.compress(&input, 10_000).expect("Huffman reference input");
        print!("{index} ");
        for byte in output {
            print!("{byte:02x}");
        }
        println!();
    }
}

fn connect_headless(endpoint: &str, screenshot: Option<&str>) {
    use std::net::{ToSocketAddrs, UdpSocket};
    use std::time::{Duration, Instant};

    let address = if endpoint.contains(':') {
        endpoint.to_owned()
    } else {
        format!("{endpoint}:8303")
    };
    let peer = address
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| addresses.next())
        .unwrap_or_else(|| {
            eprintln!("Unable to resolve DDNet server: {address}");
            std::process::exit(2);
        });
    let socket = UdpSocket::bind(if peer.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" })
        .unwrap_or_else(|error| {
            eprintln!("UDP bind failed: {error}");
            std::process::exit(1);
        });
    socket.connect(peer).unwrap_or_else(|error| {
        eprintln!("UDP connect failed: {error}");
        std::process::exit(1);
    });
    socket.set_read_timeout(Some(Duration::from_millis(250))).unwrap();

    let mut session = GameSession06::new(String::new(), 64 * 1024 * 1024, 16_777_216);
    let hello = session.connect().expect("start fresh DDNet transport");
    socket.send(&hello).unwrap_or_else(|error| {
        eprintln!("Initial packet failed: {error}");
        std::process::exit(1);
    });
    println!("DDNet 0.6: connecting to {peer} (headless, 12 second cap)");

    let deadline = Instant::now() + Duration::from_secs(12);
    let mut last_connect = Instant::now();
    let mut last_keepalive = Instant::now();
    let mut incoming = [0u8; 1400];

    while Instant::now() < deadline {
        match socket.recv(&mut incoming) {
            Ok(length) => {
                let actions = match session.receive(&incoming[..length]) {
                    Ok(actions) => actions,
                    Err(error) => {
                        eprintln!("Discarding unsupported or invalid packet: {error:?}");
                        continue;
                    }
                };
                for action in actions {
                    match action {
                        GameAction::Outgoing(bytes) => {
                            if let Err(error) = socket.send(&bytes) {
                                eprintln!("UDP write failed: {error}");
                                std::process::exit(1);
                            }
                        }
                        GameAction::TransportConnected => println!("Transport: connected"),
                        GameAction::MapAnnounced { name, size } =>
                            println!("Server map: {name}, {size} bytes"),
                        GameAction::MapLoaded { name, width, height } =>
                            println!("Map validated: {name} ({width}x{height} tiles)"),
                        GameAction::StartInfoSent => {
                            println!("Cl_StartInfo sent; waiting for Sv_ReadyToEnter");
                        }
                        GameAction::EnterGameSent => {
                            println!("ENTERGAME transmitted; waiting for first snapshot...");
                        }
                        GameAction::SnapshotReceived { tick, packed_bytes } => {
                            println!("Snapshot received: tick {tick}, {packed_bytes} packed bytes.");
                        }
                        GameAction::SnapshotDecoded { tick, item_count } => {
                            println!("Snapshot decoded: tick {tick}, {item_count} objects.");
                            if let Some(path) = screenshot {
                                capture_game_state(&session, tick, path);
                            }
                            println!("The original DDNet sprite renderer is still missing.");
                            return;
                        }
                        GameAction::Disconnected(reason) => {
                            eprintln!("Server closed connection: {reason}");
                            std::process::exit(1);
                        }
                    }
                }
            }
            Err(error) if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) => {}
            Err(error) => {
                eprintln!("UDP receive failed: {error}");
                std::process::exit(1);
            }
        }
        if session.stage == Stage::Connecting
            && last_connect.elapsed() >= Duration::from_millis(500)
        {
            let _ = socket.send(&hello);
            last_connect = Instant::now();
        }
        if session.transport.state == ddnet_rust::transport::State::Online {
            match session.transport.poll_retransmission(Instant::now()) {
                Ok(Some(retry)) => {
                    if let Err(error) = socket.send(&retry) {
                        eprintln!("Vital resend failed: {error}");
                        std::process::exit(1);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    eprintln!("Reliable transport stopped: {error:?}");
                    std::process::exit(1);
                }
            }
        }
        if session.transport.state == ddnet_rust::transport::State::Online
            && last_keepalive.elapsed() >= Duration::from_secs(1)
        {
            if let Ok(bytes) = session.transport.keepalive() {
                let _ = socket.send(&bytes);
            }
            last_keepalive = Instant::now();
        }
    }

    eprintln!("Timed out waiting for DDNet 0.6 map/READY handshake. Stage: {:?}", session.stage);
    std::process::exit(1);
}

fn render_debug_map(path: &str, output: &str, png: bool) {
    let bytes = std::fs::read(path).unwrap_or_else(|error| {
        eprintln!("Unable to open map {path}: {error}");
        std::process::exit(1);
    });
    let file = Datafile::parse(&bytes).unwrap_or_else(|error| {
        eprintln!("Invalid map file {path}: {error:?}");
        std::process::exit(1);
    });
    let map = GameMap::from_datafile(&file, 16_777_216).unwrap_or_else(|error| {
        eprintln!("Unable to read collision tile layer {path}: {error:?}");
        std::process::exit(1);
    });
    let mut frame = Framebuffer::new(960, 540).expect("fixed diagnostic resolution");
    let world_width = map.width as f32 * 32.0;
    let world_height = map.height as f32 * 32.0;
    let scale = (960.0 / world_width)
        .min(540.0 / world_height)
        .clamp(0.05, 4.0);
    frame.render_map(&map, Camera {
        center: Vec2::new(world_width / 2.0, world_height / 2.0),
        scale,
    }).expect("bounded map camera");
    let rendered = if png {
        frame.png_bytes().expect("bounded debug PNG encoding")
    } else {
        frame.ppm_bytes()
    };
    std::fs::write(output, rendered).unwrap_or_else(|error| {
        eprintln!("Unable to write debug frame {output}: {error}");
        std::process::exit(1);
    });
    println!("Wrote debug frame {output}: {}x{} tiles", map.width, map.height);
}

fn render_textured_map(path: &str, asset_root: &str, output: &str) {
    let bytes = std::fs::read(path).unwrap_or_else(|error| {
        eprintln!("Unable to open map {path}: {error}");
        std::process::exit(1);
    });
    let datafile = Datafile::parse(&bytes).unwrap_or_else(|error| {
        eprintln!("Invalid DDNet map {path}: {error:?}");
        std::process::exit(1);
    });
    let visual = VisualMap::from_datafile(&datafile, Path::new(asset_root))
        .unwrap_or_else(|error| {
            eprintln!("Cannot load map textures {path}: {error:?}");
            std::process::exit(1);
        });
    if visual.layers.is_empty() {
        eprintln!("No regular image-backed tile layers in {path}");
        std::process::exit(1);
    }
    // Use a meaningful map coordinate window instead of fitting a huge
    // map to a tiny thumbnail (where each tile becomes subpixel).
    let width = visual.layers.iter().map(|l| l.width).max().unwrap_or(30);
    let height = visual.layers.iter().map(|l| l.height).max().unwrap_or(20);
    let camera = Camera {
        center: Vec2::new(width as f32 * 16.0, height as f32 * 16.0),
        scale: (960.0 / (width as f32 * 32.0))
            .min(540.0 / (height as f32 * 32.0))
            .clamp(0.05, 2.0),
    };
    let mut frame = Framebuffer::new(960, 540).expect("fixed framebuffer");
    let drawn = visual.render(&mut frame, camera).unwrap_or_else(|error| {
        eprintln!("Texture render failed: {error:?}");
        std::process::exit(1);
    });
    let png = frame.png_bytes().expect("bounded PNG encoding");
    std::fs::write(output, png).unwrap_or_else(|error| {
        eprintln!("Unable to write {output}: {error}");
        std::process::exit(1);
    });
    println!(
        "Rendered {drawn} tiles from {} textured layers / {} images to {output}",
        visual.layers.len(), visual.images.len(),
    );
}

fn capture_game_state(session: &GameSession06, tick: i32, output: &str) {
    let Some(map) = session.loaded_map.as_ref() else {
        eprintln!("Cannot capture: no validated DDNet map");
        std::process::exit(1);
    };
    let Some(state) = session.snapshot_state(tick) else {
        eprintln!("Cannot capture: snapshot history unavailable");
        std::process::exit(1);
    };

    let center = state.items.iter()
        .find_map(CharacterCoreNet::from_object)
        .map(|character| Vec2::new(character.x as f32, character.y as f32))
        .unwrap_or_else(|| Vec2::new(
            map.width as f32 * 16.0,
            map.height as f32 * 16.0,
        ));
    let camera = Camera { center, scale: 1.0 };
    let mut frame = Framebuffer::new(960, 540).expect("fixed 960x540 capture");
    frame.render_map(map, camera).expect("valid snapshot camera");
    let rendered = frame.render_snapshot(state, camera).expect("bounded snapshot rendering");
    let bytes = frame.png_bytes().expect("fixed-resolution PNG encoding");
    std::fs::write(output, bytes).unwrap_or_else(|error| {
        eprintln!("Failed to save PNG screenshot {output}: {error}");
        std::process::exit(1);
    });
    println!("Saved snapshot {tick} to {output} (characters drawn: {rendered})");
}

#[cfg(feature = "window")]
fn play_offline(path: &str) {
    if let Err(error) = ddnet_rust::window::play_offline(path) {
        eprintln!("Offline physics window failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(feature = "window"))]
fn play_offline(_path: &str) {
    eprintln!("This command requires --features window");
    std::process::exit(2);
}

#[cfg(feature = "window")]
fn play_online(endpoint: &str) {
    if let Err(error) = ddnet_rust::window_online::play_online(endpoint) {
        eprintln!("Online prototype failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(feature = "window"))]
fn play_online(_endpoint: &str) {
    eprintln!("This command requires --features window");
    std::process::exit(2);
}

#[cfg(feature = "window")]
fn play_offline_smoke(map: &str, output: &str) {
    if let Err(error) = ddnet_rust::window::smoke_offline(map, output) {
        eprintln!("Offline window smoke test failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(feature = "window"))]
fn play_offline_smoke(_map: &str, _output: &str) {
    eprintln!("This command requires --features window");
    std::process::exit(2);
}

fn audio_smoke(output: &str) {
    use std::sync::Arc;
    const RATE: u32 = 48_000;
    const FRAMES: usize = 4_800;
    let signal: Vec<i16> = (0..FRAMES)
        .map(|i| {
            let phase = std::f32::consts::TAU * 440.0 * (i as f32 / RATE as f32);
            (phase.sin() * 8_000.0) as i16
        })
        .collect();
    let sample = Arc::new(Sample::new(signal, 1, RATE, 0)
        .expect("fixed 440 Hz diagnostic sample"));
    let mut mixer = AudioMixer::new(RATE).expect("valid 48 kHz mixer");
    mixer.play(sample, VoiceOptions::default()).expect("one diagnostic voice");
    let stereo = mixer.mix_frames(FRAMES).expect("bounded 100 ms PCM block");
    let wav = mixer.wav_bytes(&stereo).expect("bounded RIFF WAV serialization");
    std::fs::write(output, wav).unwrap_or_else(|e| {
        eprintln!("Unable to write audio test {output}: {e}");
        std::process::exit(1);
    });
    println!("Wrote synthetic 440 Hz PCM WAV to {output} (not real DDNet audio assets)");
}

fn audio_wv_preview(path: &str, output: &str) {
    use std::sync::Arc;
    const RATE: u32 = 48_000;
    let bytes = std::fs::read(path).unwrap_or_else(|error| {
        eprintln!("Unable to open DDNet WavPack {path}: {error}");
        std::process::exit(1);
    });
    let sample = decode_wavpack(&bytes, RATE).unwrap_or_else(|error| {
        eprintln!("Failed to decode DDNet WavPack {path}: {error:?}");
        std::process::exit(1);
    });
    let frames = sample.frames();
    let channels = sample.channels();
    let mut mixer = AudioMixer::new(RATE).expect("valid output rate");
    mixer.play(Arc::new(sample), VoiceOptions::default())
        .expect("valid one-shot decoded voice");
    let mut mixed = Vec::with_capacity(frames * 2);
    for chunk in (0..frames).step_by(MAX_BLOCK_FRAMES) {
        let size = (frames - chunk).min(MAX_BLOCK_FRAMES);
        mixed.extend_from_slice(&mixer.mix_frames(size).expect("bounded mix chunk"));
    }
    let wav = mixer.wav_bytes(&mixed).expect("valid WAV export");
    std::fs::write(output, wav).unwrap_or_else(|error| {
        eprintln!("Unable to write {output}: {error}");
        std::process::exit(1);
    });
    println!("Decoded real DDNet .wv: {frames} frames, {channels} channel(s), 48000 Hz -> {output}");
}

fn servers_list(file: Option<&str>, query: Option<&str>) {
    let (servers, source) = if let Some(path) = file {
        let data = std::fs::read(path).unwrap_or_else(|e| {
            eprintln!("Cannot open master list {path}: {e}");
            std::process::exit(1)
        });
        (parse_master(&data).unwrap_or_else(|e| {
            eprintln!("Invalid master JSON {path}: {e:?}");
            std::process::exit(1)
        }), path.to_owned())
    } else {
        let (entries, url) = fetch_master().unwrap_or_else(|e| {
            eprintln!("Official DDNet master unavailable: {e:?}");
            std::process::exit(1)
        });
        (entries, url.to_owned())
    };
    let settings = ClientSettings::load(&ClientSettings::default_path())
        .unwrap_or_default();
    let search = query.unwrap_or(&settings.search);
    let filtered: Vec<_> = servers.iter()
        .filter(|s| matches_filter(s, search, settings.hide_empty))
        .collect();
    println!("DDNet master: {source}; {} compatible 0.6 servers; {} matching",
        servers.len(), filtered.len());
    for s in filtered.into_iter().take(50) {
        println!("{:>2}/{:<2} {:<12} {:<22} {:<47} {}",
            s.players, s.max_players, s.game_type, s.map, s.name, s.endpoint);
    }
}

fn print_client_settings() {
    let path = ClientSettings::default_path();
    let s = ClientSettings::load(&path).unwrap_or_else(|e| {
        eprintln!("Cannot read settings {}: {e:?}", path.display());
        std::process::exit(1)
    });
    println!("Config: {}", path.display());
    println!("Nickname: {}", s.nickname);
    println!("Filter: {}", s.search);
    println!("Hide empty: {}", s.hide_empty);
    println!("Last 0.6 endpoint: {}", s.last_server);
}

fn set_client_name(name: &str) {
    let path = ClientSettings::default_path();
    let mut s = ClientSettings::load(&path).unwrap_or_else(|e| {
        eprintln!("Settings error: {e:?}");
        std::process::exit(1)
    });
    s.nickname = name.to_owned();
    s.save(&path).unwrap_or_else(|e| {
        eprintln!("Invalid nickname or settings path: {e:?}");
        std::process::exit(1)
    });
    println!("Saved nickname: {}", s.nickname);
}

#[cfg(feature = "window")]
fn browse_servers() {
    if let Err(e) = ddnet_rust::server_menu::browse_servers() {
        eprintln!("Server browser failed: {e}");
        std::process::exit(1);
    }
}
#[cfg(not(feature = "window"))]
fn browse_servers() {
    eprintln!("The windowed browser requires cargo run --features window -- --browse-servers");
    std::process::exit(2);
}
