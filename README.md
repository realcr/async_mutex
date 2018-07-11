# async_mutex

This crate provide an "asynchonous mutex" for
[futures](https://github.com/rust-lang-nursery/futures-rs), intended for single-thread usages.

## Usage

First, construct the mutex using `AsyncMutex::new()`:

``` rust
// A mutex that hold an `u32` value.
let mutex = AsyncMutex::new(u32);
```

Then, get the resource by acquiring it. There are two ways to acquire:
- Borrow the resource
- Take ownership of the resource

### Borrow the resource

`AsyncMutex::acquire_borrow(&self, F)` returns a future that will complete after:
- Waiting for the resource to be available
- Apply the resource to `F`

``` rust
let task = mutex.acquire_borrow(|num| -> Result<(), ()> {
    *num += 1;
    Ok(())
});
```
