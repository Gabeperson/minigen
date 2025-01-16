use minigen::*;
#[tokio::main]
async fn main() {
    tokio::task::spawn(func());
    std::thread::sleep(std::time::Duration::from_secs(10000));
}

async fn func() {
    let mut generator = generator(|mut yielder| async move {
        for i in 0..100 {
            yielder.yield_value(i).await;
        }
    });
    loop {
        match generator.resume_async().await {
            GeneratorStatus::Yielded(v) => println!("Yielded: {v:?}"),
            GeneratorStatus::Returned(v2) => println!("Returned: {v2:?}"),
            GeneratorStatus::Completed => {
                println!("completed");
                break;
            }
        }
    }
}
// use minigen::*;
// fn main() {
//     let count = 100_000_000i32;

//     let mut v1 = Vec::with_capacity(count as usize);
//     let mut v2 = Vec::with_capacity(count as usize);
//     println!("Starting...");

//     let now = std::time::Instant::now();
//     for i in It(std::hint::black_box(count), std::hint::black_box(0)) {
//         v2.push(std::hint::black_box(i));
//     }
//     println!("Elapsed: {:?}", now.elapsed());

//     let now = std::time::Instant::now();
//     let generator = iterator(|mut yielder| async move {
//         for i in 0..std::hint::black_box(count) {
//             yielder.yield_value(std::hint::black_box(i)).await;
//         }
//     });
//     for v in generator {
//         v1.push(std::hint::black_box(v));
//     }
//     println!("Elapsed: {:?}", now.elapsed());
//     assert_eq!(v1.len(), v2.len());
//     std::hint::black_box(v1);
//     std::hint::black_box(v2);
// }

// struct It(i32, i32);
// impl Iterator for It {
//     type Item = i32;
//     #[inline(never)]
//     fn next(&mut self) -> Option<Self::Item> {
//         if self.1 < self.0 {
//             self.1 += 1;
//             return Some(self.1 - 1);
//         }
//         None
//     }
// }
