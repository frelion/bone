use super::*;

#[test]
fn budget_reservations_do_not_overrun() {
    let mut budget = Budget::new(1, 1);
    budget.reserve_call().unwrap();
    assert!(budget.reserve_call().is_err());
    budget.reserve_job().unwrap();
    assert!(budget.reserve_job().is_err());
    assert_eq!(budget.calls_used, 1);
    assert_eq!(budget.jobs_used, 1);
}
