use std::{thread, time::Duration};

fn main() {
    println!("Mercs and Mines v2: Engine Online. Ready for commands.");
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}
