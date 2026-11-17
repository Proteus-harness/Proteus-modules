//! Test-only observability of actual record decoding and fingerprint I/O.
#[cfg(test)]
#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct Counters {
    pub decoded: u64,
    pub fingerprint_bytes: u64,
}
#[cfg(test)]
thread_local! { static COUNTERS:std::cell::Cell<Counters>=std::cell::Cell::new(Counters { decoded:0,fingerprint_bytes:0 }); }
#[cfg(test)]
pub(crate) fn reset() {
    COUNTERS.set(Counters::default());
}
#[cfg(test)]
pub(crate) fn get() -> Counters {
    COUNTERS.get()
}
pub(crate) fn decoded() {
    #[cfg(test)]
    COUNTERS.set(Counters {
        decoded: COUNTERS.get().decoded + 1,
        ..COUNTERS.get()
    });
}
pub(crate) fn fingerprint(bytes: usize) {
    #[cfg(test)]
    COUNTERS.set(Counters {
        fingerprint_bytes: COUNTERS.get().fingerprint_bytes + bytes as u64,
        ..COUNTERS.get()
    });
    #[cfg(not(test))]
    let _ = bytes;
}
