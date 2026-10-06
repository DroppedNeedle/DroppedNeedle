//! What each scan failure code means to a person, and what to do about it.
//!
//! Every path a scan skips or fails, and every run that ends early, carries
//! a stable code from [`super::models::failure_codes`] (plus the
//! `WALK_<errno>` family the walk derives from the operating system). This
//! maps each code to one plain sentence and one action, so the failures
//! view never shows raw error text or a reason without a way forward.

use super::models::failure_codes as codes;

/// A failure code with its plain sentence and the action to take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReason {
    /// The stable machine code, as recorded.
    pub code: String,
    /// What happened, in words a user understands.
    pub message: &'static str,
    /// What to do about it.
    pub action: &'static str,
}

const SCAN_AGAIN_OR_REPORT: &str =
    "Scan again. If it keeps happening, report it with the path shown here.";
const CHECK_MOUNT: &str = "Check that the drive or network share is mounted and the path in \
     Settings > Library is right, then scan again.";
const CHECK_PERMISSIONS: &str = "Give the account DroppedNeedle runs as read access to the folder \
     (with Docker, check the volume mount and PUID/PGID), then scan again.";

/// Sentence and action for one code.
fn text(code: &str) -> (&'static str, &'static str) {
    match code {
        codes::ROOT_UNAVAILABLE => (
            "This library folder could not be reached, so it was skipped.",
            CHECK_MOUNT,
        ),
        codes::ROOT_PERMISSION_DENIED | "WALK_EACCES" | "WALK_EPERM" => (
            "DroppedNeedle is not allowed to read this folder.",
            CHECK_PERMISSIONS,
        ),
        codes::WALK_TIMEOUT => (
            "The folder stopped responding while it was being read, so the scan gave up on it.",
            "Check that the drive or network share is healthy and reachable, then scan again.",
        ),
        codes::WALKER_UNAVAILABLE => (
            "The scan could not start reading this folder because the server had no capacity \
             left.",
            "Wait for other heavy work to finish, then scan again.",
        ),
        codes::PROBE_UNAVAILABLE => (
            "The folder could not be checked before the scan because the server was busy.",
            "Scan again in a few minutes.",
        ),
        codes::WALK_SUPERSEDED => (
            "A newer scan of this folder took over partway through.",
            "Nothing to do: the newer scan covers it.",
        ),
        "WALK_ENOENT" | "WALK_ENOTDIR" => (
            "This folder disappeared while it was being read.",
            "Check that the folder still exists and the share stayed mounted, then scan again.",
        ),
        "WALK_EIO" => (
            "The disk reported a read error for this folder.",
            "Check the health of the drive (or the connection to the share), then scan again.",
        ),
        "WALK_ENAMETOOLONG" => (
            "A path in this folder is too long for the system to open.",
            "Shorten the folder or file names, then scan again.",
        ),
        "WALK_ELOOP" => (
            "This folder holds a link that points back into itself, so reading it stopped.",
            "Remove the looping link, then scan again.",
        ),
        "WALK_ENFILE" | "WALK_EMFILE" => (
            "The server ran out of open file handles while reading this folder.",
            "Raise the open-file limit for the container, or scan again when less is running.",
        ),
        codes::SYMLINK_ESCAPE_OUT => (
            "This is a link to something outside the library folder, so it was skipped.",
            "Move the real file into the library folder, or add its folder as a library root.",
        ),
        codes::NON_REGULAR_FILE => (
            "This is not an ordinary file (for example a device or a pipe), so it was skipped.",
            "Remove it from the library folder if it does not belong there.",
        ),
        codes::WALK_NAME_ENCODING => (
            "This file name is not valid text, so it can't be read safely.",
            "Rename the file using ordinary characters, then scan again.",
        ),
        codes::NFC_TWIN_COLLISION => (
            "Two files have names that differ only in how accented letters are stored, so only \
             one was read.",
            "Rename or remove one of the two files, then scan again.",
        ),
        codes::TAG_READ_DEFERRED => (
            "This file's tags were not read in this scan because the scan ran out of time for it.",
            "Nothing to do: the next scan reads it.",
        ),
        codes::TAG_READ_FAILED => (
            "Can't read this file's tags: the file may be damaged or not really audio.",
            "Re-download or replace the file, then scan again.",
        ),
        codes::MTIME_SKEW => (
            "This file's modified time looks wrong, so it was read again to be safe.",
            "Nothing to do. If it keeps showing up, check the clock on the machine or share that \
             holds the file.",
        ),
        codes::SUPERSEDED_POLICY_CHANGED => (
            "The library settings changed while this scan ran, so it stopped.",
            "Nothing to do: a scan with the new settings follows.",
        ),
        codes::UNEXPECTED_WORKER_FAILURE => (
            "The scan stopped because of a problem inside DroppedNeedle.",
            SCAN_AGAIN_OR_REPORT,
        ),
        codes::MASS_MISSING_GUARD => (
            "Most of the files under this folder vanished at once, so none were marked missing.",
            "Check that the drive or share is mounted, then scan again. If you deleted the files \
             on purpose, remove those albums from the library instead.",
        ),
        codes::CATALOG_COMMIT_FAILED => (
            "This file could not be added to the library catalog in this scan.",
            SCAN_AGAIN_OR_REPORT,
        ),
        _ if code.starts_with("WALK_") => (
            "This folder could not be read completely.",
            "Check that the folder is readable and the disk is healthy, then scan again.",
        ),
        _ => (
            "The scan could not finish with this path.",
            SCAN_AGAIN_OR_REPORT,
        ),
    }
}

/// The reason for one recorded failure or terminal code.
pub fn scan_reason(code: &str) -> ScanReason {
    let (message, action) = text(code);
    ScanReason {
        code: code.to_owned(),
        message,
        action,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_recorded_code_has_its_own_reason() {
        let fallback = text("NOT_A_CODE");
        for code in [
            codes::ROOT_UNAVAILABLE,
            codes::ROOT_PERMISSION_DENIED,
            codes::WALK_TIMEOUT,
            codes::WALKER_UNAVAILABLE,
            codes::PROBE_UNAVAILABLE,
            codes::WALK_SUPERSEDED,
            codes::WALK_ERROR,
            codes::SYMLINK_ESCAPE_OUT,
            codes::NON_REGULAR_FILE,
            codes::WALK_NAME_ENCODING,
            codes::NFC_TWIN_COLLISION,
            codes::TAG_READ_DEFERRED,
            codes::TAG_READ_FAILED,
            codes::MTIME_SKEW,
            codes::SUPERSEDED_POLICY_CHANGED,
            codes::UNEXPECTED_WORKER_FAILURE,
            codes::MASS_MISSING_GUARD,
            codes::CATALOG_COMMIT_FAILED,
        ] {
            assert_ne!(text(code), fallback, "{code} falls back");
        }
    }
}
