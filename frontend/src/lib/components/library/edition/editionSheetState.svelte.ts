// Whether an edition sheet is open. A choice can move the album to another
// release group; the album page waits for the sheet to close before it
// follows the album there, so the person can see what happened first.
export const editionSheet = $state({ open: false });

/** The edition a request gets, for an album the library does not hold. */
export interface EditionRequestPick {
	/** The edition the request gets: the person's pick, the live request's, else the catalog's. */
	release: string | null;
	/** A request for the album is live. */
	requested: boolean;
	/** Its download has started, so the edition can no longer change. */
	locked: boolean;
	/** The album's title, for the sheet's heading. */
	albumTitle: string;
	/** Make `release` the edition to get. Rejects with a sentence to show. */
	pick: (release: string) => Promise<void>;
}
