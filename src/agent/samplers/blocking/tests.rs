use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

const INTERVAL: Duration = Duration::from_secs(60);

#[test]
fn cadence_and_in_flight_admission_are_independent() {
    for cadence in [Cadence::FromDispatch, Cadence::FromCompletion] {
        let worker = BlockingWorker::new("test", INTERVAL, cadence);
        let start = Instant::now();
        let mut permit = worker.try_acquire_at(start).unwrap();
        permit.outcome = Some(Outcome::Ran);
        assert!(worker.try_acquire_at(start).is_none());
        assert_eq!(recover_lock("test", &worker.schedule).last, Some(start));
        let finishing = start + Duration::from_secs(1);
        permit.finish_at(finishing);
        let last = recover_lock("test", &worker.schedule).last.unwrap();
        match cadence {
            Cadence::FromDispatch => assert_eq!(last, start),
            Cadence::FromCompletion => assert_eq!(last, finishing),
        }
        assert!(worker
            .try_acquire_at(last + (INTERVAL - Duration::from_nanos(1)))
            .is_none());
        // Rejected refreshes do not postpone the next admissible read.
        assert_eq!(recover_lock("test", &worker.schedule).last, Some(last));
        assert!(worker.try_acquire_at(last + INTERVAL).is_some());
    }
}

#[test]
fn completion_cadence_keeps_an_idle_gap_after_an_overrunning_sweep() {
    for cadence in [Cadence::FromDispatch, Cadence::FromCompletion] {
        let worker = BlockingWorker::new("test", INTERVAL, cadence);
        let start = Instant::now();
        let mut permit = worker.try_acquire_at(start).unwrap();
        permit.outcome = Some(Outcome::Ran);
        assert!(worker.try_acquire_at(start + INTERVAL).is_none());
        let finishing = start + Duration::from_secs(90);
        permit.finish_at(finishing);
        match cadence {
            Cadence::FromDispatch => assert!(worker.try_acquire_at(finishing).is_some()),
            Cadence::FromCompletion => {
                assert!(worker.try_acquire_at(finishing).is_none());
                assert!(worker
                    .try_acquire_at(start + Duration::from_secs(149))
                    .is_none());
                assert!(worker
                    .try_acquire_at(start + Duration::from_secs(150))
                    .is_some());
            }
        }
    }
}

#[test]
fn an_unspawned_permit_preserves_the_previous_cadence_anchor() {
    for cadence in [Cadence::FromDispatch, Cadence::FromCompletion] {
        let worker = BlockingWorker::new("test", INTERVAL, cadence);
        let start = Instant::now();
        drop(worker.try_acquire_at(start).unwrap());
        assert!(recover_lock("test", &worker.schedule).last.is_none());
        let mut first = worker.try_acquire_at(start).unwrap();
        first.outcome = Some(Outcome::Ran);
        first.finish_at(start + Duration::from_secs(1));
        let previous = recover_lock("test", &worker.schedule).last;
        let next = start + INTERVAL * 2;
        drop(worker.try_acquire_at(next).unwrap());
        assert_eq!(recover_lock("test", &worker.schedule).last, previous);
        assert!(worker.try_acquire_at(next).is_some());
    }
}

#[test]
fn contended_admission_never_waits_or_consumes_the_first_read() {
    let worker = BlockingWorker::new("test", INTERVAL, Cadence::FromDispatch);
    let schedule = recover_lock("test", &worker.schedule);
    assert!(worker.try_acquire().is_none());
    assert!(schedule.last.is_none());
    drop(schedule);
    assert!(worker.try_acquire().is_some());
}

#[test]
fn concurrent_callers_admit_one_worker() {
    let worker = BlockingWorker::new("test", Duration::ZERO, Cadence::FromDispatch);
    let permits = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| worker.try_acquire()))
            .collect();
        threads
            .into_iter()
            .filter_map(|t| t.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(permits.len(), 1);
    drop(permits);
    assert!(worker.try_acquire().is_some());
}

#[cfg(target_os = "linux")]
#[test]
fn stuck_warning_resets_only_when_a_new_worker_is_admitted() {
    let worker = BlockingWorker::new("test", INTERVAL, Cadence::FromDispatch)
        .with_stuck_warning(INTERVAL * 3);
    let start = Instant::now();
    let permit = worker.try_acquire_at(start).unwrap();
    assert!(worker.try_acquire_at(start + INTERVAL * 3).is_none());
    assert!(!recover_lock("test", &worker.schedule).stuck_warned);
    assert!(worker.try_acquire_at(start + INTERVAL * 4).is_none());
    assert!(recover_lock("test", &worker.schedule).stuck_warned);
    assert!(worker.try_acquire_at(start + INTERVAL * 5).is_none());
    assert!(recover_lock("test", &worker.schedule).stuck_warned);
    drop(permit);
    let _next = worker.try_acquire_at(start + INTERVAL * 5).unwrap();
    assert!(!recover_lock("test", &worker.schedule).stuck_warned);
}

#[tokio::test]
async fn a_blocking_read_does_not_block_the_async_worker_or_overlap() {
    let worker = BlockingWorker::new("test", Duration::ZERO, Cadence::FromDispatch);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task = worker.try_acquire().unwrap().spawn(move || {
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    started_rx.await.unwrap();
    assert!(worker.try_acquire().is_none());
    // This single-thread runtime can still make progress while the read waits.
    tokio::task::yield_now().await;
    release_tx.send(()).unwrap();
    task.await.unwrap();
    assert!(worker.try_acquire().is_some());
}

#[tokio::test]
async fn an_unwinding_read_recovers_state_and_waits_a_full_interval() {
    for cadence in [Cadence::FromDispatch, Cadence::FromCompletion] {
        let worker = BlockingWorker::new("test", INTERVAL, cadence);
        let state = Arc::new(Mutex::new(vec![1]));
        let damaged = state.clone();
        worker
            .try_acquire_at(Instant::now())
            .unwrap()
            .spawn(move || {
                let mut state = recover_lock("test", &damaged);
                state.push(2);
                panic!("injected read panic");
            })
            .await
            .unwrap();
        assert!(state.is_poisoned());
        let last = recover_lock("test", &worker.schedule).last.unwrap();
        assert!(recover_lock("test", &worker.schedule)
            .running_since
            .is_none());
        assert!(worker.try_acquire().is_none());
        assert!(worker
            .try_acquire_at(last + (INTERVAL - Duration::from_nanos(1)))
            .is_none());
        let recovered = state.clone();
        worker
            .try_acquire_at(last + INTERVAL)
            .unwrap()
            .spawn(move || recover_lock("test", &recovered).push(3))
            .await
            .unwrap();
        assert!(!state.is_poisoned());
        assert_eq!(*recover_lock("test", &state), vec![1, 2, 3]);
    }
}

#[test]
fn cancelling_a_queued_task_releases_its_permit() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    for cadence in [Cadence::FromDispatch, Cadence::FromCompletion] {
        runtime.block_on(async {
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            });
            started_rx.await.unwrap();
            let worker = BlockingWorker::new("test", INTERVAL, cadence);
            let ran = Arc::new(AtomicBool::new(false));
            let check = ran.clone();
            let queued = worker
                .try_acquire()
                .unwrap()
                .spawn(move || check.store(true, Ordering::Release));
            assert!(worker.try_acquire().is_none());
            queued.abort();
            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            assert!(queued.await.unwrap_err().is_cancelled());
            assert!(!ran.load(Ordering::Acquire));
            assert!(recover_lock("test", &worker.schedule).last.is_none());
            assert!(worker.try_acquire().is_some());
        });
    }
}
