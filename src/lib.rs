extern crate futures;
#[cfg(test)]
extern crate tokio;

use std::cell::RefCell;
use std::rc::Rc;
use std::marker::PhantomData;

use futures::prelude::*;
use futures::sync::oneshot;

use std::collections::LinkedList;

#[derive(Debug)]
struct Awakener<T> {
    queue: LinkedList<oneshot::Sender<T>>,
}

impl<T> Awakener<T> {
    fn new() -> Awakener<T> {
        Awakener {
            queue: LinkedList::new(),
        }
    }

    /// Try to send the resource.
    /// Return `None` if succeed.
    /// Return `Some(resource)` if failed.
    fn wakeup_next(&mut self, mut resource: T) -> Option<T> {
        while let Some(sender) = self.queue.pop_front() {
            resource = match sender.send(resource) {
                Ok(_) => return None,
                Err(resource) => resource,
            }
        }
        Some(resource)
    }

    /// Make a pair of `(sender, receiver)`.
    /// `sender` is pushed to `self.queue`.
    /// Return `receiver`.
    fn add_awakener(&mut self) -> oneshot::Receiver<T> {
        let (sender, receiver) = oneshot::channel();
        self.queue.push_back(sender);
        receiver
    }
}

#[derive(Debug)]
enum Inner<T> {
    Empty,
    Broken,
    Present(T),
    Pending(Awakener<T>),
}

#[derive(Debug)]
pub struct AsyncMutex<T> {
    inner: Rc<RefCell<Inner<T>>>,
}

impl<T> AsyncMutex<T> {
    pub fn new(resource: T) -> AsyncMutex<T> {
        AsyncMutex {
            inner: Rc::new(RefCell::new(Inner::Present(resource))),
        }
    }

    pub fn acquire_borrow<F, B, O, E>(&self, f: F) -> impl Future<Item = O, Error = AsyncMutexError<E>>
    where
        F: FnOnce(&mut T) -> B,
        B: IntoFuture<Item = O, Error = E>,
    {
        let inner = Rc::clone(&self.inner);
        WaitPoll {
            inner: Rc::clone(&self.inner),
            marker: Default::default(),
        }.and_then(|receiver| {
            // Wait until we receive the resource via `receiver`
            receiver.map_err(|_| AsyncMutexError::AwakenerCanceled)
        }).and_then(move |mut t| {
            // The resource is received.
            let result = f(&mut t);
            wakeup_next(inner, t);
            result.into_future().map_err(AsyncMutexError::Function)
        })
    }

    pub fn acquire<F, B, O, E>(&self, f: F) -> impl Future<Item = O, Error = AsyncMutexError<E>>
    where
        F: FnOnce(T) -> B,
        B: IntoFuture<Item = (T, O), Error = (Option<T>, E)>,
    {
        let ok_inner = Rc::clone(&self.inner);
        let err_inner = Rc::clone(&self.inner);
        WaitPoll {
            inner: Rc::clone(&self.inner),
            marker: Default::default(),
        }.and_then(|receiver| {
            // Wait until we receive the resource via `receiver`
            receiver.map_err(|_| AsyncMutexError::AwakenerCanceled)
        }).and_then(move |t| {
            // The resource is received.
            f(t).into_future().map(move |(resource, output)| {
                wakeup_next(ok_inner, resource);
                output
            }).map_err(move |(resource, error)| {
                match resource {
                    Some(r) => wakeup_next(err_inner, r),
                    None => {
                        err_inner.replace(Inner::Broken);
                    }
                }
                AsyncMutexError::Function(error)
            })
        })
    }
}

