# globalize

Globalize makes global mutable variables practical by injecting them as static
mutable references into main:

```rust
use globalize::globals;

#[globals(
    s1 = String::new(),
    s2 = String::new(),
)]
fn main(s1: &'static mut String, s2: &'static mut String) {
    *s1 = "Hello".to_owned();
    *s2 = "World".to_owned();

    // Because these references are static, they can be shared with other
    // threads
    let t1 = std::thread::spawn(|| {
        s1.len() + s2.len()
    });

    let t2 = std::thread::spawn(|| {
        s1.len() * s2.len()
    });

    let out1 = t1.join().unwrap();
    let out2 = t2.join().unwrap();

    assert_eq!(out1 + out2, 35);
}
```

The disadvantage, of course, is that the references are only available to main
and must be passed as arguments or by capture to other functions. However, being
static references, they are much easier to pass to other threads or async tasks,
and can allow you to avoid polluting type signatures with lifetime annotations.

# Safety

`main`, as it turns out, is re-entrant (it's possible to call `main()` in your
program). `globalize` therefore inserts an extra check that panics if `main` is
called more than once. For absolute peak performance, can avoid this check by
adding `unsafe: nonreentrant;` to the attribute.

```rust
use globalize::globals;

#[globals(
    unsafe: nonreentrant;
    foo = String::new(),
)]
fn main(foo: &'static mut String) {}
```
