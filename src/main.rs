mod app;
mod audio;
mod call_detection;
mod cli;
mod clipboard;
mod mcp;
mod model;
mod store;
mod titles;
mod transcribe;
mod vocabulary;

fn main() {
    // Audio, transcripts, DB and socket are private to the current user.
    unsafe {
        libc::umask(0o077);
    }
    if let Err(error) = cli::run() {
        eprintln!("meetlocal: {error:#}");
        std::process::exit(1);
    }
}