/// Call Awakener::wakeup_next(), and set the state accordingly
fn wakeup_next<T>(inner: Rc<RefCell<Inner<T>>>, t: T) {
    let state = inner.replace(Inner::Empty);
    if let Inner::Pending(mut awakener) = state {
        let next_state = match awakener.wakeup_next(t) {
            Some(t) => Inner::Present(t),
            None => Inner::Pending(awakener),
        };
        inner.replace(next_state);
    } else {
        // We are holding the resource in `t`,
        // so it's impossible that the resource is also in `Inner::Present`
        //
        // T as a generic type is not clone-able nor copy-able,
        // so this cannot happen.
        unreachable!()
    }
}

/// This struct is a future.
/// It resolves immediately to `(inner, f, receiver)`,
/// where `receiver` is a `oneshot::Receiver<T>`.
///
/// This struct is introduced so that `AsyncMutex::acquire_borrow`
/// will so nothing unless polled.
#[derive(Debug)]
struct WaitPoll<T, E> {
    inner: Rc<RefCell<Inner<T>>>,
    marker: PhantomData<E>,
}

impl<T, E> Future for WaitPoll<T, E> {
    type Item = oneshot::Receiver<T>;
    type Error = AsyncMutexError<E>;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let inner = self.inner.replace(Inner::Empty);

        let receiver = match inner {
            Inner::Pending(mut awakener) => {
                // Already in pending state, we just need to add a new awakener.
                let receiver = awakener.add_awakener();
                self.inner.replace(Inner::Pending(awakener));
                receiver
            }
            Inner::Present(t) => {
                // Resource is available,
                // add a new awakener and then wake up it at once.
                let mut awakener = Awakener::new();
                let receiver = awakener.add_awakener();
                match awakener.wakeup_next(t) {
                    Some(t) => {
                        self.inner.replace(Inner::Present(t));
                        return Err(AsyncMutexError::AwakenerCanceled);
                    }
                    None => {
                        self.inner.replace(Inner::Pending(awakener));
                        receiver
                    }
                }
            }
            Inner::Broken => return Err(AsyncMutexError::ResourceBroken),
            Inner::Empty => unreachable!(),
        };

        Ok(Async::Ready(receiver))
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AsyncMutexError<E> {
    ResourceBroken,
    AwakenerCanceled,
    Function(E),
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
    use super::AsyncMutex;
    use tokio::prelude::*;
    use tokio::runtime::current_thread::Runtime;

    struct NumCell {
        num: usize,
    }

    #[test]
    fn simple() {
        let mut runtime = Runtime::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task1 = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
            num_cell.num += 1;
            Ok((num_cell, ()))
        });

        assert_eq!(runtime.block_on(task1).unwrap(), ());

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

