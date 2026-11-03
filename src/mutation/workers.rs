//! A pool of worker threads that stops at the first error.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;

/// Runs `work` for each index on `workers` threads. `work` returns `None` when no
/// new index can start. After an error no new index starts, because the error can
/// be a full disk or a broken copy, and the other indexes then only cost time.
pub fn run_workers<T: Send>(
    count: usize,
    workers: usize,
    work: impl Fn(usize) -> io::Result<Option<T>> + Sync,
) -> io::Result<Vec<(usize, T)>> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, T)>> = Mutex::new(Vec::new());
    let first_error: Mutex<Option<io::Error>> = Mutex::new(None);
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    if index >= count {
                        return;
                    }
                    match work(index) {
                        Ok(Some(row)) => results
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push((index, row)),
                        Ok(None) => {
                            next.store(count, Ordering::SeqCst);
                            return;
                        }
                        Err(err) => {
                            next.store(count, Ordering::SeqCst);
                            first_error
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .get_or_insert(err);
                            return;
                        }
                    }
                }
            });
        }
    });
    if let Some(err) = first_error
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
    {
        return Err(err);
    }
    Ok(results.into_inner().unwrap_or_else(PoisonError::into_inner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workers_start_no_new_job_after_an_error() {
        let calls = AtomicUsize::new(0);
        let result: io::Result<Vec<(usize, ())>> = run_workers(1000, 2, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::other("broken"))
        });
        assert_eq!(
            result.map_err(|err| err.to_string()),
            Err("broken".to_owned())
        );
        assert!(calls.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn workers_stop_at_none_and_keep_the_results() {
        let Ok(results) = run_workers(5, 1, |index| Ok((index < 2).then_some(index))) else {
            panic!("no error");
        };
        assert_eq!(results, [(0, 0), (1, 1)]);
    }
}
