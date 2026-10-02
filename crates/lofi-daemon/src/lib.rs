pub mod mpv;
pub mod server;
pub mod state;

// One lock for every test in this crate that mutates process-wide env vars.
// Tests run on parallel threads in one process, and a per-module lock would
// not serialize against env mutations made by another module's tests.
#[cfg(test)]
pub(crate) static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
