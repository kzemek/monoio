use scoped_tls::scoped_thread_local;

use crate::driver::Driver;
use crate::scheduler::{LocalScheduler, TaskQueue};
#[cfg(all(target_os = "linux", feature = "iouring"))]
use crate::IoUringDriver;
#[cfg(feature = "legacy")]
use crate::LegacyDriver;

use crate::task::waker_fn::{dummy_waker, set_poll, should_poll};
use crate::task::{new_task, JoinHandle};
use crate::time::driver::Handle as TimeHandle;

#[cfg(any(all(target_os = "linux", feature = "iouring"), feature = "legacy"))]
use crate::time::TimeDriver;

use std::future::Future;

scoped_thread_local!(pub(crate) static CURRENT: Context);

pub(crate) struct Context {
    /// Thread id(not the kernel thread id but a generated unique number)
    #[cfg(feature = "sync")]
    pub(crate) thread_id: usize,

    /// Thread unpark handles
    #[cfg(feature = "sync")]
    pub(crate) unpark_cache:
        std::cell::RefCell<fxhash::FxHashMap<usize, crate::driver::UnparkHandle>>,

    /// Waker sender cache
    #[cfg(feature = "sync")]
    pub(crate) waker_sender_cache:
        std::cell::RefCell<fxhash::FxHashMap<usize, flume::Sender<std::task::Waker>>>,

    /// Owned task set and local run queue
    pub(crate) tasks: TaskQueue,
    /// Time Handle
    pub(crate) time_handle: Option<TimeHandle>,
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Context {
    pub(crate) fn new() -> Self {
        #[cfg(feature = "sync")]
        let thread_id = crate::builder::BUILD_THREAD_ID.with(|id| *id);

        Self {
            #[cfg(feature = "sync")]
            thread_id,
            #[cfg(feature = "sync")]
            unpark_cache: std::cell::RefCell::new(fxhash::FxHashMap::default()),
            #[cfg(feature = "sync")]
            waker_sender_cache: std::cell::RefCell::new(fxhash::FxHashMap::default()),
            tasks: TaskQueue::default(),
            time_handle: None,
        }
    }

    #[allow(unused)]
    #[cfg(feature = "sync")]
    pub(crate) fn unpark_thread(&self, id: usize) {
        use crate::driver::{thread::get_unpark_handle, unpark::Unpark};
        if let Some(handle) = self.unpark_cache.borrow().get(&id) {
            handle.unpark();
            return;
        }

        if let Some(v) = get_unpark_handle(id) {
            // Write back to local cache
            let w = v.clone();
            self.unpark_cache.borrow_mut().insert(id, w);
            v.unpark();
            return;
        }

        debug_assert!(false, "thread to unpark has not been registered");
    }

    #[allow(unused)]
    #[cfg(feature = "sync")]
    pub(crate) fn send_waker(&self, id: usize, w: std::task::Waker) {
        use crate::driver::thread::get_waker_sender;
        if let Some(sender) = self.waker_sender_cache.borrow().get(&id) {
            let _ = sender.send(w);
            return;
        }

        if let Some(s) = get_waker_sender(id) {
            // Write back to local cache
            let _ = s.send(w);
            self.waker_sender_cache.borrow_mut().insert(id, s);
            return;
        }

        debug_assert!(false, "sender has not been registered");
    }
}

/// Monoio runtime
pub struct Runtime<D> {
    pub(crate) driver: D,
    pub(crate) context: Context,
}

impl<D> Runtime<D> {
    /// Block on
    pub fn block_on<F>(&mut self, future: F) -> F::Output
    where
        F: Future,
        D: Driver,
    {
        assert!(
            !CURRENT.is_set(),
            "Can not start a runtime inside a runtime"
        );

        let waker = dummy_waker();
        let cx = &mut std::task::Context::from_waker(&waker);

        self.driver.with(|| {
            CURRENT.set(&self.context, || {
                #[cfg(feature = "sync")]
                let join = unsafe { spawn_without_static(future) };
                #[cfg(not(feature = "sync"))]
                let join = future;

                pin!(join);
                set_poll();
                loop {
                    loop {
                        // Consume all tasks(with max round to prevent io starvation)
                        let mut max_round = self.context.tasks.len() * 2;
                        while let Some(t) = self.context.tasks.pop() {
                            t.run();
                            if max_round == 0 {
                                // maybe there's a looping task
                                break;
                            } else {
                                max_round -= 1;
                            }
                        }

                        // Check main future
                        if should_poll() {
                            // check if ready
                            if let std::task::Poll::Ready(t) = join.as_mut().poll(cx) {
                                return t;
                            }
                        }

                        if self.context.tasks.is_empty() {
                            // No task to execute, we should wait for io blockingly
                            // Hot path
                            break;
                        }

                        // Cold path
                        let _ = self.driver.submit();
                    }

                    // Wait and Process CQ(the error is ignored for not debug mode)
                    #[cfg(not(all(debug_assertions, feature = "debug")))]
                    let _ = self.driver.park();

                    #[cfg(all(debug_assertions, feature = "debug"))]
                    if let Err(e) = self.driver.park() {
                        tracing!("park error: {:?}", e);
                    }
                }
            })
        })
    }
}

/// Fusion Runtime is a wrapper of io_uring driver or legacy driver based runtime.
#[cfg(feature = "legacy")]
pub enum FusionRuntime<#[cfg(all(target_os = "linux", feature = "iouring"))] L, R> {
    /// Uring driver based runtime.
    #[cfg(all(target_os = "linux", feature = "iouring"))]
    Uring(Runtime<L>),
    /// Legacy driver based runtime.
    Legacy(Runtime<R>),
}

