// edition:2024

use std::collections::HashMap;

// === Pattern 1: match on string with 2+ literal arms ===

// Should warn: match on &str with 2 literal arms
fn match_str(s: &str) -> i32 {
    match s {
        "start" => 1,
        "stop" => 2,
        _ => 0,
    }
}

// Should warn: match on String with 3 literal arms
fn match_string(s: String) -> i32 {
    match s.as_str() {
        "create" => 1,
        "update" => 2,
        "delete" => 3,
        _ => 0,
    }
}

// Should NOT warn: only 1 literal arm
fn match_single_literal(s: &str) -> i32 {
    match s {
        "only" => 1,
        _ => 0,
    }
}

// Should NOT warn: match on integer, not string
fn match_integer(n: i32) -> &'static str {
    match n {
        1 => "one",
        2 => "two",
        _ => "other",
    }
}

// === Pattern 2: if/else-if chain comparing string to 2+ literals ===

// Should warn: 2 string comparisons on same variable
fn if_chain(s: &str) -> i32 {
    if s == "alpha" {
        1
    } else if s == "beta" {
        2
    } else {
        0
    }
}

// Should warn: 3 string comparisons
fn if_chain_three(cmd: &str) -> i32 {
    if cmd == "run" {
        1
    } else if cmd == "compile" {
        2
    } else if cmd == "check" {
        3
    } else {
        0
    }
}

// Should NOT warn: only 1 comparison
fn if_single(s: &str) -> i32 {
    if s == "only_one" {
        1
    } else {
        0
    }
}

// === Pattern 3: HashMap with string-literal keys ===

// Should warn: 2 literal-key inserts on same map
fn hashmap_literal_keys() {
    let mut config: HashMap<&str, i32> = HashMap::new();
    config.insert("timeout", 30);
    config.insert("retries", 3);
}

// Should warn: 3 literal-key inserts
fn hashmap_three_keys() {
    let mut map: HashMap<&str, i32> = HashMap::new();
    map.insert("name", 1);
    map.insert("role", 2);
    map.insert("dept", 3);
}

// Should NOT warn: keys are not string literals (method call)
fn hashmap_string_keys_via_method() {
    let mut map: HashMap<String, String> = HashMap::new();
    map.insert("name".to_string(), "Alice".to_string());
    map.insert("role".to_string(), "Admin".to_string());
}

// Should NOT warn: dynamic keys
fn hashmap_dynamic_keys(keys: &[&str]) {
    let mut map: HashMap<&str, i32> = HashMap::new();
    for (i, key) in keys.iter().enumerate() {
        map.insert(key, i as i32);
    }
}

// Should NOT warn: only 1 literal key
fn hashmap_single_key() {
    let mut map: HashMap<&str, i32> = HashMap::new();
    map.insert("only", 1);
}

fn main() {}
