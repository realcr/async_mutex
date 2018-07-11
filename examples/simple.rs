extern crate async_mutex;
extern crate tokio;

use async_mutex::AsyncMutex;
use tokio::prelude::*;
use tokio::runtime::current_thread::Runtime;

fn main() {
    let mutex = AsyncMutex::new(0u32);

    let mut runtime = Runtime::new().unwrap();

    let task = mutex.acquire(|mut num| -> Result<_, (_, ())> {
        num += 1;
        Ok((num, ()))
    });
    runtime.spawn(task.map_err(|_| ()));

    for _ in 0..1000 {
        let task = mutex.acquire_borrow(|num| -> Result<_, ()> {
            *num += 1;
            Ok(())
        });
        runtime.spawn(task.map_err(|_| ()));
    }

    runtime.run().unwrap();

    let task = mutex.acquire_borrow(|num| -> Result<_, ()> { Ok(*num) });

    assert_eq!(runtime.block_on(task), Ok(1001));
}
