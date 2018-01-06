extern crate divans;
#[cfg(feature="no-stdlib")]
fn main() {
    panic!("For no-stdlib examples please see the tests")
}
#[cfg(not(feature="no-stdlib"))]
fn main() {
    use std::io;
    let stdin = &mut io::stdin();
    {
        use std::io::{Read, Write};
        let mut reader = divans::DivansDecompressorReader::new(
            stdin,
            4096 /* buffer size */);
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf[..]) {
                Err(e) => {
                    if let io::ErrorKind::Interrupted = e.kind() {
                        continue;
                    }
                    panic!(e);
                }
                Ok(size) => {
                    if size == 0 {
                        break;
                    }
                    match io::stdout().write_all(&buf[..size]) {
                        Err(e) => panic!(e),
                        Ok(_) => {},
                    }
                }
            }
        }
    }   
}