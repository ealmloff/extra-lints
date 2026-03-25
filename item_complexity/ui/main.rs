// Should NOT warn: small function
fn small() -> i32 {
    let x = 1;
    let y = 2;
    x + y
}

// Should NOT warn: moderate function (under 500 nodes)
fn moderate() {
    let a = 1;
    let b = 2;
    let c = 3;
    let d = 4;
    let e = 5;
    let _ = if a > b { c + d } else { d + e };
    let _ = if b > c { a + e } else { c + d };
    let _ = match a {
        1 => b + c,
        2 => d + e,
        _ => 0,
    };
}

// Should warn: function with excessive HIR nodes (>500)
fn very_complex() {
    let a = 1; let b = 2; let c = 3; let d = 4; let e = 5;
    let f = 6; let g = 7; let h = 8; let i = 9; let j = 10;

    let _ = if a > b {
        if c > d { if e > f { a + b + c + d + e + f } else { g + h + i + j } }
        else { if g > h { a + c + e + g } else { b + d + f + h } }
    } else {
        if i > j { if a > c { a + b + c } else { d + e + f } }
        else { if b > d { g + h + i } else { j + a + b } }
    };

    let _ = match a {
        1 => b + c + d, 2 => e + f + g, 3 => h + i + j,
        4 => a + b + c + d + e, 5 => f + g + h + i + j, _ => 0,
    };
    let _ = match b {
        1 => c + d + e, 2 => f + g + h, 3 => i + j + a, _ => 0,
    };
    let _ = match c {
        1 => a + b + c, 2 => d + e + f, 3 => g + h + i, _ => j,
    };
    let _ = match d {
        1 => a + b, 2 => c + d, 3 => e + f, 4 => g + h, _ => i + j,
    };

    let v: Vec<i32> = (0..10).collect();
    let _s1: i32 = v.iter().map(|x| x + 1).filter(|x| *x > 5).sum();
    let _s2: i32 = v.iter().map(|x| x * 2).filter(|x| *x < 10).product();
    let _s3 = v.iter().any(|x| *x == 3);
    let _s4 = v.iter().all(|x| *x >= 0);
    let _s5 = v.iter().filter(|x| **x > 3).count();
    let _s6: i32 = v.iter().map(|x| x + 3).filter(|x| *x > 7).sum();
    let _s7: i32 = v.iter().map(|x| x * 3).filter(|x| *x < 20).product();
    let _s8 = v.iter().any(|x| *x == 7);
    let _s9 = v.iter().all(|x| *x < 100);
    let _s10 = v.iter().filter(|x| **x > 1).count();

    let _ = (a + b) * (c + d) + (e + f) * (g + h) + (i + j) * (a + b);
    let _ = (c + d) * (e + f) + (g + h) * (i + j) + (a + b) * (c + d);
    let _ = (e + f) * (g + h) + (i + j) * (a + b) + (c + d) * (e + f);
    let _ = (a + c) * (b + d) + (e + g) * (f + h) + (i + a) * (j + b);
    let _ = (c + e) * (d + f) + (g + i) * (h + j) + (a + c) * (b + d);
    let _ = (a + b) * (c + d) + (e + f) * (g + h) + (i + j) * (a + b);

    let _ = if a > 0 {
        if b > 0 { if c > 0 { a + b + c } else { d + e + f } }
        else { if d > 0 { g + h + i } else { j + a + b } }
    } else {
        if e > 0 { if f > 0 { c + d + e } else { f + g + h } }
        else { if g > 0 { i + j + a } else { b + c + d } }
    };

    let _ = if a > 1 {
        if b > 1 { if c > 1 { a + b + c } else { d + e + f } }
        else { if d > 1 { g + h + i } else { j + a + b } }
    } else {
        if e > 1 { if f > 1 { c + d + e } else { f + g + h } }
        else { if g > 1 { i + j + a } else { b + c + d } }
    };

    let _ = match (a, b) {
        (1, 1) => c + d, (1, 2) => e + f, (2, 1) => g + h,
        (2, 2) => i + j, (3, _) => a + b + c, _ => 0,
    };
    let _ = match (c, d) {
        (1, 1) => a + b, (1, 2) => c + d, (2, 1) => e + f,
        (2, 2) => g + h, (3, _) => i + j + a, _ => 0,
    };
}

fn main() {}
