// Whether an edition sheet is open. A choice can move the album to another
// release group; the album page waits for the sheet to close before it
// follows the album there, so the person can see what happened first.
export const editionSheet = $state({ open: false });
