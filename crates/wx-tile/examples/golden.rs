//! Writes the golden tiles in `fixtures/tiles/`, which the web client's
//! decoder is tested against. The sample patterns are simple enough to
//! recompute anywhere:
//!
//! * `pattern_1band.wxt`: 257 x 257, one band,
//!   `v = (col * 7 + row * 13) % 65536`, except no-data (0) where
//!   `(col + row) % 11 == 0`.
//! * `pattern_3band.wxt`: 257 x 257, three bands,
//!   `v = (col * (b + 1) * 31 + row * 17 + b * 1000) % 65536` for band `b`.
//! * `tiny.wxt`: 3 x 2, one band, samples `[0, 65535, 1, 32768, 0, 65534]`.
//!
//! Run from the repository root: `cargo run -p wx-tile --example golden`

use wx_tile::{Tile, encode};

fn main() {
    let n = 257usize;
    let one = (0..n * n)
        .map(|i| {
            let (col, row) = (i % n, i / n);
            if (col + row).is_multiple_of(11) {
                0
            } else {
                ((col * 7 + row * 13) % 65536) as u16
            }
        })
        .collect();
    let three = (0..3usize)
        .map(|b| {
            (0..n * n)
                .map(|i| {
                    let (col, row) = (i % n, i / n);
                    ((col * (b + 1) * 31 + row * 17 + b * 1000) % 65536) as u16
                })
                .collect()
        })
        .collect();
    let out = std::path::Path::new("fixtures/tiles");
    std::fs::create_dir_all(out).unwrap();
    for (name, tile) in [
        ("pattern_1band.wxt", Tile::new(257, 257, vec![one])),
        ("pattern_3band.wxt", Tile::new(257, 257, three)),
        (
            "tiny.wxt",
            Tile::new(3, 2, vec![vec![0, 65535, 1, 32768, 0, 65534]]),
        ),
    ] {
        let bytes = encode(&tile);
        std::fs::write(out.join(name), &bytes).unwrap();
        println!("{name}: {} bytes", bytes.len());
    }
}