/// Fusion Runtime is a wrapper of io_uring driver or legacy driver based runtime.
#[cfg(all(target_os = "linux", feature = "iouring", not(feature = "legacy")))]
pub enum FusionRuntime<L> {
    /// Uring driver based runtime.
    Uring(Runtime<L>),
}

#[cfg(all(target_os = "linux", feature = "iouring", feature = "legacy"))]
impl<L, R> FusionRuntime<L, R>
where
    L: Driver,
    R: Driver,
{
    /// Block on
    pub fn block_on<F>(&mut self, future: F) -> F::Output
    where
        F: Future,
    {
        match self {
            FusionRuntime::Uring(inner) => inner.block_on(future),
            FusionRuntime::Legacy(inner) => inner.block_on(future),
        }
    }
}

#[cfg(all(feature = "legacy", not(all(target_os = "linux", feature = "iouring"))))]
impl<R> FusionRuntime<R>
where
    R: Driver,
{
    /// Block on
    pub fn block_on<F>(&mut self, future: F) -> F::Output
    where
        F: Future,
    {
        match self {
            FusionRuntime::Legacy(inner) => inner.block_on(future),
        }
    }
}

#[cfg(all(not(feature = "legacy"), all(target_os = "linux", feature = "iouring")))]
impl<R> FusionRuntime<R>
where
    R: Driver,
{
    /// Block on
    pub fn block_on<F>(&mut self, future: F) -> F::Output
    where
        F: Future,
    {
        match self {
            FusionRuntime::Uring(inner) => inner.block_on(future),
        }
    }
}

// L -> Fusion<L, R>
#[cfg(all(target_os = "linux", feature = "iouring", feature = "legacy"))]
impl From<Runtime<IoUringDriver>> for FusionRuntime<IoUringDriver, LegacyDriver> {
    fn from(r: Runtime<IoUringDriver>) -> Self {
        Self::Uring(r)
    }
}

// TL -> Fusion<TL, TR>
#[cfg(all(target_os = "linux", feature = "iouring", feature = "legacy"))]
impl From<Runtime<TimeDriver<IoUringDriver>>>
    for FusionRuntime<TimeDriver<IoUringDriver>, TimeDriver<LegacyDriver>>
{
    fn from(r: Runtime<TimeDriver<IoUringDriver>>) -> Self {
        Self::Uring(r)
    }
}

// R -> Fusion<L, R>
#[cfg(all(target_os = "linux", feature = "iouring", feature = "legacy"))]
impl From<Runtime<LegacyDriver>> for FusionRuntime<IoUringDriver, LegacyDriver> {
    fn from(r: Runtime<LegacyDriver>) -> Self {
        Self::Legacy(r)
    }
}

// TR -> Fusion<TL, TR>
#[cfg(all(target_os = "linux", feature = "iouring", feature = "legacy"))]
impl From<Runtime<TimeDriver<LegacyDriver>>>
    for FusionRuntime<TimeDriver<IoUringDriver>, TimeDriver<LegacyDriver>>
{
    fn from(r: Runtime<TimeDriver<LegacyDriver>>) -> Self {
        Self::Legacy(r)
    }
}

// R -> Fusion<R>
#[cfg(all(feature = "legacy", not(all(target_os = "linux", feature = "iouring"))))]
impl From<Runtime<LegacyDriver>> for FusionRuntime<LegacyDriver> {
    fn from(r: Runtime<LegacyDriver>) -> Self {
        Self::Legacy(r)
    }
}

