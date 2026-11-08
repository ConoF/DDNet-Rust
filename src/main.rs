use ddnet_rust::datafile::Datafile;
use ddnet_rust::map::GameMap;
use ddnet_rust::layers::PhysicsLayers;
use ddnet_rust::physics::{Character, Input, JumpEvent, Tuning};
use ddnet_rust::protocol::{decode_int, write_int};
use ddnet_rust::transport::probe_server;
use ddnet_rust::session::{GameAction, GameSession06, Stage};
use ddnet_rust::huffman::{reference_inputs, Huffman};

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
        [arg, path] if arg == "--inspect-layers" => inspect_layers(path),
        [arg, addr] if arg == "--probe-server" => probe(addr),
        [arg, addr] if arg == "--connect-headless" => connect_headless(addr),
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
    println!("  --probe-server HOST[:PORT]  Probe a live DDNet 0.6 UDP handshake");
    println!("  --connect-headless HOST[:PORT]  Test DDNet 0.6 login and map download");
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

fn connect_headless(endpoint: &str) {
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
                        GameAction::EnterGameSent => {
                            println!("ENTERGAME transmitted. Snapshot/rendering not implemented yet.");
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
