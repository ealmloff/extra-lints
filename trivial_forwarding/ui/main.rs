// === Pattern 1: Trivial wrapper modules ===

mod reexport_only {
    pub use std::collections::HashMap;
}

mod multi_items {
    pub use std::collections::HashMap;
    pub use std::collections::HashSet;
}

mod with_fn {
    pub use std::collections::HashMap;
    pub fn helper() -> i32 {
        42
    }
}

mod empty_mod {}

mod logic_only {
    pub fn compute() -> i32 {
        42
    }
}

mod private_reexport {
    use std::collections::HashMap;
    // private use — not a pub re-export
    pub fn make_map() -> HashMap<(), ()> {
        let mut m = HashMap::new();
        m.insert((), ());
        m
    }
}

mod glob_reexport {
    pub use std::collections::*;
}

// === Pattern 2: Trivial forwarding functions ===

fn target_add(x: i32, y: i32) -> i32 {
    x + y
}
fn target_greet(name: &str) -> String {
    format!("Hello, {name}")
}
fn target_noop() {}

// SHOULD WARN
fn forwarding_add(x: i32, y: i32) -> i32 {
    target_add(x, y)
}

// SHOULD WARN
fn forwarding_greet(name: &str) -> String {
    target_greet(name)
}

// SHOULD WARN
fn forwarding_noop() {
    target_noop()
}

// SHOULD NOT WARN: arguments reordered
fn reordered(x: i32, y: i32) -> i32 {
    target_add(y, x)
}

// SHOULD NOT WARN: extra computation on result
fn extra_work(x: i32, y: i32) -> i32 {
    target_add(x, y) + 1
}

// SHOULD NOT WARN: body has statements before the call
fn with_side_effect(x: i32, y: i32) -> i32 {
    println!("calling");
    target_add(x, y)
}

// SHOULD NOT WARN: partial forwarding
fn partial(_x: i32, _y: i32) -> String {
    target_greet("constant")
}

// === Pattern 2b: Method forwarding ===

struct Inner {
    value: i32,
}

impl Inner {
    fn get(&self) -> i32 {
        self.value
    }
    fn add(&self, x: i32) -> i32 {
        self.value + x
    }
}

struct Outer {
    inner: Inner,
}

impl Outer {
    // SHOULD WARN: trivial delegation to field method
    fn get(&self) -> i32 {
        self.inner.get()
    }

    // SHOULD WARN: trivial delegation with args
    fn add(&self, x: i32) -> i32 {
        self.inner.add(x)
    }

    // SHOULD NOT WARN: transforms the result
    fn get_doubled(&self) -> i32 {
        self.inner.get() * 2
    }

    // SHOULD NOT WARN: `builder` methods are exempt
    fn builder(&self) -> i32 {
        self.inner.get()
    }

    // SHOULD NOT WARN: `new` methods are exempt
    fn new(&self) -> i32 {
        self.inner.get()
    }
}

struct MultiOuter {
    inner: Inner,
    label: &'static str,
}

impl MultiOuter {
    // SHOULD NOT WARN: multi-field wrappers may intentionally delegate to one field
    fn get(&self) -> i32 {
        self.inner.get()
    }

    // SHOULD NOT WARN: multi-field wrappers may forward through a helper function
    fn add_via_helper(&self, x: i32) -> i32 {
        multi_outer_add_via_helper(self, x)
    }
}

fn multi_outer_add_via_helper(this: &MultiOuter, x: i32) -> i32 {
    this.inner.add(x) + this.label.len() as i32
}

trait ReadValue {
    fn read(&self) -> i32;
}

impl ReadValue for Outer {
    // SHOULD NOT WARN: trait impls may need a forwarding body
    fn read(&self) -> i32 {
        self.inner.get()
    }
}

// === Unsafe edge case ===

extern "C" {
    fn dangerous_extern(x: i32) -> i32;
}

// SHOULD NOT WARN: the unsafe block adds safety value
fn safe_wrapper(x: i32) -> i32 {
    unsafe { dangerous_extern(x) }
}

fn main() {}
