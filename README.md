# AsyncMutex

AsyncMutex is an asynchronous Mutex, used for synchronization in the context of
Futures. A usual Mutex (from std) is not a good choice for synchronization
between Futures, because it may block your future, and it will not be able to
yield execution to another future.

AsyncMutex yields execution to a different future if the resource is not ready.
When the resource is ready, the future waiting for the resource will be woken
up.

Example:

```rust
#![feature(futures_api, pin, async_await, await_macro, arbitrary_self_types)]
#![feature(generators)]
use async_mutex::AsyncMutex;
use std::sync::Arc;
use futures::executor::ThreadPool;
use futures::task::SpawnExt;
use futures::{future, SinkExt, StreamExt};
use futures::channel::mpsc;

struct NumCell {
    x: u32,
}

impl NumCell {
    async fn add(&mut self) {
        await!(future::lazy(|_| {
            self.x += 1
        }));
    }
}

async fn add_task(arc_mut_num_cell: Arc<AsyncMutex<NumCell>>, mut sender: mpsc::Sender<()>) {
    let mut guard = await!(arc_mut_num_cell.lock());
    await!(guard.add());
    await!(sender.send(())).unwrap();
}

fn main() {
    let (sender, mut receiver) = mpsc::channel::<()>(0);
    let arc_mut_num_cell = Arc::new(AsyncMutex::new(NumCell {x: 0}));
    let mut thread_pool = ThreadPool::new().unwrap();
    for _ in 0 .. 100usize {
        thread_pool.spawn(add_task(arc_mut_num_cell.clone(), sender.clone()));
    }
    thread_pool.run(async move {
        for _ in 0 .. 100usize {
            await!(receiver.next()).unwrap();
        }
        let mut guard = await!(arc_mut_num_cell.lock());
        assert_eq!(guard.x, 100);
    });
}
```

