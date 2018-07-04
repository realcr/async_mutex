use std::marker;

use futures::prelude::*;

#[derive(Debug)]
struct Inner<T> {
    data: marker::PhantomData<T>,
}

#[derive(Debug)]
pub struct AsyncMutex<T> {
    data: marker::PhantomData<Inner<T>>,
}

impl<T> AsyncMutex<T> {
    pub fn new(_resource: T) -> AsyncMutex<T> {
        unimplemented!()
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
    fn simple() {
        let mut core = Core::new().unwrap();
        let handle = core.handle();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task1 = async_mutex.acquire(|ref mut num_cell| -> Result<(), ()> {
            num_cell.num += 1;
            Ok(())
        });

        handle.spawn(task1.map_err(|_| ()));

        {
            let _ = async_mutex.acquire(|ref mut num_cell| -> Result<(), ()> {
                num_cell.num += 1;
                Ok(())
            });

            let _ = async_mutex.acquire(|ref mut num_cell| -> Result<(), ()> {
                num_cell.num += 1;
                Ok(())
            });
        }

        let task2 = async_mutex.acquire(|ref mut num_cell| -> Result<_, ()> {
            num_cell.num += 1;
            let num = num_cell.num;
            Ok(num)
        });

        assert_eq!(core.run(task2).unwrap(), 2);
    }

    #[test]
    fn multiple() {
        const N: usize = 1_000;

        let mut core = Core::new().unwrap();
        let handle = core.handle();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        for num in 0..N {
            let task = async_mutex.acquire(move |num_cell| -> Result<(), ()> {
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

}
