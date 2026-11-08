use ddnet_rust::datafile::Datafile;
use ddnet_rust::physics::{Character, Input, JumpEvent, Tuning};
use ddnet_rust::protocol::{decode_int, write_int};

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
