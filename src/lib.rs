#![feature(futures_api, pin, async_await, await_macro, arbitrary_self_types)]
#![feature(generators)]
#![feature(dbg_macro)]
#![feature(nll)]
#![feature(try_from)]
#![feature(optin_builtin_traits)]
#![crate_type = "lib"] 

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};
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

#[derive(Debug)]
struct Inner<T> {
    mutex_resource: Mutex<T>,
    mutex_opt_pending: Mutex<Option<Awakener>>,
}

#[derive(Debug)]
pub struct AsyncMutex<T> {
    arc_inner: Arc<Inner<T>>,
}

pub struct AcquiredAsyncMutex<T> {
    arc_inner: Arc<Inner<T>>,
}

impl<T> AcquiredAsyncMutex<T> {
    fn guard<'a>(self) -> AsyncMutexGuard<'a,T> {
        let inner = &*self.arc_inner;
        let mutex_guard = inner.mutex_resource.lock().unwrap();
        AsyncMutexGuard {
            mutex_guard,
            arc_inner: self.arc_inner.clone(),
        }
    }
}

pub struct AsyncMutexGuard<'a,T> {
    mutex_guard: MutexGuard<'a,T>,
    arc_inner: Arc<Inner<T>>,
}

impl<'a,T> Drop for AsyncMutexGuard<'a,T> {
    fn drop(&mut self) {
        let inner = &*self.arc_inner;
        let mut opt_pending_guard = inner.mutex_opt_pending.lock().unwrap();
        if let Some(mut pending_guard) = (&mut *opt_pending_guard).take() {
            if pending_guard.is_empty() {
                *opt_pending_guard = None;
            } else {
                pending_guard.wakeup_next();
            }
        }
    }
}

impl<'a,T> Deref for AsyncMutexGuard<'a,T> {
    type Target = T;

    fn deref(&self) -> &T {
        &*self.mutex_guard
    }
}

impl<'a,T> DerefMut for AsyncMutexGuard<'a,T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut *self.mutex_guard
    }
}

impl<T> AsyncMutex<T> {
    pub fn new(resource: T) -> AsyncMutex<T> {
        let inner = Inner {
            mutex_resource: Mutex::new(resource),
            mutex_opt_pending: Mutex::new(None),
        };
        AsyncMutex {
            arc_inner: Arc::new(inner),
        }
    }

    fn acquire(&self) -> impl Future<Output=AcquiredAsyncMutex<T>> + '_
    {
        future::lazy(|_| ())
            .then(move |_| self.acquire_inner())
    }

    fn acquire_inner(&self) -> impl Future<Output=AcquiredAsyncMutex<T>> + '_
    {
        let inner = &*self.arc_inner;
        let fut_wait = {
            let mut opt_pending_guard = inner.mutex_opt_pending.lock().unwrap();

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

        fut_wait
            .then(move |_| {
                let acquired_async_mutex = AcquiredAsyncMutex {
                    arc_inner: self.arc_inner.clone(),
                };
                future::ready(acquired_async_mutex)
            })
    }
}


impl<T> Clone for AsyncMutex<T> {
    fn clone(&self) -> AsyncMutex<T> {
        AsyncMutex {
            arc_inner: Arc::clone(&self.arc_inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{FutureExt, SinkExt, StreamExt};
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

        let task1 = async_mutex.acquire().then(|mut num_cell| {
            num_cell.num += 1;
            future::ready(())
        });

        thread_pool.run(task1);

        {
            let _ = async_mutex.acquire().then(|mut num_cell| {
                num_cell.num += 1;
                future::ready(())
            });

            let _ = async_mutex.acquire().then(|mut num_cell| {
                num_cell.num += 1;
                future::ready(())
            });
        }

        let task2 = async_mutex.acquire().then(|mut num_cell| {
            num_cell.num += 1;
            let num = num_cell.num;
            future::ready(num)
        });

        assert_eq!(thread_pool.run(task2), 2);
    }


    async fn inc_task(async_mutex: AsyncMutex<NumCell>, sender: mpsc::Sender<()>) {
        {
            let guard = await!(async_mutex.acquire());
            let resource = &mut *guard;
            resource.num += 1;
        }
        await!(sender.send(())).unwrap();
    }

    #[test]
    fn borrow_multiple() {
        const N: usize = 1_000;
        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let mut thread_pool = ThreadPool::new().unwrap();
        let (sender, mut receiver) = mpsc::channel::<()>(0);

        for _ in 0..N {
            thread_pool.spawn(inc_task(async_mutex.clone(), sender.clone())).unwrap();
        }

        let task = async_mutex.acquire().then(|mut num_cell| {
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

    /*

    #[test]
    fn borrow_nested() {
        let mut thread_pool = ThreadPool::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task = async move {
            await!(async_mutex.acquire_borrow(|num_cell| {
                num_cell.num += 1;

                let mut _nested_task = async_mutex.acquire_borrow(|num_cell| {
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
    */

}
