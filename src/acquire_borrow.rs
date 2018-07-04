use std::marker;

use futures::prelude::*;

struct Inner<T> {
    data: marker::PhantomData<T>,
}

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

pub struct AcquireFuture<T, F> {
    data: marker::PhantomData<(T, F)>,
}

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
