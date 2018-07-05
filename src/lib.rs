extern crate futures;
#[macro_use]
extern crate log;
#[cfg(test)]
extern crate tokio_core;

use std::cell::RefCell;
use std::mem;
use std::rc::Rc;

use futures::prelude::*;
use futures::sync::oneshot;

use std::collections::LinkedList;

#[derive(Debug)]
enum ResourceState<T> {
    Empty,
    Broken,
    Pending(LinkedList<oneshot::Sender<T>>),
    Present(T),
}

#[derive(Debug)]
pub struct Inner<T> {
    resource: ResourceState<T>,
}

impl<T> Inner<T> {
    fn wakeup_next(&mut self, resource: T) {
        if let ResourceState::Pending(mut awakeners) =
            mem::replace(&mut self.resource, ResourceState::Empty)
        {
            let mut bucket = Some(resource);

            while let Some(awakener) = awakeners.pop_front() {
                let resource = bucket
                    .take()
                    .expect("Attempted to take resource after it gone");

                match awakener.send(resource) {
                    Ok(_) => break,
                    Err(resource) => {
                        bucket = Some(resource);
                        continue;
                    }
                }
            }

            self.resource = match bucket {
                Some(t) => ResourceState::Present(t),
                None => ResourceState::Pending(awakeners),
            }
        }
    }
}

#[derive(Debug)]
pub struct AsyncMutex<T> {
    inner: Rc<RefCell<Inner<T>>>,
}

#[derive(Debug)]
pub enum AsyncMutexError<E> {
    AwakenerCanceled,
    ResourceBroken,
    Function(E),
}

impl<E> From<E> for AsyncMutexError<E> {
    fn from(e: E) -> AsyncMutexError<E> {
        AsyncMutexError::Function(e)
    }
}

#[derive(Debug)]
enum AcquireFutureState<T, F, G> {
    NotPolled(F),
    WaitResource((oneshot::Receiver<T>, F)),
    WaitFunction(G),
    Broken,
    Empty,
}

#[derive(Debug)]
#[must_use = "futures do nothing unless polled"]
pub struct AcquireFuture<T, F, G> {
    inner: Rc<RefCell<Inner<T>>>,
    state: AcquireFutureState<T, F, G>,
}

impl<T> AsyncMutex<T> {
    /// Create a new **single threading** shared mutex resource.
    pub fn new(t: T) -> AsyncMutex<T> {
        let inner = Rc::new(RefCell::new(Inner {
            resource: ResourceState::Present(t),
        }));

        AsyncMutex { inner }
    }

    /// Acquire a shared resource (in the same thread) and invoke the function `f` over it.
    ///
    /// The `f` MUST return an `IntoFuture` that resolves to a tuple of the form (res, output),
    /// where `t` is the original `resource`, and `output` is custom output.
    ///
    /// If the acquirer produce get into trouble,he can choose to consume the resource by returning
    /// `(None, e)`, or give back the resource by returning `(Some(res), e)`.
    ///
    /// This function returns a future that resolves to the value given at output.
    pub fn acquire<F, B, E, G, O>(&self, f: F) -> AcquireFuture<T, F, G>
    where
        F: FnOnce(T) -> B,
        G: Future<Item = (T, O), Error = (Option<T>, E)>,
        B: IntoFuture<Item = G::Item, Error = G::Error, Future = G>,
    {
        AcquireFuture {
            inner: Rc::clone(&self.inner),
            state: AcquireFutureState::NotPolled(f),
        }
    }
}

