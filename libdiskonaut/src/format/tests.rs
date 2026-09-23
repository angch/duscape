use super::{DisplayCount, DisplaySize, truncate_middle};

#[test]
fn truncate_middle_char_boundary() {
    assert_eq!(
        truncate_middle("굿걸 - 누가 방송국을 털었나 E06.mp4", 44),
        "굿걸 - 누가 방송국을[...]국을 털었나 E06.mp4",
    );
}

#[test]
fn display_count_separates_thousands() {
    let cases = [
        (0, "0"),
        (7, "7"),
        (999, "999"),
        (1_000, "1,000"),
        (12_345, "12,345"),
        (123_456, "123,456"),
        (1_234_567, "1,234,567"),
        (11_341_063, "11,341,063"),
        (u64::MAX, "18,446,744,073,709,551,615"),
    ];
    for (count, expected) in cases {
        assert_eq!(DisplayCount(count).to_string(), expected);
    }
}

#[test]
fn display_count_honours_width() {
    assert_eq!(format!("{:>7}", DisplayCount(1_234)), "  1,234");
}

#[test]
fn display_size_formats_kilobytes() {
    assert_eq!(format!("{}", DisplaySize(2048.0)), "2.0K");
}
