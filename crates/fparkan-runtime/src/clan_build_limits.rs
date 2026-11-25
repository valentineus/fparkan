pub(super) fn maximum_variable(class: u32) -> Option<&'static str> {
    match class {
        0x0100_2000 => Some("dMaxTransport"),
        0x0100_4000 => Some("dMaxBuilder"),
        0x8000_0004 => Some("dMaxMine"),
        0x8000_0008 => Some("dMaxStorage"),
        0x8000_0010 => Some("dMaxPlant"),
        0x8010_0000 | 0x8020_0000 => Some("dMaxTower"),
        _ => None,
    }
}

pub(super) fn at_limit(class: u32, owned: u32, maximum: Option<u32>, pending: usize) -> bool {
    let full = maximum.is_some_and(|maximum| owned >= maximum);
    match class {
        0x0100_2000 | 0x0100_4000 => full,
        0x8000_0004 | 0x8000_0008 | 0x8000_0010 | 0x8020_0000 => full || pending >= 1,
        0x8010_0000 => full || pending >= 3,
        0x8000_0040 | 0x8000_0400 => pending >= 1,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_class_limits_distinguish_owned_and_pending_buildings() {
        for (class, owned, maximum, pending, expected) in [
            (0x0100_4000, 0, 2, 9, false),
            (0x0100_4000, 2, 2, 0, true),
            (0x0100_2000, 1, 2, 9, false),
            (0x0100_2000, 2, 2, 0, true),
            (0x8000_0004, 0, 4, 0, false),
            (0x8000_0004, 0, 4, 1, true),
            (0x8000_0004, 2, 2, 0, true),
            (0x8000_0008, 0, 4, 1, true),
            (0x8000_0010, 0, 4, 1, true),
            (0x8000_0040, 2, 0, 0, false),
            (0x8000_0040, 0, 0, 1, true),
            (0x8000_0400, 2, 0, 0, false),
            (0x8000_0400, 0, 0, 1, true),
            (0x8010_0000, 0, 9, 2, false),
            (0x8010_0000, 0, 9, 3, true),
            (0x8010_0000, 2, 2, 0, true),
            (0x8020_0000, 0, 9, 0, false),
            (0x8020_0000, 0, 9, 1, true),
            (0x8000_0002, 2, 0, 9, false),
        ] {
            assert_eq!(
                at_limit(class, owned, Some(maximum), pending),
                expected,
                "class={class:#x} owned={owned} max={maximum} pending={pending}"
            );
        }
        assert_eq!(maximum_variable(0x8000_0040), None);
        assert_eq!(maximum_variable(0x8000_0400), None);
    }
}
