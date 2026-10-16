// Whether a directory that could be removed must be kept, and why.

use std::time::Duration;

// Claude sessions and projects changed more recently than this are kept: they may be in use,
// or about to be.
pub const MIN_IDLE: Duration = Duration::from_secs(24 * 60 * 60);
// Build outputs larger than this are removed once their project is idle for LARGE_MIN_IDLE.
pub const LARGE: u64 = 5 << 30;
pub const LARGE_MIN_IDLE: Duration = Duration::from_secs(2 * 60 * 60);

pub const NOT_IDLE: &str = "changed < 24h";
pub const NOT_IDLE_LARGE: &str = "changed < 2h";
pub const CANNOT_CHECK: &str = "cannot check";
pub const INTERRUPTED: &str = "interrupted";

// `busy_at_start` comes from a snapshot taken when the section started. `busy_now` takes a
// fresh look and runs last, right before the removal. Any failed check keeps the directory.
pub fn keep_reason<E, F>(
    busy: &'static str,
    busy_at_start: bool,
    recent_change: impl FnOnce() -> Result<Option<&'static str>, E>,
    busy_now: impl FnOnce() -> Result<bool, F>,
) -> Option<&'static str> {
    if busy_at_start {
        return Some(busy);
    }
    match recent_change() {
        Err(_) => return Some(CANNOT_CHECK),
        Ok(Some(reason)) => return Some(reason),
        Ok(None) => {}
    }
    match busy_now() {
        Err(_) => Some(CANNOT_CHECK),
        Ok(true) => Some(busy),
        Ok(false) => None,
    }
}

// Why build outputs of `size` bytes, in a project last changed `age` ago, must be kept.
pub fn recently_changed(age: Duration, size: u64) -> Option<&'static str> {
    if size > LARGE {
        (age < LARGE_MIN_IDLE).then_some(NOT_IDLE_LARGE)
    } else {
        (age < MIN_IDLE).then_some(NOT_IDLE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUSY: &str = "running";
    const HOUR: Duration = Duration::from_secs(60 * 60);
    const GIB: u64 = 1 << 30;

    fn never<T>() -> Result<T, ()> {
        panic!("must not be checked")
    }

    fn idle() -> Result<Option<&'static str>, ()> {
        Ok(None)
    }

    #[test]
    fn keeps_what_was_busy_at_start_without_more_checks() {
        assert_eq!(keep_reason(BUSY, true, never, never), Some(BUSY));
    }

    #[test]
    fn keeps_what_changed_recently_or_cannot_be_checked() {
        assert_eq!(
            keep_reason(BUSY, false, || Ok::<_, ()>(Some(NOT_IDLE)), never),
            Some(NOT_IDLE)
        );
        assert_eq!(
            keep_reason(BUSY, false, || Err(()), never),
            Some(CANNOT_CHECK)
        );
        assert_eq!(
            keep_reason(BUSY, false, idle, || Err(())),
            Some(CANNOT_CHECK)
        );
    }

    #[test]
    fn looks_again_right_before_removing() {
        assert_eq!(
            keep_reason(BUSY, false, idle, || Ok::<_, ()>(true)),
            Some(BUSY)
        );
        assert_eq!(keep_reason(BUSY, false, idle, || Ok::<_, ()>(false)), None);
    }

    #[test]
    fn removes_large_outputs_after_a_shorter_idle_time() {
        assert_eq!(recently_changed(HOUR, GIB), Some(NOT_IDLE));
        assert_eq!(recently_changed(23 * HOUR, GIB), Some(NOT_IDLE));
        assert_eq!(recently_changed(24 * HOUR, GIB), None);

        assert_eq!(recently_changed(HOUR, 6 * GIB), Some(NOT_IDLE_LARGE));
        assert_eq!(recently_changed(2 * HOUR, 6 * GIB), None);

        // Exactly the threshold is not large.
        assert_eq!(recently_changed(3 * HOUR, LARGE), Some(NOT_IDLE));
        assert_eq!(recently_changed(3 * HOUR, LARGE + 1), None);
    }
}
