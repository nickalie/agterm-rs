//! The hop onto GPUI's main thread: ordering, delays on GPUI's (test) clock, and shutdown.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;

use gpui_kit::{AppContext as _, TestAppContext};

use super::*;

type Seen = Vec<(u32, bool)>;

/// A loop over `cx`'s executor with its consumer running on `state`; `after` is counted in `landed`.
fn driven<S: 'static>(cx: &mut TestAppContext, state: S) -> (Arc<GpuiMainLoop<S>>, Entity<S>, Rc<Cell<u32>>) {
    let main_loop = Arc::new(GpuiMainLoop::new(cx.executor()));
    let state = cx.new(|_| state);
    let landed = Rc::new(Cell::new(0));
    let counted = Rc::clone(&landed);
    let tasks = main_loop.tasks();
    let driving = state.clone();
    cx.update(|cx| drive(tasks, driving, move |_, _| counted.set(counted.get() + 1), cx).detach());
    (main_loop, state, landed)
}

#[gpui_kit::test]
fn tasks_posted_from_other_threads_run_in_order_on_the_main_thread(cx: &mut TestAppContext) {
    let (main_loop, state, landed) = driven::<Seen>(cx, Vec::new());
    let owner = thread::current().id();
    let record = move |n: u32| -> MainTask<Seen> {
        Box::new(move |seen: &mut Seen| seen.push((n, thread::current().id() == owner)))
    };
    let posted = thread::scope(|scope| {
        scope
            .spawn(|| (0..20).try_for_each(|n| main_loop.post(record(n))))
            .join()
    });
    assert_eq!(posted.ok(), Some(Ok(())));
    cx.run_until_parked();
    let expected: Seen = (0..20).map(|n| (n, true)).collect();
    state.read_with(cx, |seen, _| assert_eq!(*seen, expected));
    assert_eq!(landed.get(), 20, "the frontend's work lands after every task");
}

#[gpui_kit::test]
fn the_landing_runs_with_the_state_released(cx: &mut TestAppContext) {
    let main_loop = Arc::new(GpuiMainLoop::new(cx.executor()));
    let state = cx.new(|_| 0_u32);
    let read = Rc::new(Cell::new(None));
    let reading = Rc::clone(&read);
    let tasks = main_loop.tasks();
    let driving = state.clone();
    cx.update(|cx| {
        drive(tasks, driving, move |state, cx| reading.set(Some(*state.read(cx))), cx).detach();
    });
    main_loop.post(Box::new(|count: &mut u32| *count += 41)).unwrap();
    main_loop.post(Box::new(|count: &mut u32| *count += 1)).unwrap();
    cx.run_until_parked();
    assert_eq!(read.get(), Some(42));
}

#[gpui_kit::test]
fn a_delayed_task_runs_after_its_delay_and_after_the_immediate_ones(cx: &mut TestAppContext) {
    let (main_loop, state, _) = driven::<Vec<&str>>(cx, Vec::new());
    main_loop
        .post_after(
            Duration::from_millis(40),
            Box::new(|seen: &mut Vec<&str>| seen.push("late")),
        )
        .unwrap();
    main_loop
        .post(Box::new(|seen: &mut Vec<&str>| seen.push("now")))
        .unwrap();
    cx.run_until_parked();
    state.read_with(cx, |seen, _| assert_eq!(*seen, ["now"]));
    cx.executor().advance_clock(Duration::from_millis(39));
    cx.run_until_parked();
    state.read_with(cx, |seen, _| assert_eq!(*seen, ["now"], "not before its delay"));
    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    state.read_with(cx, |seen, _| assert_eq!(*seen, ["now", "late"]));
}

#[gpui_kit::test]
fn delayed_tasks_keep_their_deadline_order(cx: &mut TestAppContext) {
    let (main_loop, state, _) = driven::<Vec<u32>>(cx, Vec::new());
    for (n, delay) in [(3, 30), (1, 10), (2, 20)] {
        main_loop
            .post_after(
                Duration::from_millis(delay),
                Box::new(move |seen: &mut Vec<u32>| seen.push(n)),
            )
            .unwrap();
    }
    cx.executor().advance_clock(Duration::from_millis(30));
    cx.run_until_parked();
    state.read_with(cx, |seen, _| assert_eq!(*seen, [1, 2, 3]));
}

#[gpui_kit::test]
fn close_refuses_new_tasks_and_drops_the_queued_and_delayed_ones_at_once(cx: &mut TestAppContext) {
    let (main_loop, state, _) = driven::<u32>(cx, 0);
    let (queued_tx, queued_rx) = mpsc::channel::<()>();
    let (delayed_tx, delayed_rx) = mpsc::channel::<()>();
    main_loop
        .post(Box::new(move |count: &mut u32| {
            *count += 1;
            queued_tx.send(()).unwrap();
        }))
        .unwrap();
    main_loop
        .post_after(
            Duration::from_secs(3600),
            Box::new(move |count: &mut u32| {
                *count += 1;
                delayed_tx.send(()).unwrap();
            }),
        )
        .unwrap();
    main_loop.close();
    // both waiters see the loop closed now, not at the delayed task's deadline
    assert_eq!(queued_rx.try_recv(), Err(mpsc::TryRecvError::Disconnected));
    assert_eq!(delayed_rx.try_recv(), Err(mpsc::TryRecvError::Disconnected));
    assert_eq!(
        main_loop.post(Box::new(|count: &mut u32| *count += 1)),
        Err(MainLoopClosed)
    );
    assert_eq!(
        main_loop.post_after(Duration::ZERO, Box::new(|count: &mut u32| *count += 1)),
        Err(MainLoopClosed)
    );
    cx.executor().advance_clock(Duration::from_secs(3600));
    cx.run_until_parked();
    state.read_with(cx, |count, _| assert_eq!(*count, 0));
}

#[gpui_kit::test]
fn the_consumer_ends_when_the_loop_closes(cx: &mut TestAppContext) {
    let main_loop = Arc::new(GpuiMainLoop::<u32>::new(cx.executor()));
    let state = cx.new(|_| 0_u32);
    let ended = Rc::new(Cell::new(false));
    let marking = Rc::clone(&ended);
    let tasks = main_loop.tasks();
    cx.update(|cx| {
        let driving = drive(tasks, state, |_, _| {}, cx);
        cx.spawn(async move |_| {
            driving.await;
            marking.set(true);
        })
        .detach();
    });
    cx.run_until_parked();
    assert!(!ended.get());
    main_loop.close();
    cx.run_until_parked();
    assert!(ended.get());
}

#[gpui_kit::test]
fn a_task_closing_the_loop_drops_the_ones_queued_behind_it(cx: &mut TestAppContext) {
    let main_loop = Arc::new(GpuiMainLoop::<Vec<&str>>::new(cx.executor()));
    let state = cx.new(|_| Vec::new());
    let tasks = main_loop.tasks();
    let closing = Arc::clone(&main_loop);
    let driving = state.clone();
    cx.update(|cx| {
        drive(
            tasks,
            driving,
            move |state, cx| {
                if state.read(cx).contains(&"quit") {
                    closing.close();
                }
            },
            cx,
        )
        .detach();
    });
    main_loop
        .post(Box::new(|seen: &mut Vec<&str>| seen.push("quit")))
        .unwrap();
    main_loop
        .post(Box::new(|seen: &mut Vec<&str>| seen.push("after")))
        .unwrap();
    cx.run_until_parked();
    state.read_with(cx, |seen, _| assert_eq!(*seen, ["quit"]));
}
