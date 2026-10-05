use ::std::fmt;

/// A size in bytes as `2.0K`, `512.3M`, `1,862.6G`: one decimal over its unit, the whole part
/// with thousands separated by commas as [`super::DisplayCount`] separates a count's. The
/// largest unit is G, so a volume of a few terabytes shows as thousands of gigabytes, and
/// without the commas `1862.6G` reads as `186.26G` at a glance.
pub struct DisplaySize(pub f64);

impl fmt::Display for DisplaySize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&scaled(self.0, 1))
    }
}

/// [`DisplaySize`] without the decimal: `2K`, `512M`, `1,863G`.
pub struct DisplaySizeRounded(pub f64);

impl fmt::Display for DisplaySizeRounded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&scaled(self.0, 0))
    }
}

/// `bytes` over its unit with `decimals` places, the whole part grouped in thousands.
fn scaled(bytes: f64, decimals: usize) -> String {
    let (value, unit) = if bytes > 999_999_999.0 {
        (bytes / 1_073_741_824.0, "G") // 1024 * 1024 * 1024
    } else if bytes > 999_999.0 {
        (bytes / 1_048_576.0, "M") // 1024 * 1024
    } else if bytes > 999.0 {
        (bytes / 1024.0, "K")
    } else {
        // Bytes are shown as they are, as they always were: `512`, not `512.0`.
        return group_thousands(&bytes.to_string());
    };
    format!("{}{unit}", group_thousands(&format!("{value:.decimals$}")))
}

/// Commas every three digits of the whole part of `number`, a decimal part left as it is.
fn group_thousands(number: &str) -> String {
    let (whole, rest) = number
        .find('.')
        .map_or((number, ""), |dot| number.split_at(dot));
    let mut grouped = String::with_capacity(number.len() + whole.len() / 3);
    for (index, digit) in whole.chars().enumerate() {
        if index > 0 && (whole.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped.push_str(rest);
    grouped
}
