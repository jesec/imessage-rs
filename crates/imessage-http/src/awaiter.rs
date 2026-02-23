/// Result awaiter: exponential backoff polling loop.
///
/// Polls a `get_data` function with exponential backoff (250ms * 1.5^n) until:
/// - `data_ready(data)` returns true (data is satisfactory), OR
/// - `max_wait` is exceeded (returns whatever we have, or None)
use std::time::{Duration, Instant};

/// Poll with exponential backoff until data is ready or timeout.
///
/// - `initial_wait`: first sleep duration (default 250ms)
/// - `multiplier`: backoff multiplier (default 1.5)
/// - `max_wait`: maximum total time (e.g. 60s for sends, 30s for edits)
/// - `get_data`: async function that fetches the current data
/// - `data_ready`: predicate that returns true when data is satisfactory
pub async fn result_awaiter<T, F, Fut, P>(
    initial_wait: Duration,
    multiplier: f64,
    max_wait: Duration,
    mut get_data: F,
    data_ready: P,
) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
    P: Fn(&T) -> bool,
{
    let start = Instant::now();
    let mut wait = initial_wait;
    let mut result: Option<T> = None;

    loop {
        tokio::time::sleep(wait).await;

        if start.elapsed() >= max_wait {
            break;
        }

        result = get_data().await;

        if let Some(ref data) = result
            && data_ready(data)
        {
            return result;
        }

        // Exponential backoff
        wait = Duration::from_millis((wait.as_millis() as f64 * multiplier) as u64);
    }

    result
}

/// Convenience: poll for a message to appear in the DB by GUID.
/// Uses 250ms initial wait, 1.5x multiplier, configurable max wait.
pub async fn await_message<F, Fut, T>(max_wait: Duration, get_data: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    result_awaiter(
        Duration::from_millis(250),
        1.5,
        max_wait,
        get_data,
        |_| true, // any non-None result is satisfactory
    )
    .await
}

/// Convenience: poll until a condition on existing data changes.
/// Used for edit/unsend where we wait for dateEdited to change.
pub async fn await_condition<F, Fut, T, P>(
    max_wait: Duration,
    get_data: F,
    condition: P,
) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
    P: Fn(&T) -> bool,
{
    result_awaiter(
        Duration::from_millis(250),
        1.5,
        max_wait,
        get_data,
        condition,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn awaiter_finds_data_on_third_poll() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();

        let result = await_message(Duration::from_secs(5), || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n >= 2 { Some("found") } else { None }
            }
        })
        .await;

        assert_eq!(result, Some("found"));
        assert!(counter.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn awaiter_returns_none_on_timeout() {
        let result = await_message(Duration::from_millis(500), || async { None::<i32> }).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn condition_awaiter_waits_for_change() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();

        let result = await_condition(
            Duration::from_secs(5),
            || {
                let c = c.clone();
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst);
                    Some(n)
                }
            },
            |n| *n >= 3, // wait until value is >= 3
        )
        .await;

        assert!(result.is_some());
        assert!(result.unwrap() >= 3);
    }
}
