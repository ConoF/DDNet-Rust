use ddnet_rust::datafile::Datafile;
use ddnet_rust::map::GameMap;
use ddnet_rust::layers::PhysicsLayers;
use ddnet_rust::physics::{Character, Input, JumpEvent, Tuning};
use ddnet_rust::protocol::{decode_int, write_int};
use ddnet_rust::transport::probe_server;

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
        [arg, path] if arg == "--inspect-map" => inspect_map(path),
        [arg, path] if arg == "--inspect-game-map" => inspect_game_map(path),
        [arg, path] if arg == "--inspect-layers" => inspect_layers(path),
        [arg, addr] if arg == "--probe-server" => probe(addr),
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
    println!("  --inspect-map PATH  Parse and validate a v3/v4 DDNet map's metadata");
    println!("  --probe-server HOST[:PORT]  Probe a live DDNet 0.6 UDP handshake");
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
