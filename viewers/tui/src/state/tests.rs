use super::UiEffects;

#[test]
fn loading_indicator_increments_by_three() {
    let mut effects = UiEffects::new();
    assert_eq!(effects.loading_progress_indicator, 0);
    effects.increment_loading_progress_indicator();
    assert_eq!(effects.loading_progress_indicator, 3);
    effects.increment_loading_progress_indicator();
    assert_eq!(effects.loading_progress_indicator, 6);
}
