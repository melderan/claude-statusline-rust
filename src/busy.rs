//! Telling "the database is busy or locked" from every other SQLite failure.

/// True when `e` is SQLite's BUSY or LOCKED: another connection or process
/// holds the file. Those clear by themselves or by finding the holder, so the
/// status line says so; any other failure (read-only file, full disk, a
/// corrupt file) keeps to stderr.
pub(crate) fn is_busy(e: &(dyn std::error::Error + 'static)) -> bool {
    matches!(
        e.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(f, _))
            if matches!(
                f.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(code: std::ffi::c_int) -> Box<dyn std::error::Error> {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None).into()
    }

    #[test]
    fn busy_and_locked_are_busy() {
        assert!(is_busy(failure(rusqlite::ffi::SQLITE_BUSY).as_ref()));
        assert!(is_busy(failure(rusqlite::ffi::SQLITE_LOCKED).as_ref()));
        // Extended codes keep the primary code, so they count too.
        assert!(is_busy(
            failure(rusqlite::ffi::SQLITE_BUSY_SNAPSHOT).as_ref()
        ));
    }

    #[test]
    fn other_failures_are_not() {
        assert!(!is_busy(failure(rusqlite::ffi::SQLITE_READONLY).as_ref()));
        assert!(!is_busy(failure(rusqlite::ffi::SQLITE_FULL).as_ref()));
        let plain: Box<dyn std::error::Error> = "HOME unset".into();
        assert!(!is_busy(plain.as_ref()));
    }
}
