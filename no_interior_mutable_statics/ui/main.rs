use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex, RwLock};

// Should warn: direct interior-mutable statics
static MUTEX: Mutex<i32> = Mutex::new(0);
static RWLOCK: RwLock<String> = RwLock::new(String::new());
static CELL: Cell<bool> = Cell::new(false);
static REFCELL: RefCell<Vec<u8>> = RefCell::new(Vec::new());

// Should warn: nested interior-mutable statics
static NESTED: Arc<Mutex<i32>> = Arc::new(Mutex::new(0));

// Should NOT warn: no interior mutability
static PLAIN: i32 = 42;
static PLAIN_STR: &str = "hello";

fn main() {}
