#![feature(futures_api, pin, async_await, await_macro, arbitrary_self_types)]
#![feature(generators)]
#![feature(dbg_macro)]
#![feature(nll)]
#![feature(optin_builtin_traits)]
#![crate_type = "lib"] 

use std::ops::{Deref, DerefMut};
use std::sync::Mutex;
use std::cell::UnsafeCell;
use std::collections::LinkedList;

use futures::channel::oneshot;
use futures::{future, Future, FutureExt};


#[derive(Debug)]
struct Awakener {
    queue: LinkedList<oneshot::Sender<()>>,
}

impl Awakener {
    fn new() -> Awakener {
        Awakener {
            queue: LinkedList::new(),
        }
    }

    /// Try to send the resource.
    /// Return `None` if succeed.
    /// Return `Some(resource)` if failed.
    fn wakeup_next(&mut self) {
        while let Some(sender) = self.queue.pop_front() {
            if let Ok(_) = sender.send(()) {
                break;
            }
        }
    }

    /// Make a pair of `(sender, receiver)`.
    /// `sender` is pushed to `self.queue`.
    /// Return `receiver`.
    fn add_awakener(&mut self) -> oneshot::Receiver<()> {
        let (sender, receiver) = oneshot::channel();
        self.queue.push_back(sender);
        receiver
    }

    fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

/// An asynchronous Mutex, used for synchronization in the context of Futures.
/// A usual Mutex (from std) is not a good choice for synchronization between Futures, 
/// because it may block your future, and it will not be able to yield execution to another future.
///
/// AsyncMutex yields execution to a different future if the resource is not ready.
/// When the resource is ready, the future waiting for the resource will be woken up.
///
/// Example:
/// ```
/// #![feature(futures_api, pin, async_await, await_macro, arbitrary_self_types)]
/// #![feature(generators)]
/// use async_mutex::AsyncMutex;
/// use std::sync::Arc;
/// use futures::executor::ThreadPool;
/// use futures::task::SpawnExt;
/// use futures::{future, SinkExt, StreamExt};
/// use futures::channel::mpsc;
///
/// struct NumCell {
///     x: u32,
/// }
///
/// impl NumCell {
///     async fn add(&mut self) {
///         await!(future::lazy(|_| {
///             self.x += 1
///         }));
///     }
/// }
///
/// async fn add_task(arc_mut_num_cell: Arc<AsyncMutex<NumCell>>, mut sender: mpsc::Sender<()>) {
///     let mut guard = await!(arc_mut_num_cell.lock());
///     await!(guard.add());
///     await!(sender.send(())).unwrap();
/// }
///
/// fn main() {
///     let (sender, mut receiver) = mpsc::channel::<()>(0);
///     let arc_mut_num_cell = Arc::new(AsyncMutex::new(NumCell {x: 0}));
///     let mut thread_pool = ThreadPool::new().unwrap();
///     for _ in 0 .. 100usize {
///         thread_pool.spawn(add_task(arc_mut_num_cell.clone(), sender.clone()));
///     }
///     thread_pool.run(async move {
///         for _ in 0 .. 100usize {
///             await!(receiver.next()).unwrap();
///         }
///         let mut guard = await!(arc_mut_num_cell.lock());
///         assert_eq!(guard.x, 100);
///     });
/// }
///
/// ```
#[derive(Debug)]
pub struct AsyncMutex<T> {
    cell_resource: UnsafeCell<T>,
    mutex_opt_pending: Mutex<Option<Awakener>>,
}

pub struct AsyncMutexGuard<'a,T> {
    async_mutex: &'a AsyncMutex<T>,
}

impl<'a,T> Drop for AsyncMutexGuard<'a,T> {
    fn drop(&mut self) {
        let mut opt_pending_guard = self.async_mutex.mutex_opt_pending.lock().unwrap();
        if let Some(mut pending_guard) = (&mut *opt_pending_guard).take() {
            if pending_guard.is_empty() {
            } else {
                pending_guard.wakeup_next();
                *opt_pending_guard = Some(pending_guard);
            }
        } else {
            unreachable!();
        }
    }
}

impl<'a,T> Deref for AsyncMutexGuard<'a,T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.async_mutex.cell_resource.get() }
    }
}

impl<'a,T> DerefMut for AsyncMutexGuard<'a,T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.async_mutex.cell_resource.get() }
    }
}

impl<T> AsyncMutex<T> {
    pub fn new(resource: T) -> AsyncMutex<T> {
        AsyncMutex {
            cell_resource: UnsafeCell::new(resource),
            mutex_opt_pending: Mutex::new(None),
        }
    }