// TR -> Fusion<TR>
#[cfg(all(feature = "legacy", not(all(target_os = "linux", feature = "iouring"))))]
impl From<Runtime<TimeDriver<LegacyDriver>>> for FusionRuntime<TimeDriver<LegacyDriver>> {
    fn from(r: Runtime<TimeDriver<LegacyDriver>>) -> Self {
        Self::Legacy(r)
    }
}

// L -> Fusion<L>
#[cfg(all(target_os = "linux", feature = "iouring", not(feature = "legacy")))]
impl From<Runtime<IoUringDriver>> for FusionRuntime<IoUringDriver> {
    fn from(r: Runtime<IoUringDriver>) -> Self {
        Self::Uring(r)
    }
}

// TL -> Fusion<TL>
#[cfg(all(target_os = "linux", feature = "iouring", not(feature = "legacy")))]
impl From<Runtime<TimeDriver<IoUringDriver>>> for FusionRuntime<TimeDriver<IoUringDriver>> {
    fn from(r: Runtime<TimeDriver<IoUringDriver>>) -> Self {
        Self::Uring(r)
    }
}

/// Spawns a new asynchronous task, returning a [`JoinHandle`] for it.
///
/// Spawning a task enables the task to execute concurrently to other tasks.
/// There is no guarantee that a spawned task will execute to completion. When a
/// runtime is shutdown, all outstanding tasks are dropped, regardless of the
/// lifecycle of that task.
///
///
/// [`JoinHandle`]: monoio::task::JoinHandle
///
/// # Examples
///
/// In this example, a server is started and `spawn` is used to start a new task
/// that processes each received connection.
///
/// ```no_run
/// #[monoio::main]
/// async fn main() {
///     let handle = monoio::spawn(async {
///         println!("hello from a background task");
///     });
///
///     // Let the task complete
///     handle.await;
/// }
/// ```
pub fn spawn<T>(future: T) -> JoinHandle<T::Output>
where
    T: Future + 'static,
    T::Output: 'static,
{
    #[cfg(not(feature = "sync"))]
    let (task, join) = new_task(future, LocalScheduler);
    #[cfg(feature = "sync")]
    let (task, join) = new_task(
        crate::utils::thread_id::get_current_thread_id(),
        future,
        LocalScheduler,
    );

    CURRENT.with(|ctx| {
        ctx.tasks.push(task);
    });
    join
}

#[cfg(feature = "sync")]
unsafe fn spawn_without_static<T>(future: T) -> JoinHandle<T::Output>
where
    T: Future,
{
    use crate::task::new_task_holding;
    let (task, join) = new_task_holding(
        crate::utils::thread_id::get_current_thread_id(),
        future,
        LocalScheduler,
    );

    CURRENT.with(|ctx| {
        ctx.tasks.push(task);
    });
    join
}

#[cfg(test)]
mod tests {
    #[cfg(all(feature = "sync", target_os = "linux", feature = "iouring"))]
    #[test]
    fn across_thread() {
        use crate::driver::IoUringDriver;
        use futures::channel::oneshot;

        let (tx1, rx1) = oneshot::channel::<u8>();
        let (tx2, rx2) = oneshot::channel::<u8>();

        std::thread::spawn(move || {
            let mut rt = crate::RuntimeBuilder::<IoUringDriver>::new()
                .build()
                .unwrap();
            rt.block_on(async move {
                let n = rx1.await.expect("unable to receive rx1");
                assert!(tx2.send(n).is_ok());
            });
        });

        let mut rt = crate::RuntimeBuilder::<IoUringDriver>::new()
            .build()
            .unwrap();
        rt.block_on(async move {
            assert!(tx1.send(24).is_ok());
            assert_eq!(rx2.await.expect("unable to receive rx2"), 24);
        });
    }

    #[cfg(all(target_os = "linux", feature = "iouring"))]
    #[test]
    fn timer() {
        use crate::driver::IoUringDriver;
        let mut rt = crate::RuntimeBuilder::<IoUringDriver>::new()
            .enable_timer()
            .build()
            .unwrap();
        let instant = std::time::Instant::now();
        rt.block_on(async {
            crate::time::sleep(std::time::Duration::from_millis(200)).await;
        });
        let eps = instant.elapsed().subsec_millis();
        assert!((eps as i32 - 200).abs() < 50);
    }
}
