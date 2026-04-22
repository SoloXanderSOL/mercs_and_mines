use std::{thread, time::Duration};

fn main() {
    println!("Mercs and Mines Server Starting...");
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}
