use std::cell::RefCell;
use std::rc::Rc;

use futures::future;
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

    fn wakeup_next(&mut self, mut resource: T) -> Option<T> {
        while let Some(sender) = self.queue.pop_front() {
            resource = match sender.send(resource) {
                Ok(_) => return None,
                Err(resource) => resource,
            }
        }
        Some(resource)
    }

    fn add_awakener(&mut self) -> oneshot::Receiver<T> {
        let (sender, receiver) = oneshot::channel();
        self.queue.push_back(sender);
        receiver
    }
}

#[derive(Debug)]
enum Inner<T> {
    Empty,
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

    pub fn acquire_borrow<F, B, O, E>(&self, f: F) -> impl Future<Item = O, Error = AsyncMutexError>
    where
        F: FnOnce(&mut T) -> B,
        B: IntoFuture<Item = O, Error = E>,
    {
        WaitPoll {
            inner: Rc::clone(&self.inner),
            f: Some(f),
        }.and_then(|(inner, f, receiver)| {
            (future::ok(inner), future::ok(f), receiver)
                .into_future()
                .map_err(|_| AsyncMutexError::AwakenerCanceled)
        }).and_then(|(inner, f, mut t)| {
            let result = f(&mut t);
            let state = inner.replace(Inner::Empty);
            if let Inner::Pending(mut awakener) = state {
                let next_state = match awakener.wakeup_next(t) {
                    Some(t) => Inner::Present(t),
                    None => Inner::Pending(awakener),
                };
                inner.replace(next_state);
            } else {
                unreachable!()
            }

            result.into_future().map_err(|_| AsyncMutexError::Other)
        })
    }
}

#[derive(Debug)]
struct WaitPoll<T, F> {
    inner: Rc<RefCell<Inner<T>>>,
    f: Option<F>,
}

impl<T, F> Future for WaitPoll<T, F> {
    type Item = (Rc<RefCell<Inner<T>>>, F, oneshot::Receiver<T>);
    type Error = AsyncMutexError;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let inner = self.inner.replace(Inner::Empty);

        let receiver = match inner {
            Inner::Pending(mut awakener) => {
                let receiver = awakener.add_awakener();
                self.inner.replace(Inner::Pending(awakener));
                receiver
            }
            Inner::Present(t) => {
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
            Inner::Empty => unreachable!(),
        };

        Ok(Async::Ready((
            Rc::clone(&self.inner),
            self.f.take().unwrap(),
            receiver,
        )))
    }
}

impl<T> Clone for AsyncMutex<T> {
    fn clone(&self) -> AsyncMutex<T> {
        AsyncMutex {
            inner: Rc::clone(&self.inner),
        }
    }
}

#[derive(Debug)]
pub enum AsyncMutexError {
    AwakenerCanceled,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_core::reactor::Core;

    struct NumCell {
        num: usize,
    }

    #[test]
    fn borrow_simple() {
        let mut core = Core::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        let task1 = async_mutex.acquire_borrow(|num_cell| -> Result<_, ()> {
            num_cell.num += 1;
            Ok(())
        });

        core.run(task1).unwrap();

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

        assert_eq!(core.run(task2).unwrap(), 2);
    }

    #[test]
    fn borrow_multiple() {
        const N: usize = 1_000;

        let mut core = Core::new().unwrap();

        let async_mutex = AsyncMutex::new(NumCell { num: 0 });

        for num in 0..N {
            let task = async_mutex.acquire_borrow(move |num_cell| -> Result<_, ()> {
                assert_eq!(num_cell.num, num);

                num_cell.num += 1;
                Ok(())
            });

            core.run(task).unwrap()
        }

        let task = async_mutex.acquire_borrow(|num_cell| -> Result<_, ()> {
            num_cell.num += 1;
            let num = num_cell.num;
            Ok(num)
        });

        assert_eq!(core.run(task).unwrap(), N + 1);
    }

    #[test]
    fn borrow_nested() {
        let mut core = Core::new().unwrap();

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

        assert_eq!(core.run(task).unwrap(), 1);
    }
}
