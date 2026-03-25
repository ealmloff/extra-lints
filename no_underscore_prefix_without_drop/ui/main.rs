// edition:2024

use std::sync::MutexGuard;

// === Struct fields ===

// Should warn: _count is u32, no Drop impl
struct Foo {
    _count: u32,
}

// Should NOT warn: _guard has a significant Drop (lock release)
struct Bar<'a> {
    _guard: MutexGuard<'a, i32>,
}

// Should warn: _flag is bool, no Drop impl
struct Baz {
    _flag: bool,
}

// Should warn: _data is Vec — its Drop is just deallocation
struct Qux {
    _data: Vec<u8>,
}

// Should NOT warn: no underscore prefix
struct Plain {
    count: u32,
}

// === Function args ===

// Should warn: _x is i32, no Drop impl
fn takes_int(_x: i32) {}

// Should warn: _v is Vec — Drop is just dealloc, not significant
fn takes_vec(_v: Vec<String>) {}

// Should NOT warn: no underscore prefix
fn normal_arg(x: i32) {
    let _ = x;
}

// Should warn: _flag is bool, no Drop impl
fn takes_bool(_flag: bool) {}

// Should NOT warn: bare _ is a wildcard, not a named binding
fn wildcard(_: i32) {}

// Should warn: _s is String — Drop is just dealloc
fn takes_string(_s: String) {}

// Should warn once for the explicit _x parameter, but should NOT warn on the
// compiler-generated async plumbing parameter.
async fn takes_async_int(_x: i32) {}

// === Trait method args ===

trait MyTrait {
    // Should warn: _val is u64, no Drop impl
    fn do_thing(&self, _val: u64) {
        // default impl
    }

    // Should warn: _buf is Vec — Drop is just dealloc
    fn with_buf(&self, _buf: Vec<u8>) {
        // default impl
    }

    // Should NOT warn: _guard has a significant Drop
    fn with_guard(&self, _guard: MutexGuard<'_, i32>) {
        // default impl
    }

    // No body — nothing to check
    fn no_body(&self, _x: i32);
}

fn main() {
    std::mem::drop(takes_async_int(1));
}
