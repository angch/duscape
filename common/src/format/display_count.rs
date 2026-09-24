use ::std::fmt;

/// A count of things — files, entries, errors — with thousands separated by commas, so that
/// 11341063 reads as 11,341,063.
pub struct DisplayCount(pub u64);

impl fmt::Display for DisplayCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.0.to_string();
        let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
        for (index, digit) in digits.chars().enumerate() {
            if index > 0 && (digits.len() - index).is_multiple_of(3) {
                grouped.push(',');
            }
            grouped.push(digit);
        }
        f.pad(&grouped)
    }
}