    pub fn lock(&self) -> impl Future<Output=AsyncMutexGuard<T>> + '_ {
        let mut opt_pending_guard = self.mutex_opt_pending.lock().unwrap();
        let fut_receiver = {
            if let Some(pending) = &mut *opt_pending_guard {
                // Register for waiting:
                pending.add_awakener()
            } else {
                *opt_pending_guard = Some(Awakener::new());
                let (sender, receiver) = oneshot::channel::<()>();
                sender.send(()).unwrap();
                receiver
            }
        };
        let async_mutex_guard = AsyncMutexGuard {
            async_mutex: self,
        };
        
        fut_receiver.then(move |_| {
            future::ready(async_mutex_guard)
        })

        // If lock() is dropped at this point, async_mutex_guard should be dropped.
        // This ensures that the next waiter will not be starved.
        //
        // TODO: This is not a full solution.
        // This needs to be done more carefully.
        // What happens if async_mutex_guard is dropped before fut_wait? 
        // Maybe we can make sure this never happens.

    }
}

unsafe impl<'a,T> Send for AsyncMutexGuard<'a,T> {}

// TODO: This was done based on Mutex of std.
// Find out what is the deeper meaning of this.
unsafe impl<T: Send> Send for AsyncMutex<T> {}
unsafe impl<T: Send> Sync for AsyncMutex<T> {}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use futures::{future, FutureExt, SinkExt, StreamExt};
    use futures::task::{SpawnExt};
    use futures::executor::ThreadPool;
    use futures::channel::mpsc;

    struct NumCell {
        num: usize,
    }

    #[test]
    fn borrow_simple() {
        let mut thread_pool = ThreadPool::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task1 = async_mutex.lock().then(|mut num_cell| {
            num_cell.num += 1;
            future::ready(())
        });

        thread_pool.run(task1);

        {
            let _ = async_mutex.lock().then(|mut num_cell| {
                num_cell.num += 1;
                future::ready(())
            });

            let _ = async_mutex.lock().then(|mut num_cell| {
                num_cell.num += 1;
                future::ready(())
            });
        }

        let task2 = async_mutex.lock().then(|mut num_cell| {
            num_cell.num += 1;
            let num = num_cell.num;
            future::ready(num)
        });

        assert_eq!(thread_pool.run(task2), 2);
    }


    async fn inc_task(arc_async_mutex: Arc<AsyncMutex<NumCell>>, mut sender: mpsc::Sender<()>) {
        {
            let mut guard = await!(arc_async_mutex.lock());
            let resource = &mut *guard;
            resource.num += 1;
        }
        await!(sender.send(())).unwrap();
    }

    #[test]
    fn borrow_multiple() {
        const N: usize = 1_000;
        let arc_async_mutex = Arc::new(AsyncMutex::new(NumCell { num: 0 }));

        let mut thread_pool = ThreadPool::new().unwrap();
        let (sender, mut receiver) = mpsc::channel::<()>(0);

        for _ in 0 .. N {
            thread_pool.spawn(inc_task(arc_async_mutex.clone(), sender.clone())).unwrap();
        }

        let task = arc_async_mutex.lock().then(|mut num_cell| {
            num_cell.num += 1;
            let num = num_cell.num;
            future::ready(num)
        });

        thread_pool.run(async move {
            for _ in 0 .. N {
                await!(receiver.next()).unwrap();
            }
        });

        assert_eq!(thread_pool.run(task), N + 1);
    }

    #[test]
    fn borrow_nested() {
        let mut thread_pool = ThreadPool::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task = async move {
            await!(async_mutex.lock().then(|mut num_cell| {
                num_cell.num += 1;

                let mut _nested_task = async_mutex.lock().then(|mut num_cell| {
                    assert_eq!(num_cell.num, 1);
                    num_cell.num += 1;
                    future::ready(())
                });

                let num = num_cell.num;
                future::ready(num)
            }))
        };

        assert_eq!(thread_pool.run(task), 1);
    }

    /*
    #[test]
    fn lock_drop_starvation() {
        let mut thread_pool = ThreadPool::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        thread_pool.run(async move {
            println!("guard1");
            let guard1 = await!(async_mutex.lock());
            println!("guard2");
            let guard2 = async_mutex.lock();
            let boxed_guard2 = Box::pinned(guard2);
            boxed_guard2.poll_unpin();
            drop(guard1);
            println!("guard3");
            let mut guard3 = await!(async_mutex.lock());
            guard3.num += 1;
            assert_eq!(guard3.num, 1);
        });
    }
    */

}
