#[cfg(windows)]
mod service;
#[cfg(windows)]
fn main() {
    if let Err(e) = service::entry() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!("Windows only");
}
