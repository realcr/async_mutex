use std::cell::RefCell;
use std::marker;
use std::rc::Rc;

use futures::prelude::*;
use futures::sync::oneshot;

use std::collections::LinkedList;

#[derive(Debug, Default)]
struct Awakener {
    queue: LinkedList<oneshot::Sender<()>>,
}

impl Awakener {
    fn wakeup_next(&mut self) {}
}

#[derive(Debug)]
struct Inner<T> {
    resource: T,
    awakeners: Awakener,
}

#[derive(Debug)]
pub struct AsyncMutex<T> {
    inner: Rc<RefCell<Inner<T>>>,
}

impl<T> Clone for AsyncMutex<T> {
    fn clone(&self) -> AsyncMutex<T> {
        AsyncMutex {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl<T> AsyncMutex<T> {
    pub fn new(resource: T) -> AsyncMutex<T> {
        let inner = Rc::new(RefCell::new(Inner {
            resource: resource,
            awakeners: Default::default(),
        }));

        AsyncMutex { inner }
    }

    pub fn acquire<F, B, O, E>(&self, _f: F) -> AcquireFuture<T, F>
    where
        F: FnOnce(&mut T) -> B,
        B: IntoFuture<Item = O, Error = E>,
    {
        unimplemented!()
    }
}

#[derive(Debug)]
pub struct AcquireFuture<T, F> {
    data: marker::PhantomData<(T, F)>,
}

#[derive(Debug)]
pub enum AsyncMutexError<E> {
    Other(E),
}

impl<T, F, B, O, E> Future for AcquireFuture<T, F>
where
    F: FnOnce(&mut T) -> B,
    B: IntoFuture<Item = O, Error = E>,
{
    type Item = O;
    type Error = AsyncMutexError<E>;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        unimplemented!()
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
    #[ignore]
    fn simple() {
        let mut core = Core::new().unwrap();
        let handle = core.handle();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task1 = async_mutex.acquire(|num_cell| -> Result<_, ()> {
            num_cell.num += 1;
            Ok(())
        });

        handle.spawn(task1.map_err(|_| ()));

        {
            let _ = async_mutex.acquire(|num_cell| -> Result<_, ()> {
                num_cell.num += 1;
                Ok(())
            });

            let _ = async_mutex.acquire(|num_cell| -> Result<_, ()> {
                num_cell.num += 1;
                Ok(())
            });
        }

        let task2 = async_mutex.acquire(|num_cell| -> Result<_, ()> {
            num_cell.num += 1;
            let num = num_cell.num;
            Ok(num)
        });

        assert_eq!(core.run(task2).unwrap(), 2);
    }

    #[test]
    #[ignore]
    fn multiple() {
        const N: usize = 1_000;

        let mut core = Core::new().unwrap();
        let handle = core.handle();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        for num in 0..N {
            let task = async_mutex.acquire(move |num_cell| -> Result<_, ()> {
                assert_eq!(num_cell.num, num);

                num_cell.num += 1;
                Ok(())
            });

            handle.spawn(task.map_err(|_| ()));
        }

        let task = async_mutex.acquire(|num_cell| -> Result<_, ()> {
            num_cell.num += 1;
            let num = num_cell.num;
            Ok(num)
        });

        assert_eq!(core.run(task).unwrap(), N + 1);
    }

    #[test]
    #[ignore]
    fn nested() {
        let mut core = Core::new().unwrap();
        let handle = core.handle();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task = async_mutex
            .clone()
            .acquire(move |num_cell| -> Result<_, ()> {
                num_cell.num += 1;

                let nested_task = async_mutex.acquire(|num_cell| -> Result<_, ()> {
                    assert_eq!(num_cell.num, 1);
                    num_cell.num += 1;
                    Ok(())
                });
                handle.spawn(nested_task.map_err(|_| ()));

                let num = num_cell.num;
                Ok(num)
            });

        assert_eq!(core.run(task).unwrap(), 1);
    }

    #[test]
    #[ignore]
    fn error() {
        let mut core = Core::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task1 = async_mutex.acquire(|_| -> Result<(), _> { Err(()) });

        assert!(core.run(task1).is_err());

        let task2 = async_mutex.acquire(|num_cell| -> Result<_, ()> {
            num_cell.num += 1;
            let num = num_cell.num;
            Ok(num)
        });

        assert_eq!(core.run(task2).unwrap(), 1);
    }
}
