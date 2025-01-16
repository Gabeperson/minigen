use minigen::*;
fn main() {
    let count = 10_000_000i32;

    let generator = iterator(|mut yielder| async move {
        for i in 0..count {
            yielder.yield_value(i).await;
        }
    });
    for i in generator {
        std::hint::black_box(i);
    }

    let mut v1 = Vec::with_capacity(count as usize);
    let mut v2 = Vec::with_capacity(count as usize);
    println!("Starting...");
    let now = std::time::Instant::now();
    let mut generator = iterator(|mut yielder| async move {
        for i in 0..count {
            yielder.yield_value(i).await;
        }
    });
    pollster::block_on(async {
        while let Some(v) = generator.resume().await {
            v1.push(v);
        }
    });
    println!("Elapsed: {:?}", now.elapsed());

    let now = std::time::Instant::now();
    for i in std::hint::black_box(It(count, 0)) {
        v2.push(i);
    }
    std::hint::black_box(v1);
    std::hint::black_box(v2);
    println!("Elapsed: {:?}", now.elapsed());
}

struct It(i32, i32);
impl Iterator for It {
    type Item = i32;
    #[inline(never)]
    fn next(&mut self) -> Option<Self::Item> {
        if self.1 > self.0 {
            self.1 += 1;
            return Some(self.1 - 1);
        }
        None
    }
}