        assert_eq!(runtime.block_on(task2).unwrap(), 2);
    }

    #[test]
    fn multiple() {
        const N: usize = 1_000;

        let mut runtime = Runtime::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        for _num in 0..N {
            let task = async_mutex.acquire(move |mut num_cell| -> Result<_, (_, ())> {
                num_cell.num += 1;
                Ok((num_cell, ()))
            });
            runtime.spawn(task.map_err(|_| ()));
        }
        runtime.run().unwrap();

        let task = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
            num_cell.num += 1;
            let num = num_cell.num;
            Ok((num_cell, num))
        });

        assert_eq!(runtime.block_on(task).unwrap(), N + 1);
    }

    #[test]
    fn nested() {
        let mut runtime = Runtime::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task = async_mutex
            .clone()
            .acquire(move |mut num_cell| -> Result<_, (_, ())> {
                num_cell.num += 1;

                let mut nested_task = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
                    assert_eq!(num_cell.num, 1);
                    num_cell.num += 1;
                    Ok((num_cell, ()))
                });
                assert_eq!(nested_task.poll().unwrap(), Async::NotReady);

                let num = num_cell.num;
                Ok((num_cell, num))
            });

        assert_eq!(runtime.block_on(task).unwrap(), 1);
    }

    #[test]
    fn error() {
        let mut runtime = Runtime::new().unwrap();

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

        assert!(runtime.block_on(task1).is_err());

        assert_eq!(runtime.block_on(task2).unwrap(), 1);

        assert!(runtime.block_on(task3).is_err());
        assert!(runtime.block_on(task4).is_err());
    }

    #[test]
    fn deadlock() {
        let mut runtime = Runtime::new().unwrap();

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

        runtime.block_on(task0).unwrap();
        runtime.block_on(task2).unwrap();
        runtime.block_on(task1).unwrap();
    }

    #[test]
    fn borrow_simple() {
        let mut runtime = Runtime::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task1 = async_mutex.acquire_borrow(|num_cell| -> Result<_, ()> {
            num_cell.num += 1;
            Ok(())
        });

        runtime.block_on(task1).unwrap();

        {
            let _ = async_mutex.acquire_borrow(|num_cell| -> Result<_, ()> {
                num_cell.num += 1;
                Ok(())
            });

            let _ = async_mutex.acquire_borrow(|num_cell| -> Result<_, ()> {
                num_cell.num += 1;
                Ok(())
            });
        }

        let task2 = async_mutex.acquire_borrow(|num_cell| -> Result<_, ()> {
            num_cell.num += 1;
            let num = num_cell.num;
            Ok(num)
        });

        assert_eq!(runtime.block_on(task2).unwrap(), 2);
    }

    #[test]
    fn borrow_multiple() {
        const N: usize = 1_000;

        let mut runtime = Runtime::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        for _ in 0..N {
            let task = async_mutex.acquire_borrow(move |num_cell| -> Result<_, ()> {
                num_cell.num += 1;
                Ok(())
            });
            runtime.spawn(task.map_err(|_| ()));
        }

        runtime.run().unwrap();

        let task = async_mutex.acquire_borrow(|num_cell| -> Result<_, ()> {
            num_cell.num += 1;
            let num = num_cell.num;
            Ok(num)
        });

        assert_eq!(runtime.block_on(task).unwrap(), N + 1);
    }

    #[test]
    fn borrow_nested() {
        let mut runtime = Runtime::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task = async_mutex
            .clone()
            .acquire_borrow(move |num_cell| -> Result<_, ()> {
                num_cell.num += 1;

                let mut nested_task = async_mutex.acquire_borrow(|num_cell| -> Result<_, ()> {
                    assert_eq!(num_cell.num, 1);
                    num_cell.num += 1;
                    Ok(())
                });
                assert_eq!(nested_task.poll().unwrap(), Async::NotReady);

                let num = num_cell.num;
                Ok(num)
            });

        assert_eq!(runtime.block_on(task).unwrap(), 1);
    }

    #[test]
    fn borrow_error() {
        let mut runtime = Runtime::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task1 = async_mutex.acquire_borrow(|_| -> Result<(), _> { Err(()) });

        assert!(runtime.block_on(task1).is_err());

        let task2 = async_mutex.acquire_borrow(|num_cell| -> Result<_, ()> {
            num_cell.num += 1;
            let num = num_cell.num;
            Ok(num)
        });

        assert_eq!(runtime.block_on(task2).unwrap(), 1);
    }

    #[test]
    fn mixed() {
        let mut runtime = Runtime::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task1 = async_mutex.acquire(|mut num_cell| -> Result<_, (_, ())> {
            num_cell.num += 1;
            Ok((num_cell, ()))
        });

        let task2 = async_mutex.acquire_borrow(|num_cell| -> Result<_, ()> {
            num_cell.num += 1;
            Ok(())
        });

        let task3 = async_mutex.acquire(move |mut num_cell| -> Result<_, (_, ())> {
            num_cell.num += 1;
            Ok((num_cell, ()))
        });

        runtime.block_on((task1, task2, task3).into_future()).unwrap();

        let task = async_mutex.acquire_borrow(|num_cell| -> Result<_, ()> { Ok(num_cell.num) });

        assert_eq!(runtime.block_on(task).unwrap(), 3);
    }
}