impl<T, F, B, G, E, O> Future for AcquireFuture<T, F, G>
where
    F: FnOnce(T) -> B,
    G: Future<Item = (T, O), Error = (Option<T>, E)>,
    B: IntoFuture<Item = G::Item, Error = G::Error, Future = G>,
{
    type Item = O;
    type Error = AsyncMutexError<E>;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let mut inner = self.inner.borrow_mut();
        loop {
            match mem::replace(&mut self.state, AcquireFutureState::Empty) {
                AcquireFutureState::Empty => unreachable!(),
                AcquireFutureState::NotPolled(f) => {
                    let resource = mem::replace(&mut inner.resource, ResourceState::Empty);
                    match resource {
                        ResourceState::Empty => unreachable!(),
                        ResourceState::Pending(mut awakeners) => {
                            let (awakener, waiter) = oneshot::channel::<T>();
                            awakeners.push_back(awakener);
                            inner.resource = ResourceState::Pending(awakeners);
                            self.state = AcquireFutureState::WaitResource((waiter, f));
                        }
                        ResourceState::Present(t) => {
                            inner.resource = ResourceState::Pending(LinkedList::new());
                            self.state = AcquireFutureState::WaitFunction(f(t).into_future());
                        }
                        ResourceState::Broken => {
                            inner.resource = ResourceState::Broken;
                            self.state = AcquireFutureState::Broken;
                        }
                    }
                }
                AcquireFutureState::Broken => {
                    return Err(AsyncMutexError::ResourceBroken);
                }
                AcquireFutureState::WaitResource((mut waiter, f)) => {
                    if let ResourceState::Broken = inner.resource {
                        return Err(AsyncMutexError::ResourceBroken);
                    }
                    match waiter
                        .poll()
                        .map_err(|_| AsyncMutexError::AwakenerCanceled)?
                    {
                        Async::Ready(t) => {
                            trace!("AcquireFuture::WaitResource -- Ready");

                            self.state = AcquireFutureState::WaitFunction(f(t).into_future());
                        }
                        Async::NotReady => {
                            trace!("AcquireFuture::WaitResource -- NotReady");

                            self.state = AcquireFutureState::WaitResource((waiter, f));
                            return Ok(Async::NotReady);
                        }
                    }
                }
                AcquireFutureState::WaitFunction(mut f) => {
                    match f.poll().map_err(|(resource, acquirer_error)| {
                        if let Some(resource) = resource {
                            inner.wakeup_next(resource);
                        } else {
                            inner.resource = ResourceState::Broken;
                        }
                        acquirer_error
                    })? {
                        Async::NotReady => {
                            trace!("AcquireFuture::WaitFunction -- NotReady");

                            self.state = AcquireFutureState::WaitFunction(f);
                            return Ok(Async::NotReady);
                        }
                        Async::Ready((resource, output)) => {
                            trace!("AcquireFuture::WaitFunction -- Ready");

                            inner.wakeup_next(resource);
                            return Ok(Async::Ready(output));
                        }
                    }
                }
            }
        }
    }
}

impl<T> Clone for AsyncMutex<T> {
    fn clone(&self) -> AsyncMutex<T> {
        AsyncMutex {
            inner: Rc::clone(&self.inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_core::reactor::Core;

    struct NumCell {
        num: usize,
    }

    #[test]
    fn simple() {
        let mut core = Core::new().unwrap();
        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task1 = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
            num_cell.num += 1;
            Ok((num_cell, ()))
        });

        assert_eq!(core.run(task1).unwrap(), ());

        {
            let _ = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
                num_cell.num += 1;
                Ok((num_cell, ()))
            });

            let _ = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
                num_cell.num += 1;
                Ok((num_cell, ()))
            });
        }

        let task2 = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
            num_cell.num += 1;

            let num = num_cell.num;
            Ok((num_cell, num))
        });

        assert_eq!(core.run(task2).unwrap(), 2);
    }

    #[test]
    fn multiple() {
        const N: usize = 1_000;

        let mut core = Core::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        for num in 0..N {
            let task = async_mutex.acquire(move |mut num_cell| -> Result<_, (_, ())> {
                assert_eq!(num_cell.num, num);

                num_cell.num += 1;
                Ok((num_cell, ()))
            });

            assert_eq!(core.run(task).unwrap(), ());
        }

        let task = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
            num_cell.num += 1;
            let num = num_cell.num;
            Ok((num_cell, num))
        });

        assert_eq!(core.run(task).unwrap(), N + 1);
    }

    #[test]
    fn nested() {
        let mut core = Core::new().unwrap();
        let handle = core.handle();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task = async_mutex
            .clone()
            .acquire(move |mut num_cell| -> Result<_, (_, ())> {
                num_cell.num += 1;

                let nested_task = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
                    assert_eq!(num_cell.num, 1);
                    num_cell.num += 1;
                    Ok((num_cell, ()))
                });
                handle.spawn(nested_task.map_err(|_| ()));

                let num = num_cell.num;
                Ok((num_cell, num))
            });

        assert_eq!(core.run(task).unwrap(), 1);
    }

    #[test]
    fn error() {
        let mut core = Core::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task1 =
            async_mutex.acquire(|num_cell| -> Result<(_, ()), _> { Err((Some(num_cell), ())) });

        let task2 = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
            num_cell.num += 1;
            let num = num_cell.num;
            Ok((num_cell, num))
        });

        let task3 = async_mutex.acquire(|_| -> Result<(_, ()), _> { Err((None, ())) });

        let task4 = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
            num_cell.num += 1;
            Ok((num_cell, ()))
        });

        assert!(core.run(task1).is_err());

        assert_eq!(core.run(task2).unwrap(), 1);

        assert!(core.run(task3).is_err());
        assert!(core.run(task4).is_err());
    }

    #[test]
    fn deadlock() {
        let mut core = Core::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task0 = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
            num_cell.num += 1;
            Ok((num_cell, ()))
        });

        let task1 = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
            num_cell.num += 1;
            Ok((num_cell, ()))
        });

        let task2 = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
            num_cell.num += 1;
            Ok((num_cell, ()))
        });

        core.run(task0).unwrap();
        core.run(task2).unwrap();
        core.run(task1).unwrap();
    }
}
