//! Object-store key layout.
//!
//! A part owns its grid and every tile whose stack it tops, all under keys
//! that start with the layer id and the part id, so dropping a part is one
//! object delete and one prefix delete. A tile rendered from several parts is
//! filed under the top one; when a lower part goes first the tile is never
//! asked for again and is cleaned up with the top part.

use crate::proj::TileCoord;

/// `tiles/{layer}/{top part}/{rest}/{z}/{x}/{y}.wxt`, where `combo` is the
/// stack of part ids the tile is rendered from, top first, and `rest` is
/// [`combo_tag`] of everything under the top part.
///
/// # Panics
///
/// If `combo` is empty.
pub fn tile(layer: &str, combo: &[&str], t: TileCoord) -> String {
    format!(
        "tiles/{layer}/{}/{}/{}/{}/{}.wxt",
        combo[0],
        combo_tag(&combo[1..]),
        t.z,
        t.x,
        t.y
    )
}

/// A short stable name for the parts under the top one: `solo` when there
/// are none, otherwise 16 hex digits of a hash of their ids.
pub fn combo_tag(rest: &[&str]) -> String {
    if rest.is_empty() {
        return "solo".to_string();
    }
    // FNV-1a, 64 bit. Stability across builds and platforms is the point;
    // the ids are ours, so nothing here needs to resist an adversary.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (i, id) in rest.iter().enumerate() {
        if i > 0 {
            h = (h ^ b',' as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
        for b in id.bytes() {
            h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// Prefix of every tile filed under a part, with trailing slash.
pub fn part_tile_prefix(layer: &str, part_id: &str) -> String {
    format!("tiles/{layer}/{part_id}/")
}

/// Prefix of every tile of a layer, with trailing slash.
pub fn layer_tile_prefix(layer: &str) -> String {
    format!("tiles/{layer}/")
}

/// A part's grid, which every tile is rendered from: `grids/{layer}/{part}.wxg`
pub fn grid(layer: &str, part_id: &str) -> String {
    format!("grids/{layer}/{part_id}.wxg")
}

/// Prefix of every stored grid for a layer, with trailing slash.
pub fn layer_grid_prefix(layer: &str) -> String {
    format!("grids/{layer}/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let t = TileCoord::new(5, 7, 12).unwrap();
        assert_eq!(
            tile("radar", &["mrms-2611061142"], t),
            "tiles/radar/mrms-2611061142/solo/5/7/12.wxt"
        );
        let stacked = tile("radar", &["mrms-2611061142", "gfs-26110606-360"], t);
        assert!(stacked.starts_with(&part_tile_prefix("radar", "mrms-2611061142")));
        assert!(part_tile_prefix("radar", "a").starts_with(&layer_tile_prefix("radar")));
        assert_eq!(
            grid("temp", "hrrr-26110611-360"),
            "grids/temp/hrrr-26110611-360.wxg"
        );
        assert!(grid("temp", "x").starts_with(&layer_grid_prefix("temp")));
    }

    #[test]
    fn tags_tell_stacks_apart() {
        assert_eq!(combo_tag(&[]), "solo");
        let a = combo_tag(&["hrrr-26110611-360", "gfs-26110606-660"]);
        assert_eq!(a.len(), 16);
        assert_eq!(a, combo_tag(&["hrrr-26110611-360", "gfs-26110606-660"]));
        assert_ne!(a, combo_tag(&["gfs-26110606-660", "hrrr-26110611-360"]));
        assert_ne!(a, combo_tag(&["hrrr-26110611-360"]));
        // Joining is unambiguous: ids cannot contain the separator.
        assert_ne!(combo_tag(&["ab", "c"]), combo_tag(&["a", "bc"]));
    }
}
