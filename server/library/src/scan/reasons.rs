//! What each scan failure code means to a person, and what to do about it.
//!
//! Every path a scan skips or fails, and every run that ends early, carries
//! a stable [`FailureCode`](Code). This maps each code to one plain
//! sentence and one action, so the failures view never shows raw error text
//! or a reason without a way forward.

use super::models::FailureCode as Code;

/// A failure code with its plain sentence and the action to take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReason {
    /// The stable machine code, as recorded.
    pub code: Code,
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
fn text(code: Code) -> (&'static str, &'static str) {
    match code {
        Code::RootUnavailable => (
            "This library folder could not be reached, so it was skipped.",
            CHECK_MOUNT,
        ),
        Code::RootPermissionDenied | Code::WalkEacces | Code::WalkEperm => (
            "DroppedNeedle is not allowed to read this folder.",
            CHECK_PERMISSIONS,
        ),
        Code::WalkTimeout => (
            "The folder stopped responding while it was being read, so the scan gave up on it.",
            "Check that the drive or network share is healthy and reachable, then scan again.",
        ),
        Code::WalkerUnavailable => (
            "The scan could not start reading this folder because the server had no capacity \
             left.",
            "Wait for other heavy work to finish, then scan again.",
        ),
        Code::ProbeUnavailable => (
            "The folder could not be checked before the scan because the server was busy.",
            "Scan again in a few minutes.",
        ),
        Code::WalkSuperseded => (
            "A newer scan of this folder took over partway through.",
            "Nothing to do: the newer scan covers it.",
        ),
        Code::WalkEnoent | Code::WalkEnotdir => (
            "This folder disappeared while it was being read.",
            "Check that the folder still exists and the share stayed mounted, then scan again.",
        ),
        Code::WalkEio => (
            "The disk reported a read error for this folder.",
            "Check the health of the drive (or the connection to the share), then scan again.",
        ),
        Code::WalkEnametoolong => (
            "A path in this folder is too long for the system to open.",
            "Shorten the folder or file names, then scan again.",
        ),
        Code::WalkEloop => (
            "This folder holds a link that points back into itself, so reading it stopped.",
            "Remove the looping link, then scan again.",
        ),
        Code::WalkEnfile | Code::WalkEmfile => (
            "The server ran out of open file handles while reading this folder.",
            "Raise the open-file limit for the container, or scan again when less is running.",
        ),
        Code::SymlinkEscapeOut => (
            "This is a link to something outside the library folder, so it was skipped.",
            "Move the real file into the library folder, or add its folder as a library root.",
        ),
        Code::NonRegularFile => (
            "This is not an ordinary file (for example a device or a pipe), so it was skipped.",
            "Remove it from the library folder if it does not belong there.",
        ),
        Code::WalkNameEncoding => (
            "This file name is not valid text, so it can't be read safely.",
            "Rename the file using ordinary characters, then scan again.",
        ),
        Code::NfcTwinCollision => (
            "Two files have names that differ only in how accented letters are stored, so only \
             one was read.",
            "Rename or remove one of the two files, then scan again.",
        ),
        Code::TagReadDeferred => (
            "This file's tags were not read in this scan because the scan ran out of time for it.",
            "Nothing to do: the next scan reads it.",
        ),
        Code::TagReadFailed => (
            "Can't read this file's tags: the file may be damaged or not really audio.",
            "Re-download or replace the file, then scan again.",
        ),
        Code::MtimeSkew => (
            "This file's modified time looks wrong, so it was read again to be safe.",
            "Nothing to do. If it keeps showing up, check the clock on the machine or share that \
             holds the file.",
        ),
        Code::SupersededPolicyChanged => (
            "The library settings changed while this scan ran, so it stopped.",
            "Nothing to do: a scan with the new settings follows.",
        ),
        Code::UnexpectedWorkerFailure => (
            "The scan stopped because of a problem inside DroppedNeedle.",
            SCAN_AGAIN_OR_REPORT,
        ),
        Code::MassMissingGuard => (
            "Most of the files under this folder vanished at once, so none were marked missing.",
            "Check that the drive or share is mounted, then scan again. If you deleted the files \
             on purpose, remove those albums from the library instead. Until a scan finds the \
             folder as expected, DroppedNeedle does not retag or rename files that appeared in \
             it since.",
        ),
        Code::CatalogCommitFailed => (
            "This file could not be added to the library catalog in this scan.",
            SCAN_AGAIN_OR_REPORT,
        ),
        Code::WalkError
        | Code::WalkEisdir
        | Code::WalkEinval
        | Code::WalkEnospc
        | Code::WalkErofs
        | Code::WalkEunknown => (
            "This folder could not be read completely.",
            "Check that the folder is readable and the disk is healthy, then scan again.",
        ),
        Code::RunEndPending => (
            "The scan could not finish with this path.",
            SCAN_AGAIN_OR_REPORT,
        ),
    }
}

/// The reason for one recorded failure or terminal code.
pub fn scan_reason(code: Code) -> ScanReason {
    let (message, action) = text(code);
    ScanReason {
        code,
        message,
        action,
    }
}
